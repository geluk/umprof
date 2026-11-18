//! Turns a raw trace into a profile.

use crate::etw::{KERNEL_START, TraceInfo};
use crate::images::{Image, ImageTable};
use crate::jit_map::JitMap;
use crate::model::{
    DiagnosticMessage, Diagnostics, Frame, FrameKind, Present, Profile, Sample, Thread,
    USER_STACK_LOST,
};
use crate::qpc::Qpc;
use crate::symbols::{ImageRequest, SymbolProblem, Symbolizer};
use crate::trace::{RawPresent, RawSample, RawTrace};
use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};

/// Interns frames, aggregating different addresses in one function into a single frame.
struct FrameTable {
    frames: Vec<Frame>,
    /// Maps managed frames to table indices. Managed frames cannot be referenced by
    /// base address because this may change across JIT invocations, so we treat the
    /// entire frame as its identity.
    ids: FxHashMap<Frame, u32>,
    /// Maps native addresses to frame indices. Native addresses always contain the
    /// same code, so here just the addres is enough to identify it.
    native: FxHashMap<u64, u32>,
}

impl FrameTable {
    fn new() -> Self {
        // Stands in for the missing user frames, so those samples don't look like kernel threads
        let user_stack_lost = Frame {
            name: USER_STACK_LOST.to_string(),
            kind: FrameKind::Unknown,
            image: None,
        };

        let mut ids = FxHashMap::default();
        ids.insert(user_stack_lost.clone(), 0);

        FrameTable {
            frames: vec![user_stack_lost],
            ids,
            native: FxHashMap::default(),
        }
    }

    /// Gets the index of the 'user stack lost' frame.
    fn user_stack_lost() -> u32 {
        // Always sits at index 0
        0
    }

    /// Interns a frame, storing it only if it is not yet present and returning its index.
    fn intern(&mut self, frame: Frame) -> u32 {
        if let Some(&id) = self.ids.get(&frame) {
            return id;
        }
        let id = self.frames.len() as u32;
        self.frames.push(frame.clone());
        self.ids.insert(frame, id);
        id
    }

    /// Resolves a function address to an interned frame index, interning frames on demand.
    /// This needs `Qpc` because JIT frames are time-dependent, as the same memory location
    /// may hold different JITed methods at different points in time.
    fn resolve(
        &mut self,
        address: u64,
        qpc: Qpc,
        jit: &JitMap,
        images: &ImageTable,
        native_names: &FxHashMap<u64, String>,
        diagnostics: &mut Diagnostics,
    ) -> u32 {
        if let Some(&id) = self.native.get(&address) {
            return id;
        }
        if let Some(entry) = jit.lookup_at(address, qpc) {
            let (name, kind) = Self::classify_jit_name(&entry.name);
            return self.intern(Frame {
                name,
                kind,
                image: None,
            });
        }

        let kind = if address >= KERNEL_START {
            FrameKind::Kernel
        } else {
            FrameKind::Native
        };
        let frame = match images.find(address) {
            Some(image) => {
                let name = native_names
                    .get(&address)
                    .cloned()
                    .unwrap_or_else(|| format!("{}+0x{:x}", image.name, address - image.base_addr));
                Frame {
                    name,
                    kind,
                    image: Some(image.name.clone()),
                }
            }
            None => {
                diagnostics.unknown_frames += 1;
                Frame {
                    name: format!("?? 0x{address:x}"),
                    kind: FrameKind::Unknown,
                    image: None,
                }
            }
        };
        let id = self.intern(frame);
        self.native.insert(address, id);
        id
    }

    /// Cleans up a JIT-generated name, and classifies its frame kind
    fn classify_jit_name(name: &str) -> (String, FrameKind) {
        const HARMONY_PREFIX: &str =
            "(wrapper dynamic-method) MonoMod.Utils.DynamicMethodDefinition:";

        if name == "(null)" {
            return ("[mono trampoline]".to_string(), FrameKind::Trampoline);
        }

        // Harmony-patched methods have this prefix. We strip it for legibility
        // reasons, as we can just mark the method as patched instead.
        if let Some(patched) = name.strip_prefix(HARMONY_PREFIX) {
            return (patched.to_string(), FrameKind::Managed { is_patched: true });
        }
        let kind = if name.starts_with("(wrapper dynamic-method)") {
            FrameKind::Managed { is_patched: false }
        } else if name.starts_with("(wrapper ") {
            FrameKind::Wrapper
        } else if name.contains(':') {
            FrameKind::Managed { is_patched: false }
        } else {
            FrameKind::Trampoline
        };
        (name.to_string(), kind)
    }
}

