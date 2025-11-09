use anyhow::{Context, Result};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

use crate::cli::Format;

#[derive(Serialize)]
struct JsonDiff<'a> {
    status: &'a str, // added | deleted | modified | renamed | copied | typechange | unknown
    path: &'a str,
}

pub fn run_diff(rev_a: &str, rev_b: &str, subpath: Option<&Path>, format: Format) -> Result<()> {
    use git2::{Delta, DiffOptions, Repository};

    let repo = Repository::discover(".").context("not a git repository")?;
    let obj_a = repo.revparse_single(rev_a)?;
    let obj_b = repo.revparse_single(rev_b)?;
    let tree_a = obj_a.peel_to_tree()?;
    let tree_b = obj_b.peel_to_tree()?;

    let mut opts = DiffOptions::new();
    if let Some(sp) = subpath {
        opts.pathspec(sp);
    }
    let diff = repo.diff_tree_to_tree(Some(&tree_a), Some(&tree_b), Some(&mut opts))?;

    match format {
        Format::Plain => {
            let mut out = StandardStream::stdout(ColorChoice::Auto);
            let mut hdr = ColorSpec::new();
            hdr.set_bold(true);
            out.set_color(&hdr)?;
            writeln!(&mut out, "diff {} .. {}", rev_a, rev_b)?;
            out.reset()?;

            for d in diff.deltas() {
                let status = match d.status() {
                    Delta::Added => ('+', Color::Green, "added"),
                    Delta::Deleted => ('-', Color::Red, "deleted"),
                    Delta::Modified => ('~', Color::Yellow, "modified"),
                    Delta::Renamed => ('~', Color::Yellow, "renamed"),
                    Delta::Copied => ('~', Color::Yellow, "copied"),
                    Delta::Typechange => ('~', Color::Yellow, "typechange"),
                    _ => ('~', Color::Yellow, "unknown"),
                };

                // `unwrap_or_default()` は使わず、安全に空PathBufを代替
                let path: PathBuf = d
                    .new_file()
                    .path()
                    .or_else(|| d.old_file().path())
                    .map(Path::to_path_buf)
                    .unwrap_or_default();

                let mut spec = ColorSpec::new();
                let _ = spec.set_fg(Some(status.1));
                let _ = out.set_color(&spec);
                let _ = write!(&mut out, "{} ", status.0);
                let _ = out.reset();
                writeln!(&mut out, "{}", path.display())?;
            }
        }
        Format::Json => {
            let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
            for d in diff.deltas() {
                let status = match d.status() {
                    Delta::Added => "added",
                    Delta::Deleted => "deleted",
                    Delta::Modified => "modified",
                    Delta::Renamed => "renamed",
                    Delta::Copied => "copied",
                    Delta::Typechange => "typechange",
                    _ => "unknown",
                };

                let path: PathBuf = d
                    .new_file()
                    .path()
                    .or_else(|| d.old_file().path())
                    .map(Path::to_path_buf)
                    .unwrap_or_default();

                let path_s = path.display().to_string();
                serde_json::to_writer(&mut stdout, &JsonDiff { status, path: &path_s })?;
                writeln!(&mut stdout)?;
            }
            stdout.flush()?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_diff_serializes() {
        let j = serde_json::to_string(&JsonDiff { status: "added", path: "src/main.rs" }).unwrap();
        assert!(j.contains("\"status\":\"added\""));
    }
}
