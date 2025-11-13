use anyhow::{anyhow, Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex_automata::meta::Regex;
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
        MatchMode::Name => path
            .file_name()
            .map(|s| PathBuf::from(s))
            .unwrap_or_default(),
        MatchMode::Path => path.strip_prefix(root).unwrap_or(path).to_path_buf(),
    }
}

impl PatternList {
    fn is_match(&self, target: &Path) -> bool {
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
        let include = build_patterns(
            &vec!["src/utils/**".to_string()],
            PatternSyntax::Glob,
            false,
        )
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
        let include = build_patterns(&vec!["util".to_string()], PatternSyntax::Glob, true)
            .expect("build patterns")
            .expect("pattern list");
        let root = Path::new("/project");
        let included_path = root.join("src/utils/filter.rs");
        assert!(include.is_match(&included_path));
    }

    #[test]
    fn regex_patterns_are_supported() {
        let include = build_patterns(&vec![r"src/.+".to_string()], PatternSyntax::Regex, true)
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
}