pub struct Inputs<'a> {
    pub name: String,
    pub process_id: u32,
    pub trace: RawTrace,
    pub jit: &'a JitMap,
    pub images: &'a ImageTable,
    pub symbolizer: Option<&'a Symbolizer>,
}

pub fn build_profile(inputs: Inputs<'_>) -> Result<Profile> {
    let Inputs {
        name,
        process_id,
        trace:
            RawTrace {
                info: trace_info,
                samples,
                presents,
                thread_names,
                sample_interval,
                ..
            },
        jit,
        images,
        symbolizer,
    } = inputs;

    let samples: Vec<RawSample> = samples
        .into_iter()
        .filter(|s| s.pid == process_id)
        .collect();

    let mut diagnostics = create_diagnostics(&trace_info, &samples, jit);
    let native_names = resolve_native_names(&samples, images, symbolizer, &mut diagnostics);
    let mut frames = FrameTable::new();

    let start_qpc = samples.iter().map(|s| s.qpc).min().unwrap();
    let ns_since_start = |qpc: Qpc| qpc.nanos_since(start_qpc, trace_info.perf_freq);

    let by_thread = collect_samples_by_thread(
        jit,
        images,
        samples,
        &mut diagnostics,
        native_names,
        &mut frames,
        ns_since_start,
    );

    let presents = create_presents(process_id, presents, start_qpc, ns_since_start);
    let threads = create_threads(thread_names, &frames, by_thread);

    Ok(Profile {
        name,
        process_id,
        start_time: trace_info.start_time,
        sample_interval,
        frames: frames.frames,
        threads,
        presents,
        diagnostics,
    })
}

fn create_diagnostics(info: &TraceInfo, samples: &[RawSample], jit: &JitMap) -> Diagnostics {
    let mut diagnostics = Diagnostics {
        events_lost: info.events_lost,
        buffers_lost: info.buffers_lost,
        unsupported_events: info.unsupported_events.values().sum(),
        ..Default::default()
    };
    for (&(opcode, version), &count) in &info.unsupported_events {
        diagnostics.messages.push(DiagnosticMessage::UnknownEvent {
            opcode,
            version,
            count,
        });
    }
    if let Some(jit_hz) = jit.qpc_freq.filter(|&f| f != info.perf_freq) {
        diagnostics.messages.push(DiagnosticMessage::ClockMismatch {
            jit_hz,
            trace_hz: info.perf_freq,
        });
    }
    let lost = samples.iter().filter(|s| s.user_stack_lost).count();
    if lost > 0 {
        diagnostics.messages.push(DiagnosticMessage::UserStackLost {
            lost,
            total: samples.len(),
        });
    }

    diagnostics
}

/// Resolves native names, looking them up through the PDB image table, or downloading
/// new images when no PDBs exist yet.
fn resolve_native_names(
    samples: &[RawSample],
    images: &ImageTable,
    symbolizer: Option<&Symbolizer>,
    diagnostics: &mut Diagnostics,
) -> FxHashMap<u64, String> {
    let mut names = FxHashMap::default();

    let Some(symbolizer) = symbolizer else {
        // Symbols are turned off
        return names;
    };

    let mut frames_by_image: FxHashMap<u64, (&Image, FxHashSet<u64>)> = FxHashMap::default();
    for sample in samples {
        for &address in &sample.frames {
            if let Some(image) = images.find(address) {
                frames_by_image
                    .entry(image.base_addr)
                    .or_insert_with(|| (image, FxHashSet::default()))
                    .1
                    .insert(address);
            }
        }
    }

    let mut requested = Vec::new();
    let mut requests = Vec::new();
    for (image, addresses) in frames_by_image.into_values() {
        let addresses: Vec<u64> = addresses.into_iter().collect();
        let rvas = addresses
            .iter()
            .map(|&a| (a - image.base_addr) as u32)
            .collect();
        requests.push(ImageRequest {
            path: &image.image_path,
            timestamp: image.link_timestamp,
            size: image.end_addr - image.base_addr,
            rvas,
        });
        requested.push((image, addresses));
    }

    let results = symbolizer.resolve(requests);
    for ((image, addresses), result) in requested.into_iter().zip(results) {
        if let Some(problem) = result.problem {
            let image_name = image.name.clone();
            let is_driver = image.base_addr >= KERNEL_START;
            diagnostics.messages.push(match problem {
                SymbolProblem::ImageDiffers => DiagnosticMessage::ImageDiffers {
                    image_name,
                    is_driver,
                },
                SymbolProblem::PdbNotFound => DiagnosticMessage::PdbNotFound {
                    image_name,
                    is_driver,
                },
                SymbolProblem::Failed(error) => DiagnosticMessage::SymbolsFailed {
                    image_name,
                    is_driver,
                    error,
                },
            });
        }
        for (address, name) in addresses.into_iter().zip(result.names) {
            if let Some(name) = name {
                names.insert(address, name);
            }
        }
    }
    names.shrink_to_fit();
    names
}

