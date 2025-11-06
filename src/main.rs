use anyhow::{Context, Result};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

/// 内部で使うディレクトリ走査スタック用構造体
#[derive(Debug)]
struct Frame {
    path: PathBuf,
    entries: Vec<fs::DirEntry>,
    idx: usize,
    prefix: String,
    depth: usize,
}

#[derive(Parser, Debug)]
#[command(version, about = "Fast, memory-light directory tree & git diff printer")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    path: Option<PathBuf>,

    #[arg(long)]
    max_depth: Option<usize>,

    #[arg(long, action = ArgAction::SetTrue)]
    hidden: bool,

    #[arg(long, action = ArgAction::SetTrue)]
    follow_symlinks: bool,

    #[arg(long, value_enum, default_value_t = SortMode::None)]
    sort: SortMode,

    #[arg(long, action = ArgAction::SetTrue)]
    dirs_first: bool,

    #[arg(long = "include")]
    includes: Vec<String>,

    #[arg(long = "exclude")]
    excludes: Vec<String>,

    #[arg(long, value_enum, default_value_t = MatchMode::Name)]
    match_mode: MatchMode,

    #[arg(long = "type", value_enum)]
    types: Vec<TypeFilter>,

    #[arg(long, value_enum, default_value_t = GitignoreMode::Off)]
    gitignore: GitignoreMode,

    #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
    color: ColorMode,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    Diff {
        #[arg(long = "rev-a")]
        rev_a: String,
        #[arg(long = "rev-b")]
        rev_b: String,
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum SortMode {
    None,
    Name,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum MatchMode {
    Name,
    Path,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
enum TypeFilter {
    File,
    Dir,
    Symlink,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum GitignoreMode {
    On,
    Off,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ColorMode {
    Auto,
    Always,
    Never,
}

fn is_hidden(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn build_globset(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p).with_context(|| format!("invalid glob: {p}"))?);
    }
    Ok(Some(builder.build()?))
}

fn allow_type(ty: &fs::FileType, types: &[TypeFilter]) -> bool {
    if types.is_empty() {
        return true;
    }
    let is_dir = ty.is_dir();
    let is_symlink = ty.is_symlink();
    let is_file = !is_dir && !is_symlink;
    types.iter().any(|t| match t {
        TypeFilter::File => is_file,
        TypeFilter::Dir => is_dir,
        TypeFilter::Symlink => is_symlink,
    })
}

fn color_choice(mode: ColorMode) -> ColorChoice {
    match mode {
        ColorMode::Auto => ColorChoice::Auto,
        ColorMode::Always => ColorChoice::Always,
        ColorMode::Never => ColorChoice::Never,
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Cmd::Diff { rev_a, rev_b, path }) = &cli.cmd {
        return run_diff(rev_a, rev_b, path.as_deref());
    }
    run_tree(&cli)
}

fn run_tree(cli: &Cli) -> Result<()> {
    let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut out = StandardStream::stdout(color_choice(cli.color));

    out.set_color(ColorSpec::new().set_bold(true))?;
    writeln!(&mut out, "{}", root.display())?;
    out.reset()?;

    let root_ft = fs::symlink_metadata(&root).ok().map(|m| m.file_type());
    if root_ft.map_or(false, |ft| !ft.is_dir()) || matches!(cli.max_depth, Some(1)) {
        return Ok(());
    }

    let include_glob = build_globset(&cli.includes)?;
    let exclude_glob = build_globset(&cli.excludes)?;

    let mut stack: Vec<Frame> = Vec::new();

    if let Some(frame) = read_dir_frame(&root, "", 1, cli, &include_glob, &exclude_glob)? {
        stack.push(frame);
    }

    while let Some(top) = stack.last_mut() {
        if top.idx >= top.entries.len() {
            stack.pop();
            continue;
        }
        let is_last = top.idx + 1 == top.entries.len();
        let entry = &top.entries[top.idx];
        top.idx += 1;

        let name = entry.file_name();
        let connector = if is_last { "└── " } else { "├── " };

        let ty = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                writeln!(
                    &mut out,
                    "{}{}{}  [permission denied: {}]",
                    top.prefix,
                    connector,
                    name.to_string_lossy(),
                    e
                )?;
                continue;
            }
        };

        if ty.is_dir() {
            out.set_color(ColorSpec::new().set_fg(Some(Color::Blue)))?;
        } else if ty.is_symlink() {
            out.set_color(ColorSpec::new().set_fg(Some(Color::Cyan)))?;
        }
        write!(&mut out, "{}{}", top.prefix, connector)?;
        out.reset()?;
        writeln!(&mut out, "{}", name.to_string_lossy())?;

        let child_prefix = if is_last {
            format!("{}    ", top.prefix)
        } else {
            format!("{}│   ", top.prefix)
        };

        let mut descend = ty.is_dir();
        if !descend && ty.is_symlink() && cli.follow_symlinks {
            if let Ok(m) = fs::metadata(entry.path()) {
                descend = m.is_dir();
            }
        }

        if descend {
            if let Some(maxd) = cli.max_depth {
                if top.depth + 1 > maxd {
                    continue;
                }
            }
            if let Some(frame) =
                read_dir_frame(&entry.path(), &child_prefix, top.depth + 1, cli, &include_glob, &exclude_glob)?
            {
                stack.push(frame);
            }
        }
    }

    Ok(())
}

fn read_dir_frame(
    path: &Path,
    prefix: &str,
    depth: usize,
    cli: &Cli,
    include_glob: &Option<GlobSet>,
    exclude_glob: &Option<GlobSet>,
) -> Result<Option<Frame>> {
    let rd = match fs::read_dir(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} [permission denied: {}]", path.display(), e);
            return Ok(None);
        }
    };

    let mut entries = Vec::new();
    for e in rd {
        match e {
            Ok(de) => {
                let name = de.file_name();
                if !cli.hidden && is_hidden(&name) {
                    continue;
                }
                if let Ok(ft) = de.file_type() {
                    if !allow_type(&ft, &cli.types) {
                        continue;
                    }
                }
                let fullp = de.path();
                if !match_globs(path, &fullp, include_glob, exclude_glob, cli.match_mode) {
                    continue;
                }
                entries.push(de);
            }
            Err(err) => {
                eprintln!("[read_dir error] {}: {err}", path.display());
            }
        }
    }

    if entries.is_empty() {
        return Ok(None);
    }

    if matches!(cli.sort, SortMode::Name) {
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    }
    if cli.dirs_first {
        entries.sort_by(|a, b| {
            let ad = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let bd = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            bd.cmp(&ad).then_with(|| a.file_name().cmp(&b.file_name()))
        });
    }

    Ok(Some(Frame {
        path: path.to_path_buf(),
        entries,
        idx: 0,
        prefix: prefix.to_string(),
        depth,
    }))
}

