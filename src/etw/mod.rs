//! Reads ETW traces into typed events, ignoring anything we're not interested in.

use crate::qpc::Qpc;
use anyhow::{Context as _, Result, bail};
use rustc_hash::FxHashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};
use windows::Win32::System::Diagnostics::Etw::*;
use windows::core::PWSTR;

mod parse;
mod windows_ext;
use parse::{Decoded, Header};

/// Addresses from here on up belong to the kernel.
pub const KERNEL_START: u64 = 0xffff_0000_0000_0000;

/// Identifies QPC as the clock.
const CLOCK_QPC: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// A timer interrupt.
    Sample {
        qpc: Qpc,
        tid: u32,
        cpu: u16,
    },
    /// A stack, captured alongside another event.
    Stack {
        /// QPC of the event this stack belongs to.
        event_qpc: Qpc,
        pid: u32,
        tid: u32,
        frames: &'a [u64],
    },
    ImageLoad {
        base: u64,
        size: u64,
        pid: u32,
        timestamp: u32,
        path: String,
    },
    ThreadStart {
        pid: u32,
        tid: u32,
        name: Option<String>,
    },
    ThreadName {
        pid: u32,
        tid: u32,
        name: String,
    },
    /// Specifies the sampling interval.
    SampleInterval {
        interval: Duration,
    },
    /// A thread started presenting a frame (DXGI).
    PresentStart {
        qpc: Qpc,
        pid: u32,
        tid: u32,
    },
    /// A thread finished presenting a frame (DXGI).
    PresentStop {
        qpc: Qpc,
        pid: u32,
        tid: u32,
    },
}

/// Holds general information and metadata about a trace.
#[derive(Debug)]
pub struct TraceInfo {
    pub perf_freq: u64,
    pub start_time: SystemTime,
    pub events_lost: u32,
    pub buffers_lost: u32,
    /// Map of `(opcode, version) -> count`, tracking the number of unreadable events.
    pub unsupported_events: FxHashMap<(u8, u8), u64>,
}

/// Calls `sink` for each event of interest in the trace.
pub fn read_trace(path: &Path, sink: &mut dyn FnMut(Event)) -> Result<TraceInfo> {
    let mut wide = windows_ext::to_wide_str(path);

    // This gets reused for every event that is processed.
    let mut ctx = Context {
        sink,
        scratch: Vec::new(),
        unsupported: FxHashMap::default(),
    };

    let mut log = create_logfile_struct(&mut wide, &mut ctx);

    // SAFETY: all pointers passed into ETW are only used for the duration of the calls.
    // All pointers outlive the ETW calls.
    unsafe {
        let handle = OpenTraceW(&mut log);
        if handle.Value == u64::MAX {
            return Err(windows::core::Error::from_thread())
                .with_context(|| format!("couldn't open {}", path.display()));
        }

        let header = &log.LogfileHeader;
        let pointer_size = header.Anonymous2.Anonymous.PointerSize;
        if pointer_size != 8 {
            let _ = CloseTrace(handle);
            bail!(
                "only 64-bit traces are supported, this trace has {}-bit pointers",
                pointer_size * 8
            );
        }
        if header.ReservedFlags != CLOCK_QPC {
            let _ = CloseTrace(handle);
            bail!(
                "the trace should use the QPC clock to correlate it with the JIT map, but it has {}",
                header.ReservedFlags
            );
        }

        let result = ProcessTrace(&[handle], None, None);
        let _ = CloseTrace(handle);
        result
            .ok()
            .with_context(|| format!("couldn't read {}", path.display()))?;

        Ok(TraceInfo {
            perf_freq: header.PerfFreq as u64,
            start_time: windows_ext::filetime_to_system_time(header.StartTime),
            events_lost: header.Anonymous2.Anonymous.EventsLost,
            buffers_lost: header.BuffersLost,
            unsupported_events: ctx.unsupported,
        })
    }
}

fn create_logfile_struct(wide: &mut [u16], ctx: &mut Context<'_>) -> EVENT_TRACE_LOGFILEW {
    // https://learn.microsoft.com/en-us/windows/win32/api/evntrace/ns-evntrace-event_trace_logfilew
    EVENT_TRACE_LOGFILEW {
        LogFileName: PWSTR(wide.as_mut_ptr()),
        Anonymous1: EVENT_TRACE_LOGFILEW_0 {
            ProcessTraceMode: PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP,
        },
        Anonymous2: EVENT_TRACE_LOGFILEW_1 {
            EventRecordCallback: Some(on_event),
        },
        Context: ctx as *mut Context as *mut _,
        ..Default::default()
    }
}

struct Context<'a> {
    sink: &'a mut dyn FnMut(Event),
    scratch: Vec<u64>,
    unsupported: FxHashMap<(u8, u8), u64>,
}

unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    // SAFETY: We expect ETW to pass a valid record, that holds our Context.
    unsafe {
        let record = &*record;
        let ctx = &mut *(record.UserContext as *mut Context);
        let payload = if record.UserData.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(record.UserData as *const u8, record.UserDataLength as usize)
        };

        let event = &record.EventHeader;
        let opcode = event.EventDescriptor.Opcode;
        let version = event.EventDescriptor.Version;
        let header = Header {
            provider: &event.ProviderId,
            id: event.EventDescriptor.Id,
            opcode,
            version,
            pid: event.ProcessId,
            tid: event.ThreadId,
            qpc: Qpc(event.TimeStamp as u64),
            cpu: record.BufferContext.Anonymous.ProcessorIndex,
        };

        match parse::decode(&header, payload, &mut ctx.scratch) {
            Decoded::Event(event) => (ctx.sink)(event),
            Decoded::Ignored => {}
            Decoded::Unsupported => *ctx.unsupported.entry((opcode, version)).or_default() += 1,
        }
    }
}