fn collect_samples_by_thread(
    jit: &JitMap,
    images: &ImageTable,
    samples: Vec<RawSample>,
    diagnostics: &mut Diagnostics,
    native_names: FxHashMap<u64, String>,
    frames: &mut FrameTable,
    ns_since_start: impl Fn(Qpc) -> u64,
) -> FxHashMap<u32, Vec<Sample>> {
    let mut by_thread: FxHashMap<u32, Vec<Sample>> = FxHashMap::default();
    for raw in &samples {
        let mut stack: Vec<u32> = raw
            .frames
            .iter()
            .map(|&address| {
                frames.resolve(address, raw.qpc, jit, images, &native_names, diagnostics)
            })
            .collect();
        if raw.user_stack_lost {
            stack.push(FrameTable::user_stack_lost());
        }
        by_thread.entry(raw.tid).or_default().push(Sample {
            time_ns: ns_since_start(raw.qpc),
            cpu: raw.cpu,
            stack,
        });
    }
    by_thread
}

fn create_presents(
    process_id: u32,
    presents: Vec<RawPresent>,
    start_qpc: Qpc,
    ns_since_start: impl Fn(Qpc) -> u64,
) -> Vec<Present> {
    let mut presents: Vec<Present> = presents
        .iter()
        .filter(|p| p.pid == process_id && p.start_qpc >= start_qpc)
        .map(|p| Present {
            thread_id: p.tid,
            start_ns: ns_since_start(p.start_qpc),
            end_ns: ns_since_start(p.end_qpc),
        })
        .collect();
    presents.sort_by_key(|p| p.start_ns);
    presents
}

fn create_threads(
    thread_names: FxHashMap<u32, String>,
    frames: &FrameTable,
    by_thread: FxHashMap<u32, Vec<Sample>>,
) -> Vec<Thread> {
    let mut threads: Vec<Thread> = by_thread
        .into_iter()
        .map(|(id, mut samples)| {
            samples.sort_by_key(|s| s.time_ns);
            Thread {
                id,
                name: thread_names.get(&id).cloned(),
                is_main: false,
                samples,
            }
        })
        .collect();
    mark_main_thread(&mut threads, &frames.frames);
    threads.sort_by_key(|t| (!t.is_main, std::cmp::Reverse(t.samples.len()), t.id));
    threads
}

/// Detects the main thread and marks it as such.
fn mark_main_thread(threads: &mut [Thread], frames: &[Frame]) {
    let in_executable: Vec<bool> = frames
        .iter()
        .map(|f| {
            f.image
                .as_deref()
                .is_some_and(|i| i.to_ascii_lowercase().ends_with(".exe"))
        })
        .collect();
    let main = threads
        .iter_mut()
        .map(|t| {
            let count = t
                .samples
                .iter()
                .filter(|s| s.stack.iter().any(|&f| in_executable[f as usize]))
                .count();
            (count, t)
        })
        .filter(|(count, _)| *count > 0)
        .max_by_key(|(count, _)| *count);
    if let Some((_, thread)) = main {
        thread.is_main = true;
    }
}
