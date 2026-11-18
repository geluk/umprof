//! Holds the data model for profiles that have been fully parsed and symbolised.
//! This can be fed to an output format to produce a file that a profile viewer can read.

use std::fmt::{Display, Formatter};
use std::time::{Duration, SystemTime};

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
    pub messages: Vec<String>,
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
