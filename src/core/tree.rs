use anyhow::Result;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use termcolor::{Color, ColorSpec, StandardStream, WriteColor};

use crate::cli::{Cli, SortMode};
use crate::utils::{allow_type, build_globset, color_choice, is_hidden, match_globs, Frame};

pub fn run_tree(cli: &Cli) -> Result<()> {
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
                    top.prefix, connector, name.to_string_lossy(), e
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
    include_glob: &Option<globset::GlobSet>,
    exclude_glob: &Option<globset::GlobSet>,
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
