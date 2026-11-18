//! Resolves native addresses to function names, using PDBs from local folders and symbol servers.

use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use log::debug;
use std::path::{Path, PathBuf};
use std::time::Instant;
use wholesym::{CodeId, LookupAddress, SymbolManager, SymbolManagerConfig};

const MICROSOFT_SERVER: &str = "https://msdl.microsoft.com/download/symbols";
const UNITY_SERVER: &str = "https://symbolserver.unity3d.com";

const MAX_CONCURRENT_DLS: usize = 8;

pub struct ImageRequest<'a> {
    pub path: &'a Path,
    /// The link timestamp for the image at the time of tracing.
    pub timestamp: u32,
    /// Size of the image at the time of tracing.
    pub size: u64,
    /// Addresses relative to the image's base.
    pub rvas: Vec<u32>,
}

pub struct ImageResult {
    /// One entry per requested address.
    pub names: Vec<Option<String>>,
    /// Why the image has no names, if so.
    pub problem: Option<SymbolProblem>,
}

/// Describes a problem with name resolution in an image.
pub enum SymbolProblem {
    /// The file on disk does not match the image loaded in the trace.
    ImageDiffers,
    /// A PDB was not found for this image.
    PdbNotFound,
    /// Some other failure while reading the image or its symbols.
    Failed(String),
}

impl SymbolProblem {
    fn failed(error: impl Into<anyhow::Error>) -> Self {
        Self::Failed(format!("{:#}", error.into()))
    }
}

pub struct Symbolizer {
    manager: SymbolManager,
    // Holding the runtime in here keeps async within the symbolizer,
    // which is fine for our case. All we need it for is some concurrency
    // benefit on symbol downloads.
    runtime: tokio::runtime::Runtime,
}

impl Symbolizer {
    pub fn new(cache_dir: PathBuf, local_dirs: &[PathBuf]) -> Result<Self> {
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("couldn't create {}", cache_dir.display()))?;

        // We may want to support supplying additional symbol servers, but Unity and Windows
        // should cover most interesting functions.
        let mut config = SymbolManagerConfig::new()
            .respect_nt_symbol_path(false)
            .use_debuginfod(false)
            .windows_symbols_server(MICROSOFT_SERVER, &cache_dir)
            .windows_symbols_server(UNITY_SERVER, &cache_dir);
        for dir in local_dirs {
            config = config.extra_symbols_directory(dir);
        }

        Ok(Self {
            manager: SymbolManager::with_config(config),
            runtime: tokio::runtime::Runtime::new()?,
        })
    }

    pub fn resolve(&self, requests: Vec<ImageRequest<'_>>) -> Vec<ImageResult> {
        self.runtime.block_on(
            stream::iter(requests)
                .map(|request| self.resolve_image(request))
                .buffered(MAX_CONCURRENT_DLS)
                .collect(),
        )
    }

    async fn resolve_image(&self, request: ImageRequest<'_>) -> ImageResult {
        let started = Instant::now();
        let mut names = vec![None; request.rvas.len()];
        let problem = self.try_resolve_image(&request, &mut names).await.err();
        debug!(
            "symbolized {} addresses in {} in {:.2}s",
            names.len(),
            request.path.display(),
            started.elapsed().as_secs_f64()
        );
        ImageResult { names, problem }
    }

    /// Fills in `names` by best effort. If problems occur, the returned `Result`
    /// will indicate a failure, but the names written are still usable (although
    /// they may not be as informative).
    async fn try_resolve_image(
        &self,
        request: &ImageRequest<'_>,
        names: &mut Vec<Option<String>>,
    ) -> Result<(), SymbolProblem> {
        let info = SymbolManager::library_info_for_binary_at_path(request.path, None)
            .await
            .map_err(SymbolProblem::failed)?;

        if differs_from_trace(info.code_id.as_ref(), request.timestamp, request.size) {
            // This tends to happen if the trace is captured before, and analysed after
            // a (Windows) update.
            return Err(SymbolProblem::ImageDiffers);
        }

        *names = self
            .lookup_all(request)
            .await
            .map_err(SymbolProblem::failed)?;
        // `wholesym` doesn't tell us if it didn't find the PDB, and falls back to exports and invented
        // names. We will keep them as they are still better than nothing, but we will report it.
        if is_mostly_unnamed(names) {
            return Err(SymbolProblem::PdbNotFound);
        }
        Ok(())
    }

    /// Loads the image's symbols and looks up every requested address.
    async fn lookup_all(&self, request: &ImageRequest<'_>) -> Result<Vec<Option<String>>> {
        let map = self
            .manager
            .load_symbol_map_for_binary_at_path(request.path, None)
            .await?;
        Ok(request
            .rvas
            .iter()
            .map(|&rva| {
                map.lookup_sync(LookupAddress::Relative(rva))
                    .map(|info| info.symbol.name)
            })
            .collect())
    }
}

/// Returns whether a binary's identity differs from the one recorded during a trace.
fn differs_from_trace(code_id: Option<&CodeId>, timestamp: u32, size: u64) -> bool {
    match code_id {
        Some(CodeId::PeCodeId(id)) => {
            (timestamp != 0 && id.timestamp != timestamp) || u64::from(id.image_size) != size
        }
        _ => false,
    }
}

/// Returns true if the names in the symbol map don't look like they came from a PDB.
/// Because exports can still provide a real name, this is a heuristic.
fn is_mostly_unnamed(names: &[Option<String>]) -> bool {
    let unnamed = names
        .iter()
        .filter(|n| n.as_deref().is_none_or(is_invented_name))
        .count();
    unnamed * 2 > names.len()
}

/// Returns true if this is a name invented by `wholesym`, using the unwind tables.
fn is_invented_name(name: &str) -> bool {
    name.starts_with("fun_")
}
