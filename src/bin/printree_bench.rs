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
use serde::Serialize;
use walkdir::WalkDir;

#[cfg(unix)]
type RusageSnapshot = libc::rusage;

#[cfg(not(unix))]
type RusageSnapshot = ();

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
}

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
        create_symlinks(&root, args.symlinks, &files, &mut rng)?;
    }

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
    let start = Instant::now();
    let mut entries = 0usize;
    let mut files = 0usize;
    let mut dirs = 0usize;
    let mut symlinks = 0usize;
    let mut errors = 0usize;

    for entry in WalkDir::new(root).follow_links(false) {
        match entry {
            Ok(e) => {
                entries += 1;
                let ft = e.file_type();
                if ft.is_dir() {
                    dirs += 1;
                } else if ft.is_symlink() {
                    symlinks += 1;
                } else {
                    files += 1;
                }
            }
            Err(err) => {
                errors += 1;
                eprintln!("walk error: {err}");
            }
        }
    }

    let wall_time = start.elapsed().as_millis();
    let usage_after = take_rusage();
    let resources = resource_usage_delta(usage_before, usage_after);
    let status = if errors == 0 { "ok" } else { "partial" };
    let note = if errors == 0 {
        None
    } else {
        Some(format!("encountered {} traversal errors", errors))
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
) -> ResourceUsage {
    fn delta<F>(start: &RusageSnapshot, end: &RusageSnapshot, f: F) -> i64
    where
        F: Fn(&RusageSnapshot) -> i64,
    {
        let s = f(start);
        let e = f(end);
        e.saturating_sub(s)
    }

    match (start, end) {
        (Some(s), Some(e)) => ResourceUsage {
            max_rss_kb: Some(delta(&s, &e, |u| u.ru_maxrss as i64)),
            minor_faults: Some(delta(&s, &e, |u| u.ru_minflt as i64)),
            major_faults: Some(delta(&s, &e, |u| u.ru_majflt as i64)),
            in_block_ops: Some(delta(&s, &e, |u| u.ru_inblock as i64)),
            out_block_ops: Some(delta(&s, &e, |u| u.ru_oublock as i64)),
            voluntary_ctxt: Some(delta(&s, &e, |u| u.ru_nvcsw as i64)),
            involuntary_ctxt: Some(delta(&s, &e, |u| u.ru_nivcsw as i64)),
        },
        _ => ResourceUsage::default(),
    }
}

#[cfg(not(unix))]
fn resource_usage_delta(_: Option<RusageSnapshot>, _: Option<RusageSnapshot>) -> ResourceUsage {
    ResourceUsage::default()
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

fn create_symlinks(root: &Path, count: usize, targets: &[PathBuf], rng: &mut StdRng) -> Result<()> {
    if targets.is_empty() {
        return Ok(());
    }

    let mut created_dirs: HashSet<PathBuf> = HashSet::new();
    created_dirs.insert(root.to_path_buf());
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
