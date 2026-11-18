//! Holds the data model for profiles that have been fully parsed and symbolised.
//! This can be fed to an output format to produce a file that a profile viewer can read.

use std::fmt::{Display, Formatter};
use std::time::{Duration, SystemTime};

/// Descriptor for kernel frames whose user stack was never reported
pub const USER_STACK_LOST: &str = "[user stack lost]";

/// A full profile, holding information for multiple threads within single process.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub process_id: u32,
    /// When the recording started.
    pub start_time: SystemTime,
    /// How much CPU time each sample stands for.
    pub sample_interval: Duration,
    pub frames: Vec<Frame>,
    /// Main thread first, then the busiest first.
    pub threads: Vec<Thread>,
    /// DXGI present events, in time order.
    pub presents: Vec<Present>,
    pub diagnostics: Diagnostics,
}

/// Some diagnostics that give an idication of how trustworthy the profile is.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    pub events_lost: u32,
    pub buffers_lost: u32,
    /// Events the reader recognised but couldn't parse.
    pub unsupported_events: u64,
    /// Number of frames not pointing into a loaded image or a JIT map.
    pub unknown_frames: u64,
    pub messages: Vec<DiagnosticMessage>,
}

#[derive(Debug, Clone)]
/// Diagnostic message indicating a problem during resolution.
pub enum DiagnosticMessage {
    /// The file on disk does not match the image loaded in the trace.
    ImageDiffers { image_name: String, is_driver: bool },
    /// A PDB was not found for this image.
    PdbNotFound { image_name: String, is_driver: bool },
    /// Reading this image or its symbols failed.
    SymbolsFailed {
        image_name: String,
        is_driver: bool,
        error: String,
    },
    /// The trace contains events that could not be read.
    UnknownEvent { opcode: u8, version: u8, count: u64 },
    /// Mismatch between the JIT and trace clock frequencies.
    ClockMismatch { jit_hz: u64, trace_hz: u64 },
    /// Kernel samples that could not be traced back to userspace.
    UserStackLost { lost: usize, total: usize },
}

impl DiagnosticMessage {
    /// True if this concerns a kernel driver.
    pub fn is_about_driver(&self) -> bool {
        match self {
            Self::ImageDiffers { is_driver, .. }
            | Self::PdbNotFound { is_driver, .. }
            | Self::SymbolsFailed { is_driver, .. } => *is_driver,
            _ => false,
        }
    }
}

impl Display for DiagnosticMessage {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Self::ImageDiffers { image_name, .. } => write!(
                f,
                "no symbols for {image_name}: the file on disk differs from the one in the trace"
            ),
            Self::PdbNotFound { image_name, .. } => write!(
                f,
                "no symbols for {image_name}: could not find a PDB on any symbol server"
            ),
            Self::SymbolsFailed {
                image_name, error, ..
            } => write!(f, "no symbols for {image_name}: {error}"),
            Self::UnknownEvent {
                opcode,
                version,
                count,
            } => write!(
                f,
                "could not read {count} events with opcode {opcode}, version {version}"
            ),
            Self::ClockMismatch { jit_hz, trace_hz } => write!(
                f,
                "the JIT map and trace do not run on the same clock (jit: {jit_hz} Hz, trace: \
                 {trace_hz} Hz), this may result in incorrect method names"
            ),
            Self::UserStackLost { lost, total } => write!(
                f,
                "{lost} kernel samples ({:.1}%) could not be traced back to userspace, they are \
                 marked as \"{USER_STACK_LOST}\"",
                *lost as f64 * 100.0 / *total as f64
            ),
        }
    }
}

/// A single thread being sampled.
#[derive(Debug, Clone)]
pub struct Thread {
    pub id: u32,
    pub name: Option<String>,
    pub is_main: bool,
    /// Samples ordered by timestamp.
    pub samples: Vec<Sample>,
}
impl Thread {
    pub fn display_name(&self) -> String {
        match (&self.name, self.is_main) {
            (Some(name), _) => format!("{name} ({})", self.id),
            (None, true) => format!("Main thread ({})", self.id),
            (None, false) => format!("Thread {}", self.id),
        }
    }
}

/// A single CPU sample. Records the thread's stack at the moment of sampling.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Nanoseconds since the start of the profile.
    pub time_ns: u64,

    #[allow(unused)] // Not currently used as the FF profiler cannot do anything with this
    /// CPU the thread was executing on.
    pub cpu: u16,

    /// Holds the call stack at the moment of sampling, represented as indices
    /// into [`Profile::frames`], top first.
    pub stack: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Frame {
    pub name: String,
    pub kind: FrameKind,
    /// The binary the code of this frame lives in. Only applicable to native code.
    pub image: Option<String>,
}
impl Display for Frame {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match &self.image {
            // frames without a symbol already tend to include the image name
            Some(image) if !self.name.starts_with(image.as_str()) => {
                write!(f, "{image}!{}", self.name)
            }
            _ => f.write_str(&self.name),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameKind {
    /// A managed (C#) method.
    Managed {
        /// True if the method is a Harmony patch.
        is_patched: bool,
    },
    /// Mono wrappers. These sit on the boundaries between managed and native code.
    Wrapper,
    /// Small stubs of machine code that will run some kind of initalisation
    /// (like calling the JIT compiler) on first run, before patching themselves out.
    Trampoline,
    /// Native code running in user mode.
    Native,
    /// Native code running in the kernel.
    Kernel,
    /// An address that could not be resolved to known code.
    Unknown,
}

/// A DXGI `present` event, representing a frame being handed over to the GPU.
/// This marks where one frame ends and the next one begins, on the CPU side.
/// The GPU will normally still need some time to finish rendering the frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Present {
    /// The thread that requested the buffer swap.
    pub thread_id: u32,
    pub start_ns: u64,
    pub end_ns: u64,
}
