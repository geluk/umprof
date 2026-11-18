//! Parsing logic for ETW events.

use super::Event;
use crate::qpc::Qpc;
use std::time::Duration;
use windows::core::GUID;

// Although some of these definitions are present in the `windows` crate, not all
// of them are, so we'll define every one we use here.
const PERF_INFO: GUID = GUID::from_u128(0xce1dbfb4_137e_4da6_87b0_3f59aa102cbc);
const STACK_WALK: GUID = GUID::from_u128(0xdef2fe46_7bd6_4b80_bd94_f57fe20d0ce3);
const THREAD: GUID = GUID::from_u128(0x3d6fa8d1_fe05_11d0_9dda_00c04fd7ba7c);
const IMAGE: GUID = GUID::from_u128(0x2cb15d1d_5fc1_11d2_abe1_00a0c911f518);
const DXGI: GUID = GUID::from_u128(0xca11c036_0102_4a2d_a6ad_f03cfed5d3c9);

const PERF_INFO_SAMPLE: u8 = 46;
const PERF_INFO_COLLECTION_START: u8 = 73;
const STACK_WALK_STACK: u8 = 32;
const THREAD_START: u8 = 1;
const THREAD_DC_START: u8 = 3;
const THREAD_SET_NAME: u8 = 72;
const IMAGE_DC_START: u8 = 3;
const IMAGE_LOAD: u8 = 10;

const DXGI_PRESENT_START: u16 = 42;
const DXGI_PRESENT_STOP: u16 = 43;

/// Small abstraction over event payloads to facilitate reading.
struct Payload<'a>(&'a [u8]);

impl Payload<'_> {
    fn read_u32(&self, off: usize) -> Option<u32> {
        Some(u32::from_le_bytes(
            self.0.get(off..off + 4)?.try_into().ok()?,
        ))
    }

    fn read_u64(&self, off: usize) -> Option<u64> {
        Some(u64::from_le_bytes(
            self.0.get(off..off + 8)?.try_into().ok()?,
        ))
    }

    /// Reads a WSTR into a native Rust `String`
    fn read_string(&self, off: usize) -> Option<String> {
        let units: Vec<u16> = self
            .0
            .get(off..)?
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }
}

pub enum Decoded<'a> {
    Event(Event<'a>),
    /// Not an event we're interested in.
    Ignored,
    /// An event we're interested in, that turned out to be unreadable.
    Unsupported,
}

/// Holds metadata for an event.
pub struct Header<'a> {
    pub provider: &'a GUID,
    pub id: u16,
    pub opcode: u8,
    pub version: u8,
    pub pid: u32,
    pub tid: u32,
    pub qpc: Qpc,
    pub cpu: u16,
}

impl<'a> From<Option<Event<'a>>> for Decoded<'a> {
    fn from(event: Option<Event<'a>>) -> Self {
        match event {
            Some(e) => Decoded::Event(e),
            None => Decoded::Unsupported,
        }
    }
}

/// Decodes a single event.
pub fn decode<'a>(header: &Header, payload: &[u8], scratch: &'a mut Vec<u64>) -> Decoded<'a> {
    let p = Payload(payload);
    match *header.provider {
        DXGI => decode_dxgi(header),
        PERF_INFO => decode_perf_info(header, &p),
        STACK_WALK => decode_stack_walk(header, &p, scratch),
        THREAD => decode_thread(header, &p),
        IMAGE => decode_image(header, &p),
        _ => Decoded::Ignored,
    }
}

fn decode_dxgi(header: &Header) -> Decoded<'static> {
    let &Header { qpc, pid, tid, .. } = header;
    match header.id {
        DXGI_PRESENT_START => Decoded::Event(Event::PresentStart { qpc, pid, tid }),
        DXGI_PRESENT_STOP => Decoded::Event(Event::PresentStop { qpc, pid, tid }),
        _ => Decoded::Ignored,
    }
}

