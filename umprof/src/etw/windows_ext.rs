use std::{
    path::Path,
    time::{Duration, SystemTime},
};

/// Converts a Windows `FILETIME` to `SystemTime`.
pub fn filetime_to_system_time(filetime: i64) -> SystemTime {
    const SECONDS_FROM_1601_TO_1970: u64 = 11_644_473_600;
    let ticks = filetime as u64;
    let since_1601 =
        Duration::from_secs(ticks / 10_000_000) + Duration::from_nanos(ticks % 10_000_000 * 100);
    SystemTime::UNIX_EPOCH - Duration::from_secs(SECONDS_FROM_1601_TO_1970) + since_1601
}

/// Converts a `Path` to a null-terminated UTF-16 string for use in Windows APIs.
pub fn to_wide_str(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain([0])
        .collect()
}
