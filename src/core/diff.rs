use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use termcolor::{ColorChoice, ColorSpec, StandardStream, WriteColor};

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
            render_diff_plain(&repo, &diff, &tree_a, &tree_b, subpath, rev_a, rev_b)?;
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
                serde_json::to_writer(
                    &mut stdout,
                    &JsonDiff {
                        status,
                        path: &path_s,
                    },
                )?;
                writeln!(&mut stdout)?;
            }
            stdout.flush()?;
        }
        Format::Ndjson | Format::Csv | Format::Yaml | Format::Html => {
            bail!("format {:?} not supported for diff", format)
        }
    }

    Ok(())
}

struct CombinedNode {
    name: String,
    path: PathBuf,
    old_present: bool,
    new_present: bool,
    children: Vec<CombinedNode>,
}

fn render_diff_plain(
    repo: &git2::Repository,
    diff: &git2::Diff,
    tree_a: &git2::Tree,
    tree_b: &git2::Tree,
    subpath: Option<&Path>,
    rev_a: &str,
    rev_b: &str,
) -> Result<()> {
    let mut out = StandardStream::stdout(ColorChoice::Auto);
    let mut hdr = ColorSpec::new();
    hdr.set_bold(true);
    out.set_color(&hdr)?;
    writeln!(&mut out, "diff {} .. {}", rev_a, rev_b)?;
    out.reset()?;

    let base_path = subpath.unwrap_or_else(|| Path::new(""));

    let root = build_root_node(repo, tree_a, tree_b, base_path)?;

    let (mut statuses_old, mut statuses_new) = collect_statuses(diff, base_path);
    apply_presence_defaults(&root, &mut statuses_old, &mut statuses_new);

    let mut lines = Vec::new();
    render_node(
        &root,
        "",
        "",
        RenderFlags {
            is_root: true,
            is_last_old: false,
            is_last_new: false,
        },
        &StatusMaps {
            old: &statuses_old,
            new: &statuses_new,
        },
        &mut lines,
    );

    let left_width = lines
        .iter()
        .map(|(l, _)| l.chars().count())
        .max()
        .unwrap_or(0);

    for (left, right) in lines {
        writeln!(&mut out, "{:<width$}  {}", left, right, width = left_width)?;
    }

    Ok(())
}

fn build_root_node(
    repo: &git2::Repository,
    tree_a: &git2::Tree,
    tree_b: &git2::Tree,
    base_path: &Path,
) -> Result<CombinedNode> {
    use git2::ObjectType;

    if base_path.as_os_str().is_empty() {
        let name = repo
            .workdir()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("."));
        let mut node = CombinedNode {
            name,
            path: PathBuf::new(),
            old_present: true,
            new_present: true,
            children: Vec::new(),
        };
        node.children = build_children(repo, Some(tree_a), Some(tree_b), Path::new(""))?;
        return Ok(node);
    }

    let entry_a = tree_a.get_path(base_path).ok();
    let entry_b = tree_b.get_path(base_path).ok();

    let name = base_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| base_path.display().to_string());

    let (old_is_dir, tree_a_sub) = if let Some(ref e) = entry_a {
        if e.kind() == Some(ObjectType::Tree) {
            let subtree = e.to_object(repo)?.peel_to_tree()?;
            (true, Some(subtree))
        } else {
            (false, None)
        }
    } else {
        (false, None)
    };

    let (new_is_dir, tree_b_sub) = if let Some(ref e) = entry_b {
        if e.kind() == Some(ObjectType::Tree) {
            let subtree = e.to_object(repo)?.peel_to_tree()?;
            (true, Some(subtree))
        } else {
            (false, None)
        }
    } else {
        (false, None)
    };

    let mut node = CombinedNode {
        name,
        path: PathBuf::new(),
        old_present: entry_a.is_some(),
        new_present: entry_b.is_some(),
        children: Vec::new(),
    };

    if old_is_dir || new_is_dir {
        node.children = build_children(
            repo,
            tree_a_sub.as_ref(),
            tree_b_sub.as_ref(),
            Path::new(""),
        )?;
    }

    Ok(node)
}

