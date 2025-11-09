use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use termcolor::{Color, ColorSpec, StandardStream, WriteColor};

use crate::cli::{Cli, Format, SortMode};
use crate::utils::{allow_type, build_globset, color_choice, is_hidden, match_globs, Frame};

#[derive(Serialize)]
struct JsonEntry<'a> {
    path: &'a str,
    name: &'a str,
    depth: usize,
    kind: &'a str,            // "dir" | "file" | "symlink"
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,   // 権限など
}

pub fn run_tree(cli: &Cli) -> Result<()> {
    let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let include_glob = build_globset(&cli.includes)?;
    let exclude_glob = build_globset(&cli.excludes)?;

    if cli.format == Format::Json {
        // JSONはカラー無し・NDJSON
        run_tree_json(&root, cli, &include_glob, &exclude_glob)
    } else {
        run_tree_plain(&root, cli, &include_glob, &exclude_glob)
    }
}

fn run_tree_plain(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<globset::GlobSet>,
    exclude_glob: &Option<globset::GlobSet>,
) -> Result<()> {
    let mut out = StandardStream::stdout(color_choice(cli.color));
    out.set_color(ColorSpec::new().set_bold(true))?;
    writeln!(&mut out, "{}", root.display())?;
    out.reset()?;

    let root_ft = fs::symlink_metadata(root).ok().map(|m| m.file_type());
    if root_ft.map_or(false, |ft| !ft.is_dir()) || matches!(cli.max_depth, Some(1)) {
        return Ok(());
    }

    let mut stack: Vec<Frame> = Vec::new();
    if let Some(frame) = read_dir_frame(root, "", 1, cli, include_glob, exclude_glob)? {
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
                if top.depth + 1 > maxd { continue; }
            }
            if let Some(frame) =
                read_dir_frame(&entry.path(), &child_prefix, top.depth + 1, cli, include_glob, exclude_glob)?
            {
                stack.push(frame);
            }
        }
    }

    Ok(())
}

fn run_tree_json(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<globset::GlobSet>,
    exclude_glob: &Option<globset::GlobSet>,
) -> Result<()> {
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());

    // ルートも1行出す
    let root_display = root.display().to_string();
    let root_name = root.file_name().and_then(|s| s.to_str()).unwrap_or(".");
    serde_json::to_writer(
        &mut stdout,
        &JsonEntry { path: &root_display, name: root_name, depth: 0, kind: "dir", error: None },
    )?;
    writeln!(&mut stdout)?;

    let root_ft = fs::symlink_metadata(root).ok().map(|m| m.file_type());
    if root_ft.map_or(false, |ft| !ft.is_dir()) || matches!(cli.max_depth, Some(1)) {
        stdout.flush()?;
        return Ok(());
    }

    let mut stack: Vec<Frame> = Vec::new();
    if let Some(frame) = read_dir_frame(root, "", 1, cli, include_glob, exclude_glob)? {
        stack.push(frame);
    }

    while let Some(top) = stack.last_mut() {
        if top.idx >= top.entries.len() {
            stack.pop();
            continue;
        }
        let entry = &top.entries[top.idx];
        top.idx += 1;

        let name_os = entry.file_name();
        let name = name_os.to_string_lossy();
        let path = entry.path();
        let path_s = path.display().to_string();

        match entry.file_type() {
            Ok(ft) => {
                let kind = if ft.is_dir() { "dir" } else if ft.is_symlink() { "symlink" } else { "file" };
                serde_json::to_writer(
                    &mut stdout,
                    &JsonEntry { path: &path_s, name: &name, depth: top.depth, kind, error: None },
                )?;
                writeln!(&mut stdout)?;
                // 降下
                let mut descend = ft.is_dir();
                if !descend && ft.is_symlink() && cli.follow_symlinks {
                    if let Ok(m) = fs::metadata(&path) { descend = m.is_dir(); }
                }
                if descend {
                    if let Some(maxd) = cli.max_depth {
                        if top.depth + 1 > maxd { continue; }
                    }
                    if let Some(frame) = read_dir_frame(&path, "", top.depth + 1, cli, include_glob, exclude_glob)? {
                        stack.push(frame);
                    }
                }
            }
            Err(e) => {
                serde_json::to_writer(
                    &mut stdout,
                    &JsonEntry { path: &path_s, name: &name, depth: top.depth, kind: "unknown", error: Some(&e.to_string()) },
                )?;
                writeln!(&mut stdout)?;
            }
        }
    }

    stdout.flush()?;
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
                if !cli.hidden && is_hidden(&name) { continue; }
                if let Ok(ft) = de.file_type() {
                    if !allow_type(&ft, &cli.types) { continue; }
                }
                let fullp = de.path();
                if !match_globs(path, &fullp, include_glob, exclude_glob, cli.match_mode) { continue; }
                entries.push(de);
            }
            Err(err) => { eprintln!("[read_dir error] {}: {err}", path.display()); }
        }
    }

    if entries.is_empty() { return Ok(None); }

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

    Ok(Some(Frame { path: path.to_path_buf(), entries, idx: 0, prefix: prefix.to_string(), depth }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs::{create_dir_all, File};

    #[test]
    fn json_traversal_basic() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        create_dir_all(root.join("a/sub")).unwrap();
        File::create(root.join("a/file.txt")).unwrap();

        let cli = Cli {
            cmd: None,
            path: Some(root.to_path_buf()),
            max_depth: None,
            hidden: true,
            follow_symlinks: false,
            sort: SortMode::Name,
            dirs_first: true,
            includes: vec![],
            excludes: vec![],
            match_mode: crate::cli::MatchMode::Path,
            types: vec![],
            gitignore: crate::cli::GitignoreMode::Off,
            color: crate::cli::ColorMode::Never,
            format: Format::Json,
        };

        // 例外が出ず走り切ることを確認
        run_tree(&cli).unwrap();
    }
}
