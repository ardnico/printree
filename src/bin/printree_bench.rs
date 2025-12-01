use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{ArgAction, Args, Parser, Subcommand};
use filetime::{set_file_times, FileTime};
use rand::prelude::*;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

#[cfg(all(unix, not(target_os = "macos")))]
use jemallocator::Jemalloc;

#[cfg(all(unix, not(target_os = "macos")))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[cfg(unix)]
type RusageSnapshot = libc::rusage;

#[cfg(not(unix))]
type RusageSnapshot = ();

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct IoSnapshot {
    read_syscalls: u64,
    write_syscalls: u64,
    read_bytes: u64,
    write_bytes: u64,
    read_chars: u64,
    write_chars: u64,
}

#[cfg(not(target_os = "linux"))]
#[derive(Clone, Copy)]
struct IoSnapshot;

#[cfg(all(unix, not(target_os = "macos")))]
#[derive(Clone, Copy)]
struct AllocationSnapshot {
    allocated: u64,
    active: u64,
    resident: u64,
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
#[derive(Clone, Copy)]
struct AllocationSnapshot;

#[derive(Parser, Debug)]
#[command(
    name = "printree-bench",
    about = "Synthetic tree generator and benchmark runner for printree v2"
)]
struct BenchCli {
    #[command(subcommand)]
    cmd: BenchCmd,
}

#[derive(Subcommand, Debug)]
enum BenchCmd {
    /// Generate a synthetic filesystem tree for performance benchmarks
    Gen(GenArgs),
    /// Run benchmark suites (stub interface)
    Run(RunArgs),
}

#[derive(Args, Debug)]
struct GenArgs {
    /// Number of files to create
    #[arg(long, default_value_t = 1_000_000)]
    files: usize,

    /// Maximum depth for generated directories (1 = only root)
    #[arg(long, default_value_t = 20)]
    depth: usize,

    /// Number of symlinks to create
    #[arg(long, default_value_t = 5_000)]
    symlinks: usize,

    /// Mix file sizes from 1 byte up to ~1GB using sparse files
    #[arg(long, action = ArgAction::SetTrue)]
    random_sizes: bool,

    /// Destination root for generated data
    #[arg(long)]
    root: Option<PathBuf>,

    /// Optional RNG seed for deterministic generation
    #[arg(long)]
    seed: Option<u64>,

    /// Remove any existing output directory first
    #[arg(long, action = ArgAction::SetTrue)]
    force: bool,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Benchmark case filter (e.g., "all" or a specific case name)
    #[arg(long, default_value = "all")]
    cases: String,

    /// Output path for JSON report
    #[arg(long)]
    out: Option<PathBuf>,

    /// Root directory containing the generated tree
    #[arg(long)]
    root: Option<PathBuf>,
}

#[derive(Serialize)]
struct BenchReport {
    status: String,
    root: String,
    timestamp: String,
    manifest: Option<GenerationManifest>,
    cases: Vec<CaseResult>,
}

#[derive(Serialize)]
struct CaseResult {
    name: String,
    status: String,
    wall_time_ms: u128,
    entries: usize,
    files: usize,
    dirs: usize,
    symlinks: usize,
    errors: usize,
    /// Count of traversal entries whose parent was not observed first.
    ordering_violations: usize,
    /// Count of I/O-backed walk errors (e.g., failed to open a path).
    open_failures: usize,
    note: Option<String>,
    resources: ResourceUsage,
}