fn decode_perf_info(header: &Header, p: &Payload) -> Decoded<'static> {
    let &Header {
        qpc, cpu, version, ..
    } = header;
    match header.opcode {
        PERF_INFO_SAMPLE if version >= 2 => p
            .read_u32(8)
            .map(|tid| Event::Sample { qpc, tid, cpu })
            .into(),
        PERF_INFO_COLLECTION_START if version >= 3 => match p.read_u32(0) {
            // A timer event
            Some(0) => p
                .read_u32(4)
                // Intervals are given in units of 100 ns
                .map(|ticks| Event::SampleInterval {
                    interval: Duration::from_nanos(u64::from(ticks) * 100),
                })
                .into(),
            // Some other hardware counter
            Some(_) => Decoded::Ignored,
            None => Decoded::Unsupported,
        },
        PERF_INFO_SAMPLE | PERF_INFO_COLLECTION_START => Decoded::Unsupported,
        _ => Decoded::Ignored,
    }
}

fn decode_stack_walk<'a>(header: &Header, p: &Payload, scratch: &'a mut Vec<u64>) -> Decoded<'a> {
    match header.opcode {
        STACK_WALK_STACK if header.version >= 2 => decode_stack(p, scratch).into(),
        STACK_WALK_STACK => Decoded::Unsupported,
        _ => Decoded::Ignored,
    }
}

fn decode_stack<'a>(p: &Payload, scratch: &'a mut Vec<u64>) -> Option<Event<'a>> {
    const STACK_FRAMES_OFFSET: usize = 16;

    let frames = p.0.get(STACK_FRAMES_OFFSET..)?;
    // The scratch space gets reused across events, so we must clear it before use
    scratch.clear();
    scratch.extend(
        frames
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap())),
    );
    Some(Event::Stack {
        event_qpc: Qpc(p.read_u64(0)?),
        pid: p.read_u32(8)?,
        tid: p.read_u32(12)?,
        frames: scratch,
    })
}

fn decode_thread(header: &Header, p: &Payload) -> Decoded<'static> {
    match header.opcode {
        THREAD_START | THREAD_DC_START if header.version >= 3 => decode_thread_start(p).into(),
        THREAD_SET_NAME if header.version >= 2 => decode_thread_name(p).into(),
        THREAD_START | THREAD_DC_START | THREAD_SET_NAME => Decoded::Unsupported,
        _ => Decoded::Ignored,
    }
}

fn decode_thread_start(p: &Payload) -> Option<Event<'static>> {
    const THREAD_NAME_OFFSET: usize = 72;

    Some(Event::ThreadStart {
        pid: p.read_u32(0)?,
        tid: p.read_u32(4)?,
        name: p.read_string(THREAD_NAME_OFFSET).filter(|n| !n.is_empty()),
    })
}

fn decode_thread_name(p: &Payload) -> Option<Event<'static>> {
    const SET_NAME_OFFSET: usize = 8;

    Some(Event::ThreadName {
        pid: p.read_u32(0)?,
        tid: p.read_u32(4)?,
        name: p.read_string(SET_NAME_OFFSET)?,
    })
}

fn decode_image(header: &Header, p: &Payload) -> Decoded<'static> {
    match header.opcode {
        IMAGE_LOAD | IMAGE_DC_START if header.version >= 3 => decode_image_load(p).into(),
        IMAGE_LOAD | IMAGE_DC_START => Decoded::Unsupported,
        _ => Decoded::Ignored,
    }
}

fn decode_image_load(p: &Payload) -> Option<Event<'static>> {
    const IMAGE_PATH_OFFSET: usize = 56;

    Some(Event::ImageLoad {
        base: p.read_u64(0)?,
        size: p.read_u64(8)?,
        pid: p.read_u32(16)?,
        timestamp: p.read_u32(24)?,
        path: p.read_string(IMAGE_PATH_OFFSET)?,
    })
}
