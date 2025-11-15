use anyhow::Result;
use ignore::{overrides::OverrideBuilder, WalkBuilder};
use serde::Serialize;
use std::io::Write;
use std::path::Path;
use termcolor::{Color, ColorSpec, StandardStream, WriteColor};

use crate::cli::{Cli, Format};
use crate::utils::{allow_type, build_patterns, color_choice, match_globs};

#[derive(Serialize)]
struct JsonEntry<'a> {
    path: &'a str,
    name: &'a str,
    depth: usize,
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

pub fn run_tree_gitignore(cli: &Cli) -> Result<()> {
    if cli.format == Format::Json {
        run_tree_gitignore_json(cli)
    } else {
        run_tree_gitignore_plain(cli)
    }
}

fn run_tree_gitignore_plain(cli: &Cli) -> Result<()> {
    let root = cli.path.clone().unwrap_or_else(|| ".".into());
    let root_path = Path::new(&root);
    let mut out = StandardStream::stdout(color_choice(cli.color));

    out.set_color(ColorSpec::new().set_bold(true))?;
    writeln!(&mut out, "{}", root_path.display())?;
    out.reset()?;

    let include_glob = build_patterns(&cli.includes, cli.pattern_syntax, true)?;
    let exclude_glob = build_patterns(&cli.excludes, cli.pattern_syntax, false)?;

    let mut ov = OverrideBuilder::new(&root);
    for exc in &cli.excludes {
        ov.add(exc).ok();
    }
    for inc in &cli.includes {
        ov.add(&format!("!{}", inc)).ok();
    }
    let overrides = ov.build().ok();

    let mut wb = WalkBuilder::new(&root);
    wb.hidden(!cli.hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(cli.follow_symlinks)
        .max_depth(cli.max_depth)
        .standard_filters(false);
    if let Some(o) = overrides {
        wb.overrides(o);
    }

    for dent in wb.build() {
        match dent {
            Ok(d) => {
                let path = d.path();
                if path == root_path {
                    continue;
                }
                let depth = d.depth();

                if let Some(ft) = d.file_type() {
                    if !allow_type(&ft, &cli.types) {
                        continue;
                    }
                }
                if !match_globs(
                    root_path,
                    path,
                    &include_glob,
                    &exclude_glob,
                    cli.match_mode,
                ) {
                    continue;
                }

                for _ in 0..depth {
                    write!(&mut out, "    ")?;
                }
                if let Some(ft) = d.file_type() {
                    if ft.is_dir() {
                        out.set_color(ColorSpec::new().set_fg(Some(Color::Blue)))?;
                    } else if ft.is_symlink() {
                        out.set_color(ColorSpec::new().set_fg(Some(Color::Cyan)))?;
                    }
                }
                writeln!(
                    &mut out,
                    "{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )?;
                out.reset()?;
            }
            Err(e) => {
                writeln!(&mut out, "[error] {e}")?;
            }
        }
    }

    Ok(())
}

fn run_tree_gitignore_json(cli: &Cli) -> Result<()> {
    let root = cli.path.clone().unwrap_or_else(|| ".".into());
    let root_path = Path::new(&root);
    let include_glob = build_patterns(&cli.includes, cli.pattern_syntax, true)?;
    let exclude_glob = build_patterns(&cli.excludes, cli.pattern_syntax, false)?;
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());

    // ルート
    let root_s = root_path.display().to_string();
    serde_json::to_writer(
        &mut stdout,
        &JsonEntry {
            path: &root_s,
            name: ".",
            depth: 0,
            kind: "dir",
            error: None,
        },
    )?;
    writeln!(&mut stdout)?;

    let mut ov = OverrideBuilder::new(&root);
    for exc in &cli.excludes {
        ov.add(exc).ok();
    }
    for inc in &cli.includes {
        ov.add(&format!("!{}", inc)).ok();
    }
    let overrides = ov.build().ok();

    let mut wb = WalkBuilder::new(&root);
    wb.hidden(!cli.hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(cli.follow_symlinks)
        .max_depth(cli.max_depth)
        .standard_filters(false);
    if let Some(o) = overrides {
        wb.overrides(o);
    }

    for dent in wb.build() {
        match dent {
            Ok(d) => {
                let path = d.path();
                if path == root_path {
                    continue;
                }
                let depth = d.depth();

                if let Some(ft) = d.file_type() {
                    if !allow_type(&ft, &cli.types) {
                        continue;
                    }
                }
                if !match_globs(
                    root_path,
                    path,
                    &include_glob,
                    &exclude_glob,
                    cli.match_mode,
                ) {
                    continue;
                }

                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let path_s = path.display().to_string();
                let kind = match d.file_type() {
                    Some(ft) if ft.is_dir() => "dir",
                    Some(ft) if ft.is_symlink() => "symlink",
                    Some(_) => "file",
                    None => "unknown",
                };
                serde_json::to_writer(
                    &mut stdout,
                    &JsonEntry {
                        path: &path_s,
                        name: &name,
                        depth,
                        kind,
                        error: None,
                    },
                )?;
                writeln!(&mut stdout)?;
            }
            Err(e) => {
                let msg = e.to_string();
                serde_json::to_writer(
                    &mut stdout,
                    &JsonEntry {
                        path: "",
                        name: "",
                        depth: 0,
                        kind: "unknown",
                        error: Some(&msg),
                    },
                )?;
                writeln!(&mut stdout)?;
            }
        }
    }

    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, File};
    use tempfile::tempdir;

    #[test]
    fn gitignore_json_runs() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        create_dir_all(root.join("x")).unwrap();
        File::create(root.join("x/y.txt")).unwrap();

        let cli = crate::cli::Cli {
            cmd: None,
            path: Some(root.to_path_buf()),
            max_depth: None,
            hidden: true,
            follow_symlinks: false,
            sort: crate::cli::SortMode::Name,
            dirs_first: true,
            includes: vec![],
            excludes: vec![],
            pattern_syntax: crate::cli::PatternSyntax::Glob,
            match_mode: crate::cli::MatchMode::Path,
            filter_regex: None,
            filter_size: None,
            filter_mtime: None,
            filter_perm: None,
            types: vec![],
            gitignore: crate::cli::GitignoreMode::On,
            git_status: false,
            git_rename: false,
            color: crate::cli::ColorMode::Never,
            format: crate::cli::Format::Json,
            encoding: crate::cli::EncodingMode::Utf8,
            jobs: 1,
            warn_depth: 5000,
        };

        run_tree_gitignore(&cli).unwrap();
    }
}