#[derive(Serialize, Default)]
struct ResourceUsage {
    /// Delta of maximum resident set size in kilobytes, where supported.
    max_rss_kb: Option<i64>,
    /// Delta of minor page faults (no I/O), where supported.
    minor_faults: Option<i64>,
    /// Delta of major page faults (with I/O), where supported.
    major_faults: Option<i64>,
    /// Delta of input blocks (from block reads), where supported.
    in_block_ops: Option<i64>,
    /// Delta of output blocks (to block writes), where supported.
    out_block_ops: Option<i64>,
    /// Delta of voluntary context switches, where supported.
    voluntary_ctxt: Option<i64>,
    /// Delta of involuntary context switches, where supported.
    involuntary_ctxt: Option<i64>,
    /// Delta of read syscalls, where supported (Linux `/proc/self/io`).
    read_syscalls: Option<i64>,
    /// Delta of write syscalls, where supported (Linux `/proc/self/io`).
    write_syscalls: Option<i64>,
    /// Delta of bytes the kernel attempted to read from storage.
    read_bytes: Option<i64>,
    /// Delta of bytes the kernel attempted to write to storage.
    write_bytes: Option<i64>,
    /// Delta of raw read chars as reported by `/proc/self/io`.
    read_chars: Option<i64>,
    /// Delta of raw write chars as reported by `/proc/self/io`.
    write_chars: Option<i64>,
    /// Delta of jemalloc-reported allocated bytes (requires jemalloc allocator).
    allocated_bytes: Option<i64>,
    /// Delta of jemalloc-reported active bytes (requires jemalloc allocator).
    active_bytes: Option<i64>,
    /// Delta of jemalloc resident bytes (requires jemalloc allocator).
    resident_bytes: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug)]
struct GenerationManifest {
    files: usize,
    dirs: usize,
    symlinks: usize,
    depth: usize,
    random_sizes: bool,
    seed: Option<u64>,
    timestamp: String,
}

const MANIFEST_FILE: &str = "bench-manifest.json";

fn main() -> Result<()> {
    let cli = BenchCli::parse();
    match cli.cmd {
        BenchCmd::Gen(args) => run_gen(&args),
        BenchCmd::Run(args) => run_run(&args),
    }
}

fn run_gen(args: &GenArgs) -> Result<()> {
    if args.depth == 0 {
        bail!("depth must be at least 1");
    }

    let root = args
        .root
        .clone()
        .unwrap_or_else(|| PathBuf::from("./bench-data/gen"));

    if root.exists() {
        if args.force {
            fs::remove_dir_all(&root)
                .with_context(|| format!("removing existing root {}", root.display()))?;
        } else {
            bail!(
                "output root {} exists; re-run with --force to replace",
                root.display()
            );
        }
    }

    fs::create_dir_all(&root).with_context(|| format!("creating root {}", root.display()))?;

    let mut rng = match args.seed {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_entropy(),
    };

    let mut created_dirs: HashSet<PathBuf> = HashSet::new();
    created_dirs.insert(root.clone());
    let mut files: Vec<PathBuf> = Vec::with_capacity(args.files);

    for i in 0..args.files {
        let depth = if args.depth == 1 {
            0
        } else {
            rng.gen_range(0..args.depth)
        };
        let dir = ensure_dir_for_depth(&root, depth, &mut rng, &mut created_dirs)?;
        let file_name = random_file_name(&mut rng, i);
        let path = dir.join(file_name);
        let mut file =
            File::create(&path).with_context(|| format!("creating file {}", path.display()))?;

        if args.random_sizes {
            let size = rng.gen_range(1..=1_000_000_000u64);
            file.set_len(size)
                .with_context(|| format!("setting length for {}", path.display()))?;
        }

        apply_random_mtime(&path, &mut rng)?;
        files.push(path);
    }

    if args.symlinks > 0 {
        create_symlinks(&root, args.symlinks, &files, &mut rng, &mut created_dirs)?;
    }

    let manifest = GenerationManifest {
        files: args.files,
        dirs: created_dirs.len(),
        symlinks: args.symlinks,
        depth: args.depth,
        random_sizes: args.random_sizes,
        seed: args.seed,
        timestamp: Utc::now().to_rfc3339(),
    };

    write_manifest(&root, &manifest)?;
    println!(
        "generated {} files, {} dirs, {} symlinks at {} (random_sizes={}, seed={:?})",
        manifest.files,
        manifest.dirs,
        manifest.symlinks,
        root.display(),
        manifest.random_sizes,
        manifest.seed
    );
    Ok(())
}

