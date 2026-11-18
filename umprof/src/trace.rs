//! Collects a trace's events into samples with complete stacks.

use crate::etw::{read_trace, Event, TraceInfo, KERNEL_START};
use crate::images::ImageRecord;
use crate::qpc::Qpc;
use anyhow::{Context, Result};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;
use std::time::Duration;

/// A full trace, containing everything we're interested in.
pub struct RawTrace {
    pub info: TraceInfo,
    pub images: Vec<ImageRecord>,
    pub samples: Vec<RawSample>,
    pub presents: Vec<RawPresent>,
    pub thread_names: FxHashMap<u32, String>,
    pub sample_interval: Duration,
}

#[derive(Debug)]
pub struct RawSample {
    pub pid: u32,
    pub tid: u32,
    pub qpc: Qpc,
    pub cpu: u16,
    /// Stack frames, ordered top -> bottom
    pub frames: Vec<u64>,
    /// True if this is a kernel sample for which no corresponding user stack could be found.
    /// This can happen sometimes.
    pub user_stack_lost: bool,
}

/// A DXGI present event
#[derive(Debug, PartialEq, Eq)]
pub struct RawPresent {
    pub pid: u32,
    pub tid: u32,
    pub start_qpc: Qpc,
    pub end_qpc: Qpc,
}