fn build_children(
    repo: &git2::Repository,
    tree_a: Option<&git2::Tree>,
    tree_b: Option<&git2::Tree>,
    base: &Path,
) -> Result<Vec<CombinedNode>> {
    use git2::ObjectType;

    let mut names: BTreeSet<String> = BTreeSet::new();
    if let Some(t) = tree_a {
        for entry in t.iter() {
            if let Some(name) = entry.name() {
                names.insert(name.to_string());
            }
        }
    }
    if let Some(t) = tree_b {
        for entry in t.iter() {
            if let Some(name) = entry.name() {
                names.insert(name.to_string());
            }
        }
    }

    let mut children = Vec::new();
    for name in names {
        let entry_a = tree_a.and_then(|t| t.get_name(&name));
        let entry_b = tree_b.and_then(|t| t.get_name(&name));

        let child_path = if base.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            base.join(&name)
        };

        let (old_is_dir, tree_a_child) = if let Some(ref e) = entry_a {
            if e.kind() == Some(ObjectType::Tree) {
                let subtree = e.to_object(repo)?.peel_to_tree()?;
                (true, Some(subtree))
            } else {
                (false, None)
            }
        } else {
            (false, None)
        };

        let (new_is_dir, tree_b_child) = if let Some(ref e) = entry_b {
            if e.kind() == Some(ObjectType::Tree) {
                let subtree = e.to_object(repo)?.peel_to_tree()?;
                (true, Some(subtree))
            } else {
                (false, None)
            }
        } else {
            (false, None)
        };

        let mut node = CombinedNode {
            name: name.clone(),
            path: child_path.clone(),
            old_present: entry_a.is_some(),
            new_present: entry_b.is_some(),
            children: Vec::new(),
        };

        if old_is_dir || new_is_dir {
            node.children = build_children(
                repo,
                tree_a_child.as_ref(),
                tree_b_child.as_ref(),
                &child_path,
            )?;
        }

        children.push(node);
    }

    Ok(children)
}

fn collect_statuses(
    diff: &git2::Diff,
    base_path: &Path,
) -> (HashMap<PathBuf, char>, HashMap<PathBuf, char>) {
    use git2::Delta;

    let mut old = HashMap::new();
    let mut new = HashMap::new();

    for d in diff.deltas() {
        match d.status() {
            Delta::Added => {
                if let Some(path) = d.new_file().path() {
                    if let Some(rel) = relative_to_base(path, base_path) {
                        record_status(&mut old, &mut new, &rel, Some('A'), Some('A'));
                    }
                }
            }
            Delta::Deleted => {
                if let Some(path) = d.old_file().path() {
                    if let Some(rel) = relative_to_base(path, base_path) {
                        record_status(&mut old, &mut new, &rel, Some('D'), Some('D'));
                    }
                }
            }
            Delta::Modified => {
                if let Some(path) = d.new_file().path().or_else(|| d.old_file().path()) {
                    if let Some(rel) = relative_to_base(path, base_path) {
                        record_status(&mut old, &mut new, &rel, Some('M'), Some('M'));
                    }
                }
            }
            Delta::Renamed => {
                if let Some(path) = d.old_file().path() {
                    if let Some(rel) = relative_to_base(path, base_path) {
                        record_status(&mut old, &mut new, &rel, Some('R'), Some('R'));
                    }
                }
                if let Some(path) = d.new_file().path() {
                    if let Some(rel) = relative_to_base(path, base_path) {
                        record_status(&mut old, &mut new, &rel, Some('R'), Some('R'));
                    }
                }
            }
            Delta::Copied => {
                if let Some(path) = d.new_file().path() {
                    if let Some(rel) = relative_to_base(path, base_path) {
                        record_status(&mut old, &mut new, &rel, Some('C'), Some('C'));
                    }
                }
            }
            Delta::Typechange => {
                if let Some(path) = d.new_file().path().or_else(|| d.old_file().path()) {
                    if let Some(rel) = relative_to_base(path, base_path) {
                        record_status(&mut old, &mut new, &rel, Some('T'), Some('T'));
                    }
                }
            }
            _ => {
                if let Some(path) = d.new_file().path().or_else(|| d.old_file().path()) {
                    if let Some(rel) = relative_to_base(path, base_path) {
                        record_status(&mut old, &mut new, &rel, Some('?'), Some('?'));
                    }
                }
            }
        }
    }

    (old, new)
}

fn relative_to_base(path: &Path, base: &Path) -> Option<PathBuf> {
    if base.as_os_str().is_empty() {
        return Some(path.to_path_buf());
    }
    path.strip_prefix(base).ok().map(|p| p.to_path_buf())
}

fn record_status(
    old: &mut HashMap<PathBuf, char>,
    new: &mut HashMap<PathBuf, char>,
    path: &Path,
    old_status: Option<char>,
    new_status: Option<char>,
) {
    if let Some(c) = old_status {
        set_status(old, path, c);
    }
    if let Some(c) = new_status {
        set_status(new, path, c);
    }

    let mut current = path;
    while let Some(parent) = current.parent() {
        if parent.as_os_str().is_empty() && current.as_os_str().is_empty() {
            break;
        }
        set_status(old, parent, 'M');
        set_status(new, parent, 'M');
        if parent.as_os_str().is_empty() {
            break;
        }
        current = parent;
    }
}

