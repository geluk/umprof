mod etw;
mod firefox;
mod images;
mod jit_map;
mod model;
mod qpc;
mod resolve;
mod symbols;
mod trace;

use anyhow::{Context, Result, bail};
use clap::Parser;
use images::ImageTable;
use jit_map::JitMap;
use log::{Level, LevelFilter, debug, info, warn};
use rustc_hash::{FxHashMap, FxHashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use symbols::Symbolizer;

fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(args.verbose);

    let start_time = Instant::now();
    let map_dir = args.resolve_map_dir();
    let candidates = args.resolve_candidate_pids(&map_dir)?;

    let raw_trace = trace::read(&args.trace, candidates)?;
    debug!(
        "read {} samples and {} images from {}",
        raw_trace.samples.len(),
        raw_trace.images.len(),
        args.trace.display()
    );
    let process_id = choose_process(&raw_trace, args.pid)?;
    debug!("profiling process {process_id}");

    let jit_path = jit_map::path_for_pid(&map_dir, process_id);
    let jit = JitMap::load(&jit_path)?;
    debug!(
        "read {} entries from {}",
        jit.entry_count(),
        jit_path.display()
    );
    let images = ImageTable::new(&raw_trace.images, process_id);

    let symbolizer = if args.no_symbols {
        None
    } else {
        let cache = args.resolve_symbol_cache()?;
        Some(Symbolizer::new(cache, &args.symbol_dirs)?)
    };

    let name = args
        .trace
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let profile = resolve::build_profile(resolve::Inputs {
        name: name.clone(),
        process_id,
        trace: raw_trace,
        jit: &jit,
        images: &images,
        symbolizer: symbolizer.as_ref(),
    })?;

    let path = write_output(&profile, &args.output, &name)?;
    info!("Wrote {}", path.display());
    report_diagnostics(&profile.diagnostics);
    info!("Finished in {:.2}s", start_time.elapsed().as_secs_f64());
    Ok(())
}

fn init_logging(verbose: bool) {
    let level = if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };

    env_logger::Builder::new()
        .filter_level(LevelFilter::Warn)
        .filter_module(module_path!(), level)
        .parse_default_env()
        .format(|out, record| {
            let label = match record.level() {
                Level::Info => return writeln!(out, "{}", record.args()),
                Level::Error => "error",
                Level::Warn => "warning",
                Level::Debug => "debug",
                Level::Trace => "trace",
            };
            let style = out.default_level_style(record.level()).bold();
            writeln!(out, "{style}{label}:{style:#} {}", record.args())
        })
        .init();
}

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// The ETW trace to read
    trace: PathBuf,

    /// Directory containing the JIT maps [default: the trace's containing directory]
    #[arg(long)]
    map_dir: Option<PathBuf>,

    /// PID of the process to profile [default: the most-sampled process for which a JIT map exists]
    #[arg(long)]
    pid: Option<u32>,

    /// Path to the directory where output files will be written
    #[arg(short, long, default_value = "out")]
    output: PathBuf,

    /// Don't look up native function names
    #[arg(long)]
    no_symbols: bool,

    /// Extra folders to search for PDBs
    #[arg(long = "symbols-dir")]
    symbol_dirs: Vec<PathBuf>,

    /// Symbol cache path for managed PDBs [default: %LOCALAPPDATA%\umprof\symbols]
    #[arg(long)]
    symbol_cache: Option<PathBuf>,

    /// Show what each step is doing
    #[arg(short, long)]
    verbose: bool,
}
fn write_output(profile: &model::Profile, dir: &Path, name: &str) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    let path = dir.join(format!("{name}{}", firefox::FILE_SUFFIX));
    let file =
        File::create(&path).with_context(|| format!("couldn't create {}", path.display()))?;
    let mut buffered = BufWriter::new(file);
    firefox::write(profile, &mut buffered)?;
    buffered.flush()?;
    Ok(path)
}

/// Warns about anything that makes the profile less trustworthy. The summary output used to list
/// these.
fn report_diagnostics(d: &model::Diagnostics) {
    for message in &d.messages {
        warn!("{message}");
    }
    if d.events_lost > 0 || d.buffers_lost > 0 {
        warn!(
            "the trace lost {} events and {} buffers, some samples will be missing",
            d.events_lost, d.buffers_lost
        );
    }
    if d.unsupported_events > 0 {
        warn!("couldn't read {} events", d.unsupported_events);
    }
    debug!(
        "{} addresses were outside any image or JIT map entry",
        d.unknown_frames
    );
}

fn choose_process(raw: &trace::RawTrace, requested: Option<u32>) -> Result<u32> {
    let mut counts: FxHashMap<u32, u64> = FxHashMap::default();
    for sample in &raw.samples {
        *counts.entry(sample.pid).or_default() += 1;
    }
    if let Some(pid) = requested {
        return if counts.contains_key(&pid) {
            Ok(pid)
        } else {
            bail!("no samples exist for process {pid}")
        };
    }
    let busiest = counts.into_iter().max_by_key(|&(_, count)| count);
    busiest
        .map(|(pid, _)| pid)
        .context("the trace does not contain samples for any process, make sure to run the sampler with the correct profile")
}

fn default_symbol_cache() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").context(
        "could not resolve %LOCALAPPDATA%, please choose a symbol cache directory with --symbol-cache",
    )?;
    Ok(PathBuf::from(base).join("umprof").join("symbols"))
}

impl Args {
    fn resolve_map_dir(&self) -> PathBuf {
        self.map_dir.clone().unwrap_or_else(|| {
            self.trace
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .to_path_buf()
        })
    }

    fn resolve_candidate_pids(&self, map_dir: &Path) -> Result<FxHashSet<u32>> {
        let candidates: FxHashSet<u32> = match self.pid {
            Some(pid) => [pid].into_iter().collect(),
            None => jit_map::pids_in_dir(map_dir)?.into_iter().collect(),
        };

        if candidates.is_empty() {
            bail!(
                "no JIT maps found in {}, please make sure that the directory is correct, and launch Mono with JIT maps enabled",
                map_dir.display()
            );
        }
        Ok(candidates)
    }

    fn resolve_symbol_cache(&self) -> Result<PathBuf> {
        self.symbol_cache
            .clone()
            .map_or_else(default_symbol_cache, Ok)
    }
}
