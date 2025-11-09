use anyhow::{Context, Result};
use std::io::Write; // ← これが必要
use std::path::Path;
use termcolor::{Color, ColorChoice, ColorSpec, StandardStream, WriteColor};

pub fn run_diff(rev_a: &str, rev_b: &str, subpath: Option<&Path>) -> Result<()> {
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

    let mut hdr = ColorSpec::new();
    hdr.set_bold(true);
    out.set_color(&hdr)?;
    writeln!(&mut out, "diff {} .. {}", rev_a, rev_b)?;
    out.reset()?;

    diff.print(DiffFormat::NameStatus, |_delta, _hunk, line| {
        let s = std::str::from_utf8(line.content()).unwrap_or("");

        if let Some(first) = s.chars().next() {
            let mut spec = ColorSpec::new();
            match first {
                '+' => { let _ = spec.set_fg(Some(Color::Green)); }
                '-' => { let _ = spec.set_fg(Some(Color::Red)); }
                'M' | '~' => { let _ = spec.set_fg(Some(Color::Yellow)); }
                _ => {}
            }
            let _ = out.set_color(&spec);
            let _ = write!(&mut out, "{}", s);
            let _ = out.reset();
        } else {
            let _ = write!(&mut out, "{}", s);
        }
        true
    })?;

    Ok(())
}
