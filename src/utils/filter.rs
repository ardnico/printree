use anyhow::{anyhow, Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex_automata::meta::Regex;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use termcolor::ColorChoice;

use crate::cli::args::{ColorMode, MatchMode, PatternSyntax, TypeFilter};

pub enum PatternList {
    Glob(GlobSet),
    Regex(Regex),
}

pub fn is_hidden(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

pub fn build_patterns(
    patterns: &[String],
    syntax: PatternSyntax,
    allow_partial: bool,
) -> Result<Option<PatternList>> {
    if patterns.is_empty() {
        return Ok(None);
    }

    match syntax {
        PatternSyntax::Glob => {
            let mut builder = GlobSetBuilder::new();
            for p in patterns {
                let pattern = if allow_partial && !contains_glob_meta(p) {
                    format!("*{p}*")
                } else {
                    p.clone()
                };
                builder
                    .add(Glob::new(&pattern).with_context(|| format!("invalid glob: {pattern}"))?);
            }
            Ok(Some(PatternList::Glob(builder.build()?)))
        }
        PatternSyntax::Regex => {
            let regex =
                Regex::new_many(patterns).map_err(|e| anyhow!("invalid regex pattern: {e}"))?;
            Ok(Some(PatternList::Regex(regex)))
        }
    }
}

pub fn build_include_prefixes(
    root: &Path,
    patterns: &[String],
    syntax: PatternSyntax,
    mode: MatchMode,
) -> HashSet<PathBuf> {
    if patterns.is_empty() || !matches!(mode, MatchMode::Path) {
        return HashSet::new();
    }

    let mut prefixes = HashSet::new();

    if matches!(syntax, PatternSyntax::Glob) {
        for pattern in patterns {
            let mut buf = PathBuf::new();
            let normalized = pattern.replace("\\", "/");
            let parts: Vec<_> = normalized.split('/').filter(|s| !s.is_empty()).collect();
            let keep_last = normalized.ends_with('/');

            for (idx, segment) in parts.iter().enumerate() {
                if contains_glob_meta(segment) {
                    break;
                }

                buf.push(segment);
                let is_last = idx + 1 == parts.len();
                if !is_last || keep_last {
                    prefixes.insert(buf.clone());
                }
            }
        }
    }

    // If a caller provided an absolute path, trim the root prefix so we compare
    // relative paths consistently during traversal.
    prefixes
        .into_iter()
        .map(|p| p.strip_prefix(root).map(PathBuf::from).unwrap_or(p))
        .collect()
}

pub fn include_dir_allowed(
    root: &Path,
    dir_path: &Path,
    include_glob: &Option<PatternList>,
    include_prefixes: &HashSet<PathBuf>,
    mode: MatchMode,
) -> bool {
    if include_glob.is_none() {
        return false;
    }

    let relative = dir_path
        .strip_prefix(root)
        .map(PathBuf::from)
        .unwrap_or_else(|_| dir_path.to_path_buf());

    match mode {
        MatchMode::Name => true,
        MatchMode::Path => {
            include_prefixes.is_empty()
                || include_prefixes
                    .iter()
                    .any(|prefix| relative.starts_with(prefix))
        }
    }
}

pub fn allow_type(ty: &fs::FileType, types: &[TypeFilter]) -> bool {
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

fn target_for_glob(root: &Path, path: &Path, mode: MatchMode) -> PathBuf {
    match mode {
        MatchMode::Name => path.file_name().map(PathBuf::from).unwrap_or_default(),
        MatchMode::Path => path.strip_prefix(root).unwrap_or(path).to_path_buf(),
    }
}

impl PatternList {
    pub fn is_match(&self, target: &Path) -> bool {
        match self {
            PatternList::Glob(gs) => gs.is_match(target),
            PatternList::Regex(re) => re.is_match(target.to_string_lossy().as_ref()),
        }
    }
}

pub fn match_globs(
    root: &Path,
    path: &Path,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    mode: MatchMode,
) -> bool {
    if let Some(gs) = include_glob {
        let target = target_for_glob(root, path, mode);
        if !gs.is_match(&target) {
            return false;
        }
    }
    if let Some(gs) = exclude_glob {
        let target = target_for_glob(root, path, mode);
        if gs.is_match(&target) {
            return false;
        }
    }
    true
}

fn contains_glob_meta(pattern: &str) -> bool {
    pattern.chars().any(|c| matches!(c, '*' | '?' | '[' | '{'))
}

pub fn color_choice(mode: ColorMode) -> ColorChoice {
    match mode {
        ColorMode::Auto => ColorChoice::Auto,
        ColorMode::Always => ColorChoice::Always,
        ColorMode::Never => ColorChoice::Never,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_mode_path_uses_root_prefix_for_globs() {
        let include = build_patterns(&["src/utils/**".to_string()], PatternSyntax::Glob, false)
            .expect("building include glob");
        let root = Path::new("/project");
        let included_path = root.join("src/utils/filter.rs");
        let excluded_path = root.join("src/bin/main.rs");

        assert!(match_globs(
            root,
            &included_path,
            &include,
            &None::<PatternList>,
            MatchMode::Path
        ));
        assert!(!match_globs(
            root,
            &excluded_path,
            &include,
            &None::<PatternList>,
            MatchMode::Path
        ));
    }

    #[test]
    fn include_patterns_support_partial_match_without_glob() {
        let include = build_patterns(&["util".to_string()], PatternSyntax::Glob, true)
            .expect("build patterns")
            .expect("pattern list");
        let root = Path::new("/project");
        let included_path = root.join("src/utils/filter.rs");
        assert!(include.is_match(&included_path));
    }

    #[test]
    fn regex_patterns_are_supported() {
        let include = build_patterns(&[r"src/.+".to_string()], PatternSyntax::Regex, true)
            .expect("build regex patterns");
        let root = Path::new("/project");
        let included_path = root.join("src/utils/filter.rs");
        assert!(match_globs(
            root,
            &included_path,
            &include,
            &None::<PatternList>,
            MatchMode::Path
        ));
    }

    #[test]
    fn include_prefixes_capture_intermediate_directories() {
        let root = Path::new("/project");
        let prefixes = build_include_prefixes(
            root,
            &["src/utils/deep/file.rs".to_string()],
            PatternSyntax::Glob,
            MatchMode::Path,
        );

        assert!(prefixes.contains(Path::new("src")));
        assert!(prefixes.contains(Path::new("src/utils")));
        assert!(prefixes.contains(Path::new("src/utils/deep")));
        assert!(!prefixes.contains(Path::new("src/utils/deep/file.rs")));
    }

    #[test]
    fn include_dir_allowed_accepts_ancestors_and_rejects_unrelated_dirs() {
        let root = Path::new("/project");
        let include_glob = build_patterns(
            &["src/utils/deep/file.rs".to_string()],
            PatternSyntax::Glob,
            true,
        )
        .expect("build patterns");
        let prefixes = build_include_prefixes(
            root,
            &["src/utils/deep/file.rs".to_string()],
            PatternSyntax::Glob,
            MatchMode::Path,
        );

        let ancestor = root.join("src/utils");
        assert!(include_dir_allowed(
            root,
            &ancestor,
            &include_glob,
            &prefixes,
            MatchMode::Path
        ));

        let unrelated = root.join("docs");
        assert!(!include_dir_allowed(
            root,
            &unrelated,
            &include_glob,
            &prefixes,
            MatchMode::Path
        ));
    }
}