fn run_run(args: &RunArgs) -> Result<()> {
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("bench.json"));
    let root = args
        .root
        .clone()
        .unwrap_or_else(|| PathBuf::from("./bench-data/gen"));
    if !root.exists() {
        bail!("benchmark root {} does not exist", root.display());
    }

    let manifest = match load_manifest(&root) {
        Ok(manifest) => manifest,
        Err(err) => {
            eprintln!("warning: failed to read generation manifest: {err}");
            None
        }
    };

    let case_names = parse_cases(&args.cases)?;
    let mut results = Vec::new();
    for name in case_names {
        match name.as_str() {
            "traversal" => results.push(run_traversal_case(&root)?),
            other => bail!("unsupported benchmark case: {}", other),
        }
    }

    let overall_status = if results.iter().all(|c| c.status == "ok") {
        "ok"
    } else {
        "partial"
    };
    let now = Utc::now().to_rfc3339();
    let report = BenchReport {
        status: overall_status.to_string(),
        root: root.display().to_string(),
        timestamp: now,
        manifest,
        cases: results,
    };

    let json = serde_json::to_string_pretty(&report)?;
    let mut file =
        File::create(&out).with_context(|| format!("creating report {}", out.display()))?;
    file.write_all(json.as_bytes())?;
    Ok(())
}

fn parse_cases(cases: &str) -> Result<Vec<String>> {
    if cases.trim() == "all" {
        return Ok(vec!["traversal".to_string()]);
    }

    let parsed: Vec<String> = cases
        .split(',')
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .map(|c| c.to_string())
        .collect();

    if parsed.is_empty() {
        bail!("no benchmark cases provided");
    }
    Ok(parsed)
}

fn run_traversal_case(root: &Path) -> Result<CaseResult> {
    let usage_before = take_rusage();
    let io_before = take_io_snapshot();
    let alloc_before = take_alloc_snapshot();
    let start = Instant::now();
    let mut entries = 0usize;
    let mut files = 0usize;
    let mut dirs = 0usize;
    let mut symlinks = 0usize;
    let mut errors = 0usize;
    let mut open_failures = 0usize;
    let mut ordering_violations = 0usize;
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    visited_dirs.insert(root.to_path_buf());

    for entry in WalkDir::new(root).follow_links(false) {
        match entry {
            Ok(e) => {
                let path = e.into_path();
                let parent = path.parent();
                if let Some(parent) = parent {
                    if !visited_dirs.contains(parent) {
                        ordering_violations += 1;
                    }
                }

                let ft = match path.symlink_metadata() {
                    Ok(meta) => meta.file_type(),
                    Err(err) => {
                        errors += 1;
                        eprintln!("metadata error for {}: {err}", path.display());
                        continue;
                    }
                };

                entries += 1;
                if ft.is_dir() {
                    dirs += 1;
                    visited_dirs.insert(path);
                } else if ft.is_symlink() {
                    symlinks += 1;
                } else {
                    files += 1;
                }
            }
            Err(err) => {
                errors += 1;
                if err.io_error().is_some() {
                    open_failures += 1;
                }
                eprintln!("walk error: {err}");
            }
        }
    }

    let wall_time = start.elapsed().as_millis();
    let usage_after = take_rusage();
    let io_after = take_io_snapshot();
    let alloc_after = take_alloc_snapshot();
    let resources = resource_usage_delta(
        usage_before,
        usage_after,
        io_before,
        io_after,
        alloc_before,
        alloc_after,
    );
    let status = if errors == 0 && ordering_violations == 0 {
        "ok"
    } else {
        "partial"
    };
    let note = match (errors, ordering_violations, open_failures) {
        (0, 0, 0) => None,
        _ => Some(format!(
            "errors={}, ordering_violations={}, open_failures={}",
            errors, ordering_violations, open_failures
        )),
    };

    Ok(CaseResult {
        name: "traversal".to_string(),
        status: status.to_string(),
        wall_time_ms: wall_time,
        entries,
        files,
        dirs,
        symlinks,
        errors,
        ordering_violations,
        open_failures,
        note,
        resources,
    })
}

#[cfg(unix)]
fn take_rusage() -> Option<RusageSnapshot> {
    use std::mem::MaybeUninit;

    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    let ret = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if ret == 0 {
        Some(unsafe { usage.assume_init() })
    } else {
        None
    }
}

