use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use termcolor::ColorChoice;

use crate::cli::args::{ColorMode, MatchMode, TypeFilter};

pub fn is_hidden(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

pub fn build_globset(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p).with_context(|| format!("invalid glob: {p}"))?);
    }
    Ok(Some(builder.build()?))
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
        MatchMode::Name => path.file_name().map(|s| PathBuf::from(s)).unwrap_or_default(),
        MatchMode::Path => path.strip_prefix(root).unwrap_or(path).to_path_buf(),
    }
}

pub fn match_globs(
    root: &Path,
    path: &Path,
    include_glob: &Option<GlobSet>,
    exclude_glob: &Option<GlobSet>,
    mode: MatchMode,
) -> bool {
    if let Some(gs) = include_glob {
        let target = target_for_glob(root, path, mode);
        if !gs.is_match(target) {
            return false;
        }
    }
    if let Some(gs) = exclude_glob {
        let target = target_for_glob(root, path, mode);
        if gs.is_match(target) {
            return false;
        }
    }
    true
}

pub fn color_choice(mode: ColorMode) -> ColorChoice {
    match mode {
        ColorMode::Auto => ColorChoice::Auto,
        ColorMode::Always => ColorChoice::Always,
        ColorMode::Never => ColorChoice::Never,
    }
}