fn set_status(map: &mut HashMap<PathBuf, char>, path: &Path, status: char) {
    use std::collections::hash_map::Entry;

    match map.entry(path.to_path_buf()) {
        Entry::Occupied(mut o) => {
            let existing = *o.get();
            if status_priority(status) > status_priority(existing) {
                o.insert(status);
            }
        }
        Entry::Vacant(v) => {
            v.insert(status);
        }
    }
}

fn status_priority(c: char) -> u8 {
    match c {
        'D' => 5,
        'A' => 4,
        'T' => 3,
        'R' | 'C' => 2,
        'M' => 1,
        '?' => 0,
        _ => 0,
    }
}

fn apply_presence_defaults(
    node: &CombinedNode,
    statuses_old: &mut HashMap<PathBuf, char>,
    statuses_new: &mut HashMap<PathBuf, char>,
) {
    if !node.old_present && node.new_present {
        statuses_old.entry(node.path.clone()).or_insert('A');
        statuses_new.entry(node.path.clone()).or_insert('A');
    } else if node.old_present && !node.new_present {
        statuses_old.entry(node.path.clone()).or_insert('D');
        statuses_new.entry(node.path.clone()).or_insert('D');
    }

    for child in &node.children {
        apply_presence_defaults(child, statuses_old, statuses_new);
    }
}

struct RenderFlags {
    is_root: bool,
    is_last_old: bool,
    is_last_new: bool,
}

struct StatusMaps<'a> {
    old: &'a HashMap<PathBuf, char>,
    new: &'a HashMap<PathBuf, char>,
}

fn render_node(
    node: &CombinedNode,
    prefix_old: &str,
    prefix_new: &str,
    flags: RenderFlags,
    statuses: &StatusMaps<'_>,
    lines: &mut Vec<(String, String)>,
) {
    let status_left = statuses.old.get(&node.path).copied().unwrap_or(' ');
    let status_right = statuses.new.get(&node.path).copied().unwrap_or(' ');

    let branch_old = if flags.is_root {
        ""
    } else if node.old_present {
        if flags.is_last_old {
            "└── "
        } else {
            "├── "
        }
    } else {
        "    "
    };

    let branch_new = if flags.is_root {
        ""
    } else if node.new_present {
        if flags.is_last_new {
            "└── "
        } else {
            "├── "
        }
    } else {
        "    "
    };

    let left_line = format_line(
        status_left,
        prefix_old,
        branch_old,
        node.old_present,
        &node.name,
        flags.is_root,
    );
    let right_line = format_line(
        status_right,
        prefix_new,
        branch_new,
        node.new_present,
        &node.name,
        flags.is_root,
    );
    lines.push((left_line, right_line));

    if node.children.is_empty() {
        return;
    }

    let next_prefix_old = if flags.is_root {
        String::new()
    } else {
        let mut s = prefix_old.to_string();
        s.push_str(if node.old_present {
            if flags.is_last_old {
                "    "
            } else {
                "│   "
            }
        } else {
            "    "
        });
        s
    };

    let next_prefix_new = if flags.is_root {
        String::new()
    } else {
        let mut s = prefix_new.to_string();
        s.push_str(if node.new_present {
            if flags.is_last_new {
                "    "
            } else {
                "│   "
            }
        } else {
            "    "
        });
        s
    };

    let mut old_remaining = node.children.iter().filter(|c| c.old_present).count();
    let mut new_remaining = node.children.iter().filter(|c| c.new_present).count();

    for child in &node.children {
        let child_is_last_old = if child.old_present {
            old_remaining == 1
        } else {
            false
        };
        let child_is_last_new = if child.new_present {
            new_remaining == 1
        } else {
            false
        };

        render_node(
            child,
            &next_prefix_old,
            &next_prefix_new,
            RenderFlags {
                is_root: false,
                is_last_old: child_is_last_old,
                is_last_new: child_is_last_new,
            },
            statuses,
            lines,
        );

        if child.old_present {
            old_remaining = old_remaining.saturating_sub(1);
        }
        if child.new_present {
            new_remaining = new_remaining.saturating_sub(1);
        }
    }
}

fn format_line(
    status: char,
    prefix: &str,
    branch: &str,
    present: bool,
    name: &str,
    is_root: bool,
) -> String {
    let mut line = String::new();
    line.push(status);
    line.push(' ');
    if !is_root {
        line.push_str(prefix);
        line.push_str(branch);
    }
    if present || is_root {
        line.push_str(name);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_diff_serializes() {
        let j = serde_json::to_string(&JsonDiff {
            status: "added",
            path: "src/main.rs",
        })
        .unwrap();
        assert!(j.contains("\"status\":\"added\""));
    }
}