#[cfg(not(unix))]
fn take_rusage() -> Option<RusageSnapshot> {
    None
}

#[cfg(unix)]
fn resource_usage_delta(
    start: Option<RusageSnapshot>,
    end: Option<RusageSnapshot>,
    io_start: Option<IoSnapshot>,
    io_end: Option<IoSnapshot>,
    alloc_start: Option<AllocationSnapshot>,
    alloc_end: Option<AllocationSnapshot>,
) -> ResourceUsage {
    fn delta<F>(start: &RusageSnapshot, end: &RusageSnapshot, f: F) -> i64
    where
        F: Fn(&RusageSnapshot) -> i64,
    {
        let s = f(start);
        let e = f(end);
        e.saturating_sub(s)
    }

    let base = match (start, end) {
        (Some(s), Some(e)) => ResourceUsage {
            max_rss_kb: Some(delta(&s, &e, |u| u.ru_maxrss as i64)),
            minor_faults: Some(delta(&s, &e, |u| u.ru_minflt as i64)),
            major_faults: Some(delta(&s, &e, |u| u.ru_majflt as i64)),
            in_block_ops: Some(delta(&s, &e, |u| u.ru_inblock as i64)),
            out_block_ops: Some(delta(&s, &e, |u| u.ru_oublock as i64)),
            voluntary_ctxt: Some(delta(&s, &e, |u| u.ru_nvcsw as i64)),
            involuntary_ctxt: Some(delta(&s, &e, |u| u.ru_nivcsw as i64)),
            ..ResourceUsage::default()
        },
        _ => ResourceUsage::default(),
    };

    let with_io = enrich_with_io(base, io_start, io_end);
    enrich_with_alloc(with_io, alloc_start, alloc_end)
}

#[cfg(not(unix))]
fn resource_usage_delta(
    _: Option<RusageSnapshot>,
    _: Option<RusageSnapshot>,
    _: Option<IoSnapshot>,
    _: Option<IoSnapshot>,
    _: Option<AllocationSnapshot>,
    _: Option<AllocationSnapshot>,
) -> ResourceUsage {
    ResourceUsage::default()
}

#[cfg(target_os = "linux")]
fn take_io_snapshot() -> Option<IoSnapshot> {
    use procfs::process::Process;

    let process = Process::myself().ok()?;
    let io = process.io().ok()?;
    Some(IoSnapshot {
        read_syscalls: io.syscr,
        write_syscalls: io.syscw,
        read_bytes: io.read_bytes,
        write_bytes: io.write_bytes,
        read_chars: io.rchar,
        write_chars: io.wchar,
    })
}

#[cfg(not(target_os = "linux"))]
fn take_io_snapshot() -> Option<IoSnapshot> {
    None
}

#[cfg(target_os = "linux")]
fn enrich_with_io(
    mut usage: ResourceUsage,
    start: Option<IoSnapshot>,
    end: Option<IoSnapshot>,
) -> ResourceUsage {
    match (start, end) {
        (Some(s), Some(e)) => {
            usage.read_syscalls = Some(e.read_syscalls.saturating_sub(s.read_syscalls) as i64);
            usage.write_syscalls = Some(e.write_syscalls.saturating_sub(s.write_syscalls) as i64);
            usage.read_bytes = Some(e.read_bytes.saturating_sub(s.read_bytes) as i64);
            usage.write_bytes = Some(e.write_bytes.saturating_sub(s.write_bytes) as i64);
            usage.read_chars = Some(e.read_chars.saturating_sub(s.read_chars) as i64);
            usage.write_chars = Some(e.write_chars.saturating_sub(s.write_chars) as i64);
            usage
        }
        _ => usage,
    }
}

#[cfg(not(target_os = "linux"))]
fn enrich_with_io(
    usage: ResourceUsage,
    _: Option<IoSnapshot>,
    _: Option<IoSnapshot>,
) -> ResourceUsage {
    usage
}

