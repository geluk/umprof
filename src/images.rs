//! Keeps track of loaded images; recording their memory ranges and holding
//! their location and some metadata to allow querying for symbols.

use crate::etw::KERNEL_START;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ImageRecord {
    pub base_addr: u64,
    pub size: u64,
    pub pid: u32,
    pub link_timestamp: u32,
    pub image_path: String,
}

#[derive(Debug, Clone)]
pub struct Image {
    pub base_addr: u64,
    pub end_addr: u64,
    pub link_timestamp: u32,
    pub name: String,
    pub image_path: PathBuf,
}

/// Represents all images mapped into a single process. This also includes the
/// kernel.
pub struct ImageTable {
    /// Maps images by base address.
    images: BTreeMap<u64, Image>,
}

impl ImageTable {
    pub fn new(records: &[ImageRecord], pid: u32) -> Self {
        let images = records
            .iter()
            .filter(|r| r.pid == pid || r.pid == 0 || r.base_addr >= KERNEL_START)
            .map(create_image)
            .collect();
        Self { images }
    }

    /// Finds the image pointed into by the given address.
    pub fn find(&self, address: u64) -> Option<&Image> {
        let (_, image) = self.images.range(..=address).next_back()?;
        (address < image.end_addr).then_some(image)
    }
}

fn create_image(r: &ImageRecord) -> (u64, Image) {
    let path = to_win32_path(&r.image_path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let image = Image {
        base_addr: r.base_addr,
        end_addr: r.base_addr + r.size,
        link_timestamp: r.link_timestamp,
        name,
        image_path: path,
    };
    (r.base_addr, image)
}

/// Converts a kernel path to one that file APIs will accept.
fn to_win32_path(path: &str) -> PathBuf {
    if strip_prefix_ci(path, r"\Device\").is_some() {
        return PathBuf::from(format!(r"\\?\GLOBALROOT{path}"));
    }
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    if let Some(rest) = strip_prefix_ci(path, r"\SystemRoot\") {
        return Path::new(&system_root).join(rest);
    }
    if let Some(rest) = path.strip_prefix(r"\??\") {
        return PathBuf::from(rest);
    }
    // Windows considers these paths to be relative to the system drive
    if let Some(rest) = strip_prefix_ci(path, r"\Windows\") {
        return Path::new(&system_root).join(rest);
    }
    PathBuf::from(path)
}

fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}