fn match_globs(
    root: &Path,
    path: &Path,
    include_glob: &Option<GlobSet>,
    exclude_glob: &Option<GlobSet>,
    mode: MatchMode,
) -> bool {
    if let Some(gs) = include_glob {
        let target = match mode {
            MatchMode::Name => path.file_name().unwrap_or_default(),
            MatchMode::Path => path.strip_prefix(root).unwrap_or(path).as_os_str(),
        };
        if !gs.is_match(target) {
            return false;
        }
    }
    if let Some(gs) = exclude_glob {
        let target = match mode {
            MatchMode::Name => path.file_name().unwrap_or_default(),
            MatchMode::Path => path.strip_prefix(root).unwrap_or(path).as_os_str(),
        };
        if gs.is_match(target) {
            return false;
        }
    }
    true
}

fn run_diff(rev_a: &str, rev_b: &str, subpath: Option<&Path>) -> Result<()> {
    use git2::{DiffFormat, Repository};

    let repo = Repository::discover(".").context("not a git repository")?;
    let obj_a = repo.revparse_single(rev_a)?;
    let obj_b = repo.revparse_single(rev_b)?;
    let tree_a = obj_a.peel_to_tree()?;
    let tree_b = obj_b.peel_to_tree()?;

    let mut opts = git2::DiffOptions::new();
    if let Some(sp) = subpath {
        opts.pathspec(sp);
    }

    let diff = repo.diff_tree_to_tree(Some(&tree_a), Some(&tree_b), Some(&mut opts))?;
    let mut out = StandardStream::stdout(ColorChoice::Auto);

    writeln!(&mut out, "diff {} .. {}", rev_a, rev_b)?;
    diff.print(DiffFormat::NameStatus, |_delta, _hunk, line| {
        let content = std::str::from_utf8(line.content()).unwrap_or("");
        write!(&mut out, "{}", content).unwrap();
        true
    })?;
    Ok(())
}