#[cfg(all(unix, not(target_os = "macos")))]
fn take_alloc_snapshot() -> Option<AllocationSnapshot> {
    use jemalloc_ctl::stats::{active, allocated, resident};

    Some(AllocationSnapshot {
        allocated: allocated::read().ok()?,
        active: active::read().ok()?,
        resident: resident::read().ok()?,
    })
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn take_alloc_snapshot() -> Option<AllocationSnapshot> {
    None
}

#[cfg(all(unix, not(target_os = "macos")))]
fn enrich_with_alloc(
    mut usage: ResourceUsage,
    start: Option<AllocationSnapshot>,
    end: Option<AllocationSnapshot>,
) -> ResourceUsage {
    match (start, end) {
        (Some(s), Some(e)) => {
            usage.allocated_bytes = Some(e.allocated.saturating_sub(s.allocated) as i64);
            usage.active_bytes = Some(e.active.saturating_sub(s.active) as i64);
            usage.resident_bytes = Some(e.resident.saturating_sub(s.resident) as i64);
            usage
        }
        _ => usage,
    }
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn enrich_with_alloc(
    usage: ResourceUsage,
    _: Option<AllocationSnapshot>,
    _: Option<AllocationSnapshot>,
) -> ResourceUsage {
    usage
}

fn ensure_dir_for_depth(
    root: &Path,
    depth: usize,
    rng: &mut StdRng,
    created: &mut HashSet<PathBuf>,
) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for level in 0..depth {
        let hidden = rng.gen_bool(0.1);
        let segment = if hidden {
            format!(".d{}-{}", level, rng.gen_range(0..10_000))
        } else {
            format!("d{}-{}", level, rng.gen_range(0..10_000))
        };
        path.push(segment);
    }

    if created.insert(path.clone()) {
        fs::create_dir_all(&path)
            .with_context(|| format!("creating directory {}", path.display()))?;
    }
    Ok(path)
}

fn random_file_name(rng: &mut StdRng, index: usize) -> String {
    let hidden = rng.gen_bool(0.15);
    if hidden {
        format!(".file-{}", index)
    } else {
        format!("file-{}", index)
    }
}

fn apply_random_mtime(path: &Path, rng: &mut StdRng) -> Result<()> {
    let now = SystemTime::now();
    let max_age = Duration::from_secs(365 * 24 * 60 * 60);
    let age = rng.gen_range(0..=max_age.as_secs());
    let mtime = now.checked_sub(Duration::from_secs(age)).unwrap_or(now);
    let mtime_ft = FileTime::from_system_time(mtime);
    set_file_times(path, mtime_ft, mtime_ft)
        .with_context(|| format!("setting mtime for {}", path.display()))?;
    Ok(())
}

fn write_manifest(root: &Path, manifest: &GenerationManifest) -> Result<()> {
    let manifest_path = root.join(MANIFEST_FILE);
    let json = serde_json::to_string_pretty(manifest)?;
    fs::write(&manifest_path, json)
        .with_context(|| format!("writing manifest {}", manifest_path.display()))?;
    Ok(())
}

fn load_manifest(root: &Path) -> Result<Option<GenerationManifest>> {
    let manifest_path = root.join(MANIFEST_FILE);
    if !manifest_path.exists() {
        return Ok(None);
    }

    let data = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading manifest {}", manifest_path.display()))?;
    let manifest = serde_json::from_str(&data)
        .with_context(|| format!("parsing manifest {}", manifest_path.display()))?;
    Ok(Some(manifest))
}

fn create_symlinks(
    root: &Path,
    count: usize,
    targets: &[PathBuf],
    rng: &mut StdRng,
    created_dirs: &mut HashSet<PathBuf>,
) -> Result<()> {
    if targets.is_empty() {
        return Ok(());
    }
    for i in 0..count {
        let target = &targets[rng.gen_range(0..targets.len())];
        let link_dir = ensure_dir_for_depth(root, rng.gen_range(0..=3), rng, &mut created_dirs)?;
        let link_name = format!("symlink-{}", i);
        let link_path = link_dir.join(link_name);
        create_symlink(target, &link_path).with_context(|| {
            format!(
                "creating symlink {} -> {}",
                link_path.display(),
                target.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    let meta = fs::metadata(target)?;
    if meta.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}
