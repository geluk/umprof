//! Contains functionality for reading JIT maps as emitted by our patched mono runtime.

use crate::qpc::Qpc;
use anyhow::{bail, Context, Result};
use log::warn;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JitMethod {
    pub start: u64,
    pub end: u64,
    pub compiled_qpc: Qpc,
    pub name: String,
    /// The highest end address of this entry and every entry before it in the map.
    max_end: u64,
}

pub struct JitMap {
    /// Sorted by start address.
    entries: Vec<JitMethod>,
    pub qpc_freq: Option<u64>,
}

impl JitMap {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("couldn't open {}", path.display()))?;
        Self::parse(&bytes).with_context(|| format!("couldn't parse {}", path.display()))
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        // The JIT map may still be being written to while we're reading it. That's fine
        // as the information we need will definitely be in the file by the time we end
        // up parsing it, so we'll just parse it until the last newline we encounter,
        // and ignore everything afterwards.
        let complete = bytes
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(&[][..], |last| &bytes[..=last]);
        let text = std::str::from_utf8(complete)?;

        let mut entries = Vec::new();
        let mut qpc_freq = None;
        for (index, line) in text.lines().enumerate() {
            if index == 0 {
                if let Some(header) = line.strip_prefix('#') {
                    qpc_freq = header
                        .split_whitespace()
                        .find_map(|f| f.strip_prefix("qpc-freq=")?.parse().ok());
                    continue;
                }
            }
            let Some(entry) = Self::parse_entry(line) else {
                bail!(
                    "line {}: expected '<start> <size> <qpc> <name>', but found '{line}'",
                    index + 1
                );
            };
            entries.push(entry);
        }

        entries.sort_by_key(|e| e.start);
        let mut max_end = 0;
        for entry in &mut entries {
            max_end = max_end.max(entry.end);
            entry.max_end = max_end;
        }
        Ok(Self { entries, qpc_freq })
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Looks up a JITed method at the given address and time.
    /// The QPC timestamp is necessary here as the same memory location may be
    /// reused multiple times as methods get recompiled.
    pub fn lookup_at(&self, address: u64, qpc: Qpc) -> Option<&JitMethod> {
        let candidates = &self.entries[..self.entries.partition_point(|e| e.start <= address)];
        let mut live: Option<&JitMethod> = None;
        for entry in candidates.iter().rev() {
            if entry.max_end <= address {
                break;
            }
            if address >= entry.end {
                continue;
            }
            if entry.compiled_qpc > qpc {
                // Code can't run before it's compiled, so this is likely a
                // timing issue, or an in correct file being used.
                warn!(
                    "a sample hit {} at 0x{address:x} before it was compiled, are you using the right JIT map?",
                    entry.name
                );
                continue;
            }
            if live.is_none_or(|l| entry.compiled_qpc > l.compiled_qpc) {
                live = Some(entry);
            }
        }
        live
    }

    fn parse_entry(line: &str) -> Option<JitMethod> {
        let mut parts = line.splitn(4, ' ');
        let start = u64::from_str_radix(parts.next()?, 16).ok()?;
        let size = u64::from_str_radix(parts.next()?, 16).ok()?;
        let qpc = Qpc(parts.next()?.parse().ok()?);
        let name = parts.next()?.to_string();
        Some(JitMethod {
            start,
            end: start.checked_add(size)?,
            compiled_qpc: qpc,
            name,
            max_end: 0,
        })
    }
}

/// The JIT map the runtime writes for a process.
pub fn path_for_pid(dir: &Path, pid: u32) -> PathBuf {
    dir.join(format!("mono-jit-{pid}.map"))
}

/// The process IDs of every JIT map in a directory.
pub fn pids_in_dir(dir: &Path) -> Result<Vec<u32>> {
    let entries = fs::read_dir(dir).with_context(|| format!("couldn't read {}", dir.display()))?;
    let mut pids = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let pid = name.to_str().and_then(|n| {
            n.strip_prefix("mono-jit-")?
                .strip_suffix(".map")?
                .parse::<u32>()
                .ok()
        });
        pids.extend(pid);
    }
    Ok(pids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(text: &str) -> JitMap {
        JitMap::parse(text.as_bytes()).unwrap()
    }

    fn name_at(m: &JitMap, address: u64, qpc: u64) -> Option<&str> {
        m.lookup_at(address, Qpc(qpc)).map(|e| e.name.as_str())
    }

    #[test]
    fn parses_the_header_and_names_with_spaces() {
        let m = map("# mono-jit-map pid=1 qpc-freq=10000000\n100 10 5 Foo:Bar (int, string)\n");
        assert_eq!(m.qpc_freq, Some(10_000_000));
        assert_eq!(name_at(&m, 0x100, 5), Some("Foo:Bar (int, string)"));
    }

    #[test]
    fn looks_up_addresses_within_a_range() {
        let m = map("100 10 1 first\n200 10 1 second\n");
        assert_eq!(name_at(&m, 0x100, 9), Some("first"));
        assert_eq!(name_at(&m, 0x10f, 9), Some("first"));
        assert_eq!(name_at(&m, 0x110, 9), None);
        assert_eq!(name_at(&m, 0xff, 9), None);
        assert_eq!(name_at(&m, 0x205, 9), Some("second"));
    }

    #[test]
    fn reused_address_resolves_according_to_time() {
        let m = map("100 10 5 old\n100 10 50 new\n");
        assert_eq!(name_at(&m, 0x105, 10), Some("old"));
        assert_eq!(name_at(&m, 0x105, 60), Some("new"));
    }

    #[test]
    fn overlapping_reuse_with_a_different_start_resolves_according_to_time() {
        let m = map("100 100 5 big_old\n150 10 50 small_new\n");
        assert_eq!(name_at(&m, 0x155, 10), Some("big_old"));
        assert_eq!(name_at(&m, 0x155, 60), Some("small_new"));
        // Outside the newer entry, the older entry still applies
        assert_eq!(name_at(&m, 0x180, 60), Some("big_old"));
    }

    #[test]
    fn duplicate_entries_for_one_address_resolve_consistently() {
        let m =
            map("100 a 5 delegate_invoke_has_target\n100 a 6 delegate_invoke_impl_has_target\n");
        for _ in 0..3 {
            assert_eq!(
                name_at(&m, 0x100, 9),
                Some("delegate_invoke_impl_has_target")
            );
        }
    }

    #[test]
    fn code_from_the_future_is_ignored() {
        let m = map("100 10 50 later\n");
        assert_eq!(name_at(&m, 0x105, 10), None);
    }

    #[test]
    fn tolerates_a_cut_off_last_line() {
        let m = JitMap::parse(b"100 10 5 complete\n200 10 5 caf\xC3").unwrap();
        assert_eq!(m.entries.len(), 1);
    }

    #[test]
    fn rejects_other_formats() {
        assert!(JitMap::parse("not a jit map at all\n".as_bytes()).is_err());
    }
}