/// Turns the stream of events into a collection of samples. Threads, images,
/// stacks, and other events are aggregated into their own collections.
///
/// A particular quirk about the kernel requires some additional processing.
/// When the timer interrupts a thread in kernel mode, the kernel frames are
/// logged immediately, but the user part of the stack is not reported until
/// it returns to user mode. At that point, it gets reported as a separate
/// event.
///
/// To properly attribute these kernel stacks, we keep track of them per thread,
/// until the next user mode sample comes along. In some cases, the user frames
/// never end up being logged. This may happen when the thread goes to sleep or
/// when it handles an interrupt. These kernel stacks are marked as lost, as we
/// have no way of tracing their origin anymore.
#[derive(Default)]
struct Collector {
    /// The processes to keep samples for.
    pids: FxHashSet<u32>,
    thread_pid: FxHashMap<u32, u32>,
    samples: Vec<RawSample>,
    samples_by_key: FxHashMap<(Qpc, u32), usize>,
    /// Samples per thread still waiting for their user frames.
    waiting: FxHashMap<u32, Vec<usize>>,
    presents: Vec<RawPresent>,
    /// When each thread started its current Present.
    presenting: FxHashMap<u32, Qpc>,
    images: Vec<ImageRecord>,
    thread_names: FxHashMap<u32, String>,
    sample_interval: Option<Duration>,
}
impl Collector {
    fn new(pids: FxHashSet<u32>) -> Self {
        Self {
            pids,
            ..Default::default()
        }
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Sample { qpc, tid, cpu } => self.on_sample(qpc, tid, cpu),
            Event::Stack {
                event_qpc,
                pid,
                tid,
                frames,
            } => self.on_stack(event_qpc, pid, tid, frames),
            Event::ImageLoad {
                base,
                size,
                pid,
                timestamp,
                path,
            } => self.on_image_load(base, size, pid, timestamp, path),
            Event::ThreadStart { pid, tid, name } => {
                self.thread_pid.insert(tid, pid);
                if let (true, Some(name)) = (self.pids.contains(&pid), name) {
                    self.thread_names.insert(tid, name);
                }
            }
            Event::ThreadName { pid, tid, name } => {
                if self.pids.contains(&pid) && !name.is_empty() {
                    self.thread_names.insert(tid, name);
                }
            }
            Event::SampleInterval { interval } => self.sample_interval = Some(interval),
            Event::PresentStart { qpc, pid, tid } => {
                if self.pids.contains(&pid) {
                    self.presenting.insert(tid, qpc);
                }
            }
            Event::PresentStop { qpc, pid, tid } => {
                if let Some(start_qpc) = self.presenting.remove(&tid) {
                    self.presents.push(RawPresent {
                        pid,
                        tid,
                        start_qpc,
                        end_qpc: qpc,
                    });
                }
            }
        }
    }

    fn on_sample(&mut self, qpc: Qpc, tid: u32, cpu: u16) {
        let Some(&pid) = self.thread_pid.get(&tid) else {
            return;
        };
        if self.pids.contains(&pid) {
            let next_index = self.samples.len();
            self.samples_by_key.insert((qpc, tid), next_index);
            // Samples and stacks arrive separately. It is up to on_stack to fill
            // this sample with the right stack frames.
            self.samples.push(RawSample {
                pid,
                tid,
                qpc,
                cpu,
                frames: Vec::new(),
                user_stack_lost: false,
            });
        }
    }

    fn on_stack(&mut self, event_qpc: Qpc, pid: u32, tid: u32, frames: &[u64]) {
        if !self.pids.contains(&pid) || frames.is_empty() {
            return;
        }

        // Other events may also emit stacks, in which case we won't have a corresponding
        // sample. We can just ignore those events.
        let Some(own_sample) = self.samples_by_key.get(&(event_qpc, tid)).copied() else {
            return;
        };

        // Note: stack frames are in memory order here: the stack sits 'upside down',
        // in memory, such that the bottom of the stack has the highest memory address.
        // Likewise, the top of the stack sits at index 0, and the bottom at len() - 1.

        // We know that stacks never leave the kernel, so if any frame sits inside
        // the kernel, the frames on top of it must too.
        // A stack whose tip sits in userspace must therefore be entirely in userspace.
        let user_only = frames[0] < KERNEL_START;
        // And a stack that originates in the kernel must be entirely in kernelspace.
        let kernel_only = frames[frames.len() - 1] >= KERNEL_START;

        let waiting = self.waiting.entry(tid).or_default();
        if user_only && !waiting.is_empty() {
            // If `waiting` contains our sample, we know there is at least one
            // kernel sample waiting for our user stack. It follows that any other
            // samples in the kernel sit on top of the same user stack, so they
            // can all be extended.
            // If it does not contain our sample, we know that they are waiting
            // for a user stack that isn't coming anymore, so we should mark
            // those kernel threads as having lost their user stack.
            let deferred = waiting.contains(&own_sample);
            for index in waiting.drain(..) {
                if deferred {
                    self.samples[index].frames.extend_from_slice(frames);
                } else {
                    self.samples[index].user_stack_lost = true;
                }
            }
            if deferred {
                return;
            }
        }

        self.samples[own_sample].frames.extend_from_slice(frames);
        if kernel_only {
            // This stack has only kernel frames. Mark the corresponding sample
            // as awaiting a user stack, so it can be filled in as soon as we
            // receive one.
            waiting.push(own_sample);
        }
    }

    fn on_image_load(&mut self, base: u64, size: u64, pid: u32, timestamp: u32, path: String) {
        let loaded_by_us = self.pids.contains(&pid);
        let loaded_by_kernel = pid == 0 || base >= KERNEL_START;

        if loaded_by_us || loaded_by_kernel {
            self.images.push(ImageRecord {
                base_addr: base,
                size,
                pid,
                link_timestamp: timestamp,
                image_path: path,
            });
        }
    }

    fn finish(&mut self) {
        // No more user stacks are coming, so any samples still waiting should be marked as lost
        for index in self.waiting.drain().flat_map(|(_, waiting)| waiting) {
            self.samples[index].user_stack_lost = true;
        }
    }
}

pub fn read(path: &Path, pids: FxHashSet<u32>) -> Result<RawTrace> {
    let mut collector = Collector::new(pids);
    let info = read_trace(path, &mut |event| collector.on_event(event))?;
    collector.finish();
    Ok(RawTrace {
        info,
        images: collector.images,
        samples: collector.samples,
        presents: collector.presents,
        thread_names: collector.thread_names,
        sample_interval: collector.sample_interval.context(
            "the trace does not contain a sample interval, please make sure it was recorded with CPU sampling",
        )?,
    })
}
