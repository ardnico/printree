use anyhow::Result;
use ignore::{overrides::OverrideBuilder, WalkBuilder};
use std::io::Write;
use std::path::Path;
use termcolor::{Color, ColorSpec, StandardStream, WriteColor};

use crate::cli::Cli;
use crate::utils::{allow_type, build_globset, color_choice, match_globs};

pub fn run_tree_gitignore(cli: &Cli) -> Result<()> {
    let root = cli.path.clone().unwrap_or_else(|| ".".into());
    let root_path = Path::new(&root);
    let mut out = StandardStream::stdout(color_choice(cli.color));

    out.set_color(ColorSpec::new().set_bold(true))?;
    writeln!(&mut out, "{}", root_path.display())?;
    out.reset()?;

    let include_glob = build_globset(&cli.includes)?;
    let exclude_glob = build_globset(&cli.excludes)?;

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
                if !match_globs(root_path, path, &include_glob, &exclude_glob, cli.match_mode) {
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
