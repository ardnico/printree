use clap::{ArgAction, Parser, ValueEnum};
use std::ffi::OsStr;
use std::fs::{self, DirEntry, ReadDir};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Fast, memory-light directory tree printer (std-only)
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Root path to traverse
    path: Option<PathBuf>,

    /// Max depth (1 = only root)
    #[arg(long, value_name = "N")]
    max_depth: Option<usize>,

    /// Show dotfiles (hidden on Unix: names starting with '.')
    #[arg(long, action = ArgAction::SetTrue)]
    hidden: bool,

    /// Follow symlinks (disabled by default to avoid cycles)
    #[arg(long, action = ArgAction::SetTrue)]
    follow_symlinks: bool,

    /// Sort mode (none is fastest)
    #[arg(long, value_enum, default_value_t = SortMode::None)]
    sort: SortMode,

    /// List directories before files when sorting
    #[arg(long, action = ArgAction::SetTrue)]
    dirs_first: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum SortMode {
    None,
    Name,
}

fn is_hidden(name: &OsStr) -> bool {
    // Simple rule: names starting with '.' (Unix). On Windows, this does not check attributes.
    // 速度優先のため、追加のメタデータ呼び出しは避ける。
    let s = name.to_string_lossy();
    s.starts_with('.')
}

#[derive(Debug)]
struct Frame {
    path: PathBuf,
    entries: Vec<DirEntry>,
    idx: usize,
    prefix: String, // accumulated visual prefix like "│   "
    depth: usize,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut out = BufWriter::new(io::stdout().lock());

    // Print root line
    writeln!(out, "{}", root.display())?;
    out.flush()?;

    // Early exit for files or when max_depth == Some(1)
    let root_ft = fs::symlink_metadata(&root).ok().map(|m| m.file_type());
    if root_ft.map_or(false, |ft| !ft.is_dir()) || matches!(cli.max_depth, Some(1)) {
        return Ok(());
    }

    // Build initial entries for root
    let mut stack: Vec<Frame> = Vec::with_capacity(32);

    if let Some(frame) = read_dir_frame(&root, "", 1, &cli) {
        stack.push(frame);
    }

    // Iterative DFS using manual stack
    while let Some(top) = stack.last_mut() {
        if top.idx >= top.entries.len() {
            // Directory done; pop
            stack.pop();
            continue;
        }

        let is_last = top.idx + 1 == top.entries.len();
        let entry = &top.entries[top.idx];
        top.idx += 1;

        let name = entry.file_name();
        let connector = if is_last { "└── " } else { "├── " };
        writeln!(out, "{}{}{}", top.prefix, connector, name.to_string_lossy())?;

        // Decide next prefix for children
        let child_prefix = if is_last {
            format!("{}    ", top.prefix)
        } else {
            format!("{}│   ", top.prefix)
        };

        // Minimal file_type() call; avoid full metadata unless necessary
        let ty = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue, // unreadable, skip quietly
        };

        // Optionally follow symlinks (may create cycles; we don't cycle-detect for速度)
        let is_symlink = ty.is_symlink();
        let should_descend = if ty.is_dir() {
            true
        } else if is_symlink && cli.follow_symlinks {
            // Resolve and check if target is a directory
            // Avoid metadata() where possible, but here we must know
            match fs::metadata(entry.path()) {
                Ok(m) => m.is_dir(),
                Err(_) => false,
            }
        } else {
            false
        };

        if should_descend {
            if let Some(maxd) = cli.max_depth {
                if top.depth + 1 > maxd {
                    continue;
                }
            }
            if let Some(frame) = read_dir_frame(&entry.path(), &child_prefix, top.depth + 1, &cli) {
                stack.push(frame);
            }
        }
    }

    out.flush()
}

/// Read a directory and return a prepared Frame (entries collected once).
/// Returns None if path is not a readable directory or empty after filters.
fn read_dir_frame(path: &Path, prefix: &str, depth: usize, cli: &Cli) -> Option<Frame> {
    let rd: ReadDir = match fs::read_dir(path) {
        Ok(r) => r,
        Err(_) => return None,
    };

    // Collect entries for this dir *only*; we don't hold global state.
    // Avoid metadata unless required. We will:
    //  - Filter hidden if requested by name only
    //  - Optionally sort by name (and dirs-first if requested), which requires file_type()
    let mut entries: Vec<DirEntry> = Vec::with_capacity(64);
    for e in rd {
        if let Ok(de) = e {
            if !cli.hidden && is_hidden(&de.file_name()) {
                continue;
            }
            entries.push(de);
        }
    }
    if entries.is_empty() {
        return None;
    }

    match cli.sort {
        SortMode::None => {
            // Keep OS enumeration order (fastest)
            if cli.dirs_first {
                // Need one pass to stable-partition by directory flag (requires file_type())
                stable_dirs_first(&mut entries);
            }
        }
        SortMode::Name => {
            // Optionally dirs-first + name sort; file_type() cost is per-entry only here
            entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
            if cli.dirs_first {
                stable_dirs_first(&mut entries);
            }
        }
    }

    Some(Frame {
        path: path.to_path_buf(),
        entries,
        idx: 0,
        prefix: prefix.to_string(),
        depth,
    })
}

/// Stable partition: directories before files (and symlinks to dirs before others)
fn stable_dirs_first(entries: &mut [DirEntry]) {
    // We’ll do a stable sort by a cheap key: !(is_dir_or_dir_symlink)
    entries.sort_by(|a, b| {
        let ad = is_dirish(a);
        let bd = is_dirish(b);
        bd.cmp(&ad) // true before false
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });
}

fn is_dirish(e: &DirEntry) -> bool {
    match e.file_type() {
        Ok(ft) if ft.is_dir() => true,
        Ok(ft) if ft.is_symlink() => {
            // Resolve minimal: only check if symlink target is dir (costly but only for sort)
            fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false)
        }
        _ => false,
    }
}
