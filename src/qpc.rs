//! Timestamps from the Windows performance counter.
//! This is the most accurate and highest resolution timer Windows exposes.
//! We use this when we need to correlate events by timestamp.

/// A performance counter reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Qpc(pub u64);

impl Qpc {
    /// Nanoseconds between `start` and here, at the requency given by `freq`.
    pub fn nanos_since(self, start: Qpc, freq: u64) -> u64 {
        ((self.0 - start.0) as u128 * 1_000_000_000 / freq as u128) as u64
    }
}
