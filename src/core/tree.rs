use encoding_rs::{Encoding, SHIFT_JIS, UTF_16LE};
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, FileType, Metadata};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use git2::{ErrorCode, Repository, Status, StatusOptions};
use regex_automata::meta::Regex;
use serde::Serialize;
use termcolor::{Color, ColorSpec, StandardStream, WriteColor};

use crate::cli::{Cli, Format, MatchMode, SortMode};
use crate::utils::{allow_type, build_patterns, color_choice, is_hidden, match_globs, PatternList};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

/// ディレクトリツリーのメイン実行関数
pub fn run_tree(cli: &Cli) -> Result<()> {
    let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let include_glob = build_patterns(&cli.includes, cli.pattern_syntax, true)?;
    let exclude_glob = build_patterns(&cli.excludes, cli.pattern_syntax, false)?;
    let filters = Filters::from_cli(cli, &root)?;
    let git = GitTracker::prepare(&root, cli)?;
    let jobs = JobPool::new(cli)?;

    match cli.format {
        Format::Json => run_tree_json(
            &root,
            cli,
            &include_glob,
            &exclude_glob,
            &filters,
            &git,
            &jobs,
        ),
        Format::Plain => run_tree_plain(
            &root,
            cli,
            &include_glob,
            &exclude_glob,
            &filters,
            &git,
            &jobs,
        ),
        Format::Ndjson => run_tree_ndjson(
            &root,
            cli,
            &include_glob,
            &exclude_glob,
            &filters,
            &git,
            &jobs,
        ),
        Format::Csv => run_tree_csv(
            &root,
            cli,
            &include_glob,
            &exclude_glob,
            &filters,
            &git,
            &jobs,
        ),
        Format::Yaml => run_tree_yaml(
            &root,
            cli,
            &include_glob,
            &exclude_glob,
            &filters,
            &git,
            &jobs,
        ),
        Format::Html => run_tree_html(
            &root,
            cli,
            &include_glob,
            &exclude_glob,
            &filters,
            &git,
            &jobs,
        ),
    }
}

#[derive(Clone, Debug)]
struct EntryMeta {
    path: PathBuf,
    name: OsString,
    file_type: Option<FileType>,
    target_file_type: Option<FileType>,
    size: Option<u64>,
    mtime: Option<SystemTime>,
    perm_unix: Option<u32>,
    #[cfg_attr(not(windows), allow(dead_code))]
    perm_win: Option<u32>,
    is_symlink: bool,
    symlink_target: Option<PathBuf>,
    canonical_path: Option<PathBuf>,
    loop_detected: bool,
    error: Option<String>,
    git_status: Option<char>,
}

#[derive(Clone, Debug, Serialize)]
struct Entry {
    name: String,
    path: String,
    depth: usize,
    kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    perm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    symlink_target: Option<String>,
    loop_detected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_status: Option<char>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum EntryKind {
    File,
    Dir,
    Symlink,
    Unknown,
}

struct Frame {
    entries: Vec<EntryMeta>,
    idx: usize,
    prefix: String,
    depth: usize,
}

struct PlainPending {
    entry: Entry,
    prefix: String,
    is_last: bool,
    total_size: u64,
}

impl PlainPending {
    fn new(mut entry: Entry, prefix: String, is_last: bool) -> Self {
        entry.size = None;
        Self {
            entry,
            prefix,
            is_last,
            total_size: 0,
        }
    }

    fn record_child_size(&mut self, child_size: u64) {
        self.total_size = self.total_size.saturating_add(child_size);
    }
}

struct YamlNode {
    entry: Entry,
    children: Vec<YamlNode>,
}

fn canonical_root_for_security(root: &Path, root_meta: &EntryMeta) -> Option<PathBuf> {
    if let Some(real) = root_meta.canonical_path.clone() {
        Some(real)
    } else if root.is_absolute() {
        Some(root.to_path_buf())
    } else {
        env::current_dir().ok().map(|cwd| cwd.join(root))
    }
}

fn path_within_root(path: &Path, root: &Path) -> bool {
    if path == root {
        return true;
    }
    path.starts_with(root)
}

#[derive(Clone)]
struct EntrySeed {
    path: PathBuf,
    name: OsString,
    file_type_hint: Option<FileType>,
    file_type_error: Option<String>,
}

struct GitTracker {
    map: Option<GitStatusMap>,
}

struct GitStatusMap {
    workdir: PathBuf,
    cwd: PathBuf,
    statuses: HashMap<PathBuf, char>,
}

struct JobPool {
    workers: usize,
}

impl JobPool {
    fn new(cli: &Cli) -> Result<Self> {
        let mut jobs = cli.jobs;
        if jobs == 0 {
            return Err(anyhow!("--jobs must be >= 1"));
        }
        if jobs > 256 {
            eprintln!("[warn] --jobs value {jobs} clamped to 256");
            jobs = 256;
        }
        #[cfg(windows)]
        if jobs > 64 {
            eprintln!("[warn] high --jobs values may perform poorly on Windows");
        }
        Ok(Self { workers: jobs })
    }

    fn workers(&self) -> usize {
        self.workers
    }

    fn is_parallel(&self) -> bool {
        self.workers > 1
    }
}

impl GitTracker {
    fn prepare(root: &Path, cli: &Cli) -> Result<Self> {
        let want_status = cli.git_status || cli.git_rename;
        if !want_status {
            return Ok(Self { map: None });
        }

        if cli.git_rename {
            eprintln!("[warn] rename detection enabled (slow)");
        }

        let cwd = env::current_dir()?;
        let repo = match Repository::discover(root) {
            Ok(repo) => repo,
            Err(err) if err.code() == ErrorCode::NotFound => {
                if cli.git_status || cli.git_rename {
                    eprintln!("[warn] --git-status ignored: .git not found");
                }
                return Ok(Self { map: None });
            }
            Err(err) => return Err(anyhow!(err)),
        };

        let workdir = match repo.workdir() {
            Some(dir) => dir.to_path_buf(),
            None => {
                eprintln!("[warn] --git-status ignored: repository has no workdir");
                return Ok(Self { map: None });
            }
        };

        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false)
            .include_unreadable(true);
        if cli.git_rename {
            opts.renames_head_to_index(true);
            opts.renames_index_to_workdir(true);
            opts.renames_from_rewrites(true);
        }

        let statuses = repo.statuses(Some(&mut opts))?;
        let mut map = HashMap::new();
        for entry in statuses.iter() {
            if let Some(symbol) = git_status_symbol(entry.status()) {
                if let Some(path) = status_entry_path(&entry) {
                    update_git_status(&mut map, path, symbol);
                }
            }
        }

        Ok(Self {
            map: Some(GitStatusMap {
                workdir,
                cwd,
                statuses: map,
            }),
        })
    }

    fn apply(&self, meta: &mut EntryMeta) {
        if let Some(map) = &self.map {
            meta.git_status = map.status_for(&meta.path);
        }
    }
}

impl GitStatusMap {
    fn status_for(&self, path: &Path) -> Option<char> {
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        let rel = abs.strip_prefix(&self.workdir).ok()?;
        if rel.as_os_str().is_empty() {
            return None;
        }
        self.statuses.get(rel).copied()
    }
}

fn status_entry_path(entry: &git2::StatusEntry<'_>) -> Option<PathBuf> {
    if let Some(delta) = entry.index_to_workdir() {
        if let Some(path) = delta.new_file().path() {
            return Some(path.to_path_buf());
        }
    }
    if let Some(delta) = entry.head_to_index() {
        if let Some(path) = delta.new_file().path() {
            return Some(path.to_path_buf());
        }
        if let Some(path) = delta.old_file().path() {
            return Some(path.to_path_buf());
        }
    }
    entry.path().map(PathBuf::from)
}

fn git_status_symbol(status: Status) -> Option<char> {
    if status.is_wt_deleted() || status.is_index_deleted() {
        Some('D')
    } else if status.is_wt_renamed() || status.is_index_renamed() {
        Some('R')
    } else if status.is_wt_new() || status.is_index_new() {
        Some('A')
    } else if status.is_wt_modified()
        || status.is_index_modified()
        || status.is_wt_typechange()
        || status.is_index_typechange()
    {
        Some('M')
    } else {
        None
    }
}

fn update_git_status(map: &mut HashMap<PathBuf, char>, path: PathBuf, status: char) {
    match map.entry(path) {
        std::collections::hash_map::Entry::Occupied(mut occ) => {
            if git_status_priority(status) > git_status_priority(*occ.get()) {
                occ.insert(status);
            }
        }
        std::collections::hash_map::Entry::Vacant(vac) => {
            vac.insert(status);
        }
    }
}

fn git_status_priority(symbol: char) -> u8 {
    match symbol {
        'D' => 4,
        'R' => 3,
        'A' => 2,
        'M' => 1,
        _ => 0,
    }
}

struct Filters {
    root: PathBuf,
    match_mode: MatchMode,
    regex: Option<Regex>,
    size: Option<SizeFilter>,
    mtime: Option<MtimeFilter>,
    perm: Option<PermFilter>,
}

#[derive(Clone, Copy)]
enum SizeCmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

struct SizeFilter {
    cmp: SizeCmp,
    threshold: u64,
}

struct MtimeFilter {
    earliest: SystemTime,
}

struct PermFilter {
    expected: u32,
}

impl Filters {
    fn from_cli(cli: &Cli, root: &Path) -> Result<Self> {
        let regex = if let Some(pattern) = cli.filter_regex.as_deref() {
            Some(
                Regex::new(pattern)
                    .map_err(|err| anyhow!("invalid --filter-regex value: {err}"))?,
            )
        } else {
            None
        };

        let size = if let Some(spec) = cli.filter_size.as_deref() {
            Some(parse_size_filter(spec)?)
        } else {
            None
        };

        let mtime = if let Some(spec) = cli.filter_mtime.as_deref() {
            Some(parse_mtime_filter(spec)?)
        } else {
            None
        };

        let perm = if let Some(spec) = cli.filter_perm.as_deref() {
            parse_perm_filter(spec)?
        } else {
            None
        };

        Ok(Self {
            root: root.to_path_buf(),
            match_mode: cli.match_mode,
            regex,
            size,
            mtime,
            perm,
        })
    }

    fn allows(&self, meta: &EntryMeta) -> bool {
        if let Some(re) = &self.regex {
            let target = match self.match_mode {
                MatchMode::Name => meta.name.to_string_lossy().into_owned(),
                MatchMode::Path => meta
                    .path
                    .strip_prefix(&self.root)
                    .unwrap_or(&meta.path)
                    .display()
                    .to_string(),
            };
            if !re.is_match(target.as_str()) {
                return false;
            }
        }

        if let Some(size) = &self.size {
            if !size.allows(meta.size) {
                return false;
            }
        }

        if let Some(mtime) = &self.mtime {
            if !mtime.allows(meta.mtime) {
                return false;
            }
        }

        if let Some(perm) = &self.perm {
            if !perm.allows(meta.perm_unix) {
                return false;
            }
        }

        true
    }
}

impl SizeFilter {
    fn allows(&self, size: Option<u64>) -> bool {
        let Some(size) = size else {
            return false;
        };
        match self.cmp {
            SizeCmp::Lt => size < self.threshold,
            SizeCmp::Le => size <= self.threshold,
            SizeCmp::Eq => size == self.threshold,
            SizeCmp::Ge => size >= self.threshold,
            SizeCmp::Gt => size > self.threshold,
        }
    }
}

impl MtimeFilter {
    fn allows(&self, mtime: Option<SystemTime>) -> bool {
        match mtime {
            Some(time) => time >= self.earliest,
            None => false,
        }
    }
}

impl PermFilter {
    fn allows(&self, perm: Option<u32>) -> bool {
        match perm {
            Some(bits) => bits & 0o777 == self.expected,
            None => false,
        }
    }
}

fn parse_size_filter(spec: &str) -> Result<SizeFilter> {
    let spec = spec.trim();
    let (cmp, remainder) = if let Some(rest) = spec.strip_prefix(">=") {
        (SizeCmp::Ge, rest)
    } else if let Some(rest) = spec.strip_prefix("<=") {
        (SizeCmp::Le, rest)
    } else if let Some(rest) = spec.strip_prefix("==") {
        (SizeCmp::Eq, rest)
    } else if let Some(rest) = spec.strip_prefix('>') {
        (SizeCmp::Gt, rest)
    } else if let Some(rest) = spec.strip_prefix('<') {
        (SizeCmp::Lt, rest)
    } else {
        return Err(anyhow!("invalid --filter-size value: {spec}"));
    };

    let remainder = remainder.trim();
    if remainder.is_empty() {
        return Err(anyhow!("invalid --filter-size value: {spec}"));
    }

    let mut split_idx = remainder.len();
    for (idx, ch) in remainder.char_indices() {
        if !ch.is_ascii_digit() {
            split_idx = idx;
            break;
        }
    }

    let (num_part, unit_part) = remainder.split_at(split_idx);
    if num_part.is_empty() {
        return Err(anyhow!("invalid --filter-size value: {spec}"));
    }
    let value: u64 = num_part
        .parse()
        .map_err(|_| anyhow!("invalid --filter-size numeric value: {spec}"))?;

    let unit = unit_part.trim().to_ascii_lowercase();
    let multiplier: u64 = match unit.as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        _ => return Err(anyhow!("invalid --filter-size unit: {spec}")),
    };

    let threshold = value
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("--filter-size value overflow: {spec}"))?;

    Ok(SizeFilter { cmp, threshold })
}

fn parse_mtime_filter(spec: &str) -> Result<MtimeFilter> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(anyhow!("invalid --filter-mtime value"));
    }

    let mut split_idx = spec.len();
    for (idx, ch) in spec.char_indices() {
        if !ch.is_ascii_digit() {
            split_idx = idx;
            break;
        }
    }

    let (num_part, unit_part) = spec.split_at(split_idx);
    if num_part.is_empty() {
        return Err(anyhow!("invalid --filter-mtime value: {spec}"));
    }
    let quantity: u64 = num_part
        .parse()
        .map_err(|_| anyhow!("invalid --filter-mtime value: {spec}"))?;

    let unit = unit_part.trim().to_ascii_lowercase();
    let seconds = match unit.as_str() {
        "s" | "sec" | "secs" => quantity,
        "m" | "min" | "mins" => quantity
            .checked_mul(60u64)
            .ok_or_else(|| anyhow!("invalid --filter-mtime value: {spec}"))?,
        "h" | "hour" | "hours" => quantity
            .checked_mul(60u64 * 60u64)
            .ok_or_else(|| anyhow!("invalid --filter-mtime value: {spec}"))?,
        "d" | "day" | "days" => quantity
            .checked_mul(60u64 * 60u64 * 24u64)
            .ok_or_else(|| anyhow!("invalid --filter-mtime value: {spec}"))?,
        "w" | "week" | "weeks" => quantity
            .checked_mul(60u64 * 60u64 * 24u64 * 7u64)
            .ok_or_else(|| anyhow!("invalid --filter-mtime value: {spec}"))?,
        _ => return Err(anyhow!("invalid --filter-mtime unit: {spec}")),
    };

    let duration = Duration::from_secs(seconds);
    let now = SystemTime::now();
    let earliest = now.checked_sub(duration).unwrap_or(UNIX_EPOCH);
    Ok(MtimeFilter { earliest })
}

fn parse_perm_filter(spec: &str) -> Result<Option<PermFilter>> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("invalid --filter-perm value"));
    }

    #[cfg(windows)]
    {
        let _ = trimmed;
        eprintln!("[warn] --filter-perm ignored on Windows");
        Ok(None)
    }

    #[cfg(unix)]
    {
        if !trimmed.chars().all(|c| c.is_ascii_digit()) {
            return Err(anyhow!("invalid --filter-perm value: {trimmed}"));
        }
        if trimmed.len() < 3 || trimmed.len() > 4 {
            return Err(anyhow!("invalid --filter-perm value: {trimmed}"));
        }
        let value = u32::from_str_radix(trimmed, 8)
            .map_err(|_| anyhow!("invalid --filter-perm value: {trimmed}"))?;
        Ok(Some(PermFilter {
            expected: value & 0o777,
        }))
    }
}

impl EntryMeta {
    fn from_path(path: &Path) -> Self {
        let name = path
            .file_name()
            .map(OsString::from)
            .unwrap_or_else(|| path.as_os_str().to_owned());
        let mut errors = Vec::new();
        let metadata_symlink = fs::symlink_metadata(path)
            .map_err(|err| {
                errors.push(err.to_string());
                err
            })
            .ok();

        let mut file_type = metadata_symlink.as_ref().map(|m| m.file_type());
        let is_symlink = file_type.map(|ft| ft.is_symlink()).unwrap_or(false);

        let metadata_follow = if is_symlink {
            fs::metadata(path)
                .map_err(|err| {
                    errors.push(err.to_string());
                    err
                })
                .ok()
        } else {
            metadata_symlink.clone()
        };

        if file_type.is_none() {
            file_type = metadata_follow.as_ref().map(|m| m.file_type());
        }

        Self::construct(
            path.to_path_buf(),
            name,
            file_type,
            metadata_follow,
            is_symlink,
            errors,
        )
    }

    fn construct(
        path: PathBuf,
        name: OsString,
        file_type: Option<FileType>,
        metadata: Option<Metadata>,
        is_symlink: bool,
        errors: Vec<String>,
    ) -> Self {
        let mut size = None;
        let mut mtime = None;
        let mut perm_unix = None;
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut perm_win = None;
        let mut target_file_type = None;

        if let Some(md) = metadata.as_ref() {
            size = Some(md.len());
            mtime = md.modified().ok();
            target_file_type = Some(md.file_type());
            #[cfg(unix)]
            {
                perm_unix = Some(md.permissions().mode());
            }
            #[cfg(windows)]
            {
                perm_win = Some(md.file_attributes());
            }
        }

        let is_dir = file_type.map(|ft| ft.is_dir()).unwrap_or(false);
        let canonical_path = if is_dir || is_symlink {
            fs::canonicalize(&path).ok()
        } else {
            None
        };

        let symlink_target = if is_symlink {
            if let Some(canon) = canonical_path.clone() {
                Some(canon)
            } else {
                fs::read_link(&path).ok().map(|target| {
                    if target.is_absolute() {
                        target
                    } else {
                        path.parent()
                            .map(|parent| parent.join(&target))
                            .unwrap_or(target)
                    }
                })
            }
        } else {
            None
        };

        Self {
            path,
            name,
            file_type,
            target_file_type,
            size,
            mtime,
            perm_unix,
            perm_win,
            is_symlink,
            symlink_target,
            canonical_path,
            loop_detected: false,
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("; "))
            },
            git_status: None,
        }
    }

    fn is_directory(&self) -> bool {
        self.file_type.map(|ft| ft.is_dir()).unwrap_or(false)
    }

    fn points_to_directory(&self) -> bool {
        if self.is_symlink {
            self.target_file_type.map(|ft| ft.is_dir()).unwrap_or(false)
        } else {
            self.is_directory()
        }
    }

    fn sort_key(&self) -> &OsString {
        &self.name
    }

    fn from_seed(seed: EntrySeed) -> Self {
        let EntrySeed {
            path,
            name,
            file_type_hint,
            file_type_error,
        } = seed;

        let mut errors = Vec::new();
        if let Some(err) = file_type_error {
            errors.push(err);
        }

        let mut file_type = file_type_hint;
        if file_type.is_none() {
            match fs::symlink_metadata(&path) {
                Ok(md) => file_type = Some(md.file_type()),
                Err(err) => errors.push(err.to_string()),
            }
        }

        let metadata = fs::metadata(&path)
            .map_err(|err| {
                errors.push(err.to_string());
                err
            })
            .ok();

        if file_type.is_none() {
            if let Some(md) = metadata.as_ref() {
                file_type = Some(md.file_type());
            }
        }

        let is_symlink = file_type.map(|ft| ft.is_symlink()).unwrap_or(false);

        Self::construct(path, name, file_type, metadata, is_symlink, errors)
    }
}

#[cfg(unix)]
fn format_permissions(meta: &EntryMeta) -> Option<String> {
    let _ = meta.perm_win;
    meta.perm_unix.map(|perm| format!("{perm:03o}"))
}

#[cfg(windows)]
fn format_permissions(meta: &EntryMeta) -> Option<String> {
    if let Some(perm) = meta.perm_unix {
        return Some(format!("{perm:03o}"));
    }
    meta.perm_win.map(format_windows_attributes)
}

#[cfg(not(any(unix, windows)))]
fn format_permissions(_meta: &EntryMeta) -> Option<String> {
    None
}

#[cfg(windows)]
fn format_windows_attributes(attrs: u32) -> String {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_COMPRESSED, FILE_ATTRIBUTE_DEVICE,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_HIDDEN,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_OFFLINE,
        FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SYSTEM,
        FILE_ATTRIBUTE_TEMPORARY,
    };

    let mut parts = Vec::new();
    let flags: &[(u32, &str)] = &[
        (FILE_ATTRIBUTE_DIRECTORY, "DIR"),
        (FILE_ATTRIBUTE_READONLY, "READONLY"),
        (FILE_ATTRIBUTE_HIDDEN, "HIDDEN"),
        (FILE_ATTRIBUTE_SYSTEM, "SYSTEM"),
        (FILE_ATTRIBUTE_ARCHIVE, "ARCHIVE"),
        (FILE_ATTRIBUTE_REPARSE_POINT, "REPARSE"),
        (FILE_ATTRIBUTE_COMPRESSED, "COMPRESSED"),
        (FILE_ATTRIBUTE_ENCRYPTED, "ENCRYPTED"),
        (FILE_ATTRIBUTE_OFFLINE, "OFFLINE"),
        (FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, "NOINDEX"),
        (FILE_ATTRIBUTE_TEMPORARY, "TEMP"),
        (FILE_ATTRIBUTE_DEVICE, "DEVICE"),
        (FILE_ATTRIBUTE_NORMAL, "NORMAL"),
    ];

    for (mask, label) in flags {
        if attrs & *mask != 0 {
            parts.push(*label);
        }
    }

    if parts.is_empty() {
        format!("0x{attrs:08X}")
    } else {
        format!("{} (0x{attrs:08X})", parts.join("|"))
    }
}

impl Entry {
    fn from_meta(meta: &EntryMeta, depth: usize) -> Self {
        let kind = if meta.is_symlink {
            EntryKind::Symlink
        } else if meta.is_directory() {
            EntryKind::Dir
        } else if meta.file_type.map(|ft| ft.is_file()).unwrap_or(false) {
            EntryKind::File
        } else {
            EntryKind::Unknown
        };

        let mtime = meta.mtime.and_then(|mtime| {
            mtime
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs().to_string())
        });

        let perm = format_permissions(meta);

        let symlink_target = if meta.is_symlink {
            meta.symlink_target
                .as_ref()
                .map(|p| p.display().to_string())
                .or_else(|| Some(String::from("[broken symlink]")))
        } else {
            None
        };

        Self {
            name: meta.name.to_string_lossy().into_owned(),
            path: meta.path.display().to_string(),
            depth,
            kind,
            size: meta.size,
            mtime,
            perm,
            symlink_target,
            loop_detected: meta.loop_detected,
            error: meta.error.clone(),
            git_status: meta.git_status,
        }
    }
}

fn handle_entry(
    entry_meta: &mut EntryMeta,
    prefix: &str,
    depth: usize,
    is_last: bool,
    cli: &Cli,
    visited: &mut HashSet<PathBuf>,
    root_guard: Option<&Path>,
) -> (Entry, bool, String) {
    let mut loop_detected = false;
    let mut descend = entry_meta.points_to_directory();
    let mut canonical_to_record: Option<PathBuf> = None;
    let mut blocked_outside_root = false;

    if entry_meta.is_symlink {
        if let Some(canonical) = entry_meta.canonical_path.clone() {
            if visited.contains(&canonical) {
                loop_detected = true;
                descend = false;
            } else if cli.follow_symlinks && entry_meta.points_to_directory() {
                canonical_to_record = Some(canonical);
            } else {
                descend = false;
            }
        } else {
            descend = false;
        }
    } else if entry_meta.is_directory() {
        if let Some(canonical) = entry_meta.canonical_path.clone() {
            if visited.contains(&canonical) {
                loop_detected = true;
                descend = false;
            } else {
                canonical_to_record = Some(canonical);
            }
        }
    } else {
        descend = false;
    }

    if let (Some(root_path), Some(candidate)) = (root_guard, canonical_to_record.as_ref()) {
        if !path_within_root(candidate, root_path) {
            descend = false;
            blocked_outside_root = true;
            canonical_to_record = None;
            if entry_meta.error.is_none() {
                entry_meta.error = Some(String::from("symlink target outside root"));
            }
        }
    }

    if let Some(maxd) = cli.max_depth {
        if depth >= maxd {
            descend = false;
        }
    }

    entry_meta.loop_detected = loop_detected;

    let entry = Entry::from_meta(entry_meta, depth);

    let child_prefix = if is_last {
        format!("{}    ", prefix)
    } else {
        format!("{}│   ", prefix)
    };

    if descend {
        if let Some(canonical) = canonical_to_record {
            visited.insert(canonical);
        }
    } else if blocked_outside_root && entry_meta.is_symlink {
        entry_meta.loop_detected = false;
    }

    (entry, descend, child_prefix)
}

// ---------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------
fn make_encoded_writer(cli: &Cli) -> Box<dyn WriteColor> {
    match cli.encoding {
        crate::cli::EncodingMode::Utf8 => Box::new(StandardStream::stdout(color_choice(cli.color))),
        crate::cli::EncodingMode::Utf8bom => {
            let mut stream = StandardStream::stdout(color_choice(cli.color));
            stream.write_all(&[0xEF, 0xBB, 0xBF]).ok();
            Box::new(stream)
        }
        crate::cli::EncodingMode::Utf16le => {
            let mut stream = StandardStream::stdout(color_choice(cli.color));
            stream.write_all(&[0xFF, 0xFE]).ok();
            Box::new(EncodingWriter::new(stream, UTF_16LE))
        }
        crate::cli::EncodingMode::Sjis => Box::new(EncodingWriter::new(
            StandardStream::stdout(color_choice(cli.color)),
            SHIFT_JIS,
        )),
        crate::cli::EncodingMode::Auto => Box::new(StandardStream::stdout(color_choice(cli.color))),
    }
}

/// 出力時の再エンコードを行う構造体
struct EncodingWriter<W: WriteColor> {
    inner: W,
    encoding: &'static Encoding,
}

impl<W: WriteColor> EncodingWriter<W> {
    fn new(inner: W, encoding: &'static Encoding) -> Self {
        Self { inner, encoding }
    }
}

impl<W: WriteColor> Write for EncodingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let s = std::str::from_utf8(buf)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let (cow, _, had_errors) = self.encoding.encode(s);
        if had_errors {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "failed to encode output",
            ));
        }
        self.inner.write_all(&cow)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: WriteColor> WriteColor for EncodingWriter<W> {
    fn supports_color(&self) -> bool {
        self.inner.supports_color()
    }

    fn set_color(&mut self, spec: &ColorSpec) -> io::Result<()> {
        self.inner.set_color(spec)
    }

    fn reset(&mut self) -> io::Result<()> {
        self.inner.reset()
    }
}

// ---------------------------------------------------------------------
// PLAIN 出力モード
// ---------------------------------------------------------------------
fn run_tree_plain(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
) -> Result<()> {
    let mut out = make_encoded_writer(cli);
    let mut bold = ColorSpec::new();
    bold.set_bold(true);
    out.set_color(&bold)?;
    writeln!(&mut out, "{}", root.display())?;
    out.reset()?;

    let mut root_meta = EntryMeta::from_path(root);
    git.apply(&mut root_meta);
    let root_security = canonical_root_for_security(root, &root_meta);
    let root_guard = root_security.as_deref();

    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Some(real) = root_security.clone() {
        visited.insert(real);
    } else {
        visited.insert(root.to_path_buf());
    }

    if !root_meta.points_to_directory() || matches!(cli.max_depth, Some(1)) {
        return Ok(());
    }

    let mut stack: Vec<Frame> = Vec::new();
    let mut pending_dirs: Vec<PlainPending> = Vec::new();
    if let Some(frame) = read_dir_frame(
        root,
        "",
        1,
        cli,
        include_glob,
        exclude_glob,
        filters,
        git,
        jobs,
    )? {
        stack.push(frame);
    }

    let mut depth_warned = false;
    while !stack.is_empty() {
        let stack_len = stack.len();
        let frame = stack.last_mut().unwrap();
        if frame.idx >= frame.entries.len() {
            stack.pop();
            if let Some(pending) = pending_dirs.pop() {
                finalize_pending_dir(out.as_mut(), pending, &mut pending_dirs)?;
            }
            continue;
        }

        let idx = frame.idx;
        let is_last = idx + 1 == frame.entries.len();
        let entry_meta = &mut frame.entries[idx];
        frame.idx += 1;

        let (entry, descend, child_prefix) = handle_entry(
            entry_meta,
            &frame.prefix,
            frame.depth,
            is_last,
            cli,
            &mut visited,
            root_guard,
        );

        if descend {
            let child_path = entry_meta.path.clone();
            let pending_entry = PlainPending::new(entry, frame.prefix.clone(), is_last);
            match read_dir_frame(
                &child_path,
                &child_prefix,
                frame.depth + 1,
                cli,
                include_glob,
                exclude_glob,
                filters,
                git,
                jobs,
            )? {
                Some(child_frame) => {
                    pending_dirs.push(pending_entry);
                    stack.push(child_frame);
                }
                None => {
                    finalize_pending_dir(out.as_mut(), pending_entry, &mut pending_dirs)?;
                }
            }
        } else {
            if !depth_warned
                && cli.warn_depth > 0
                && pending_dirs.len() + stack_len > cli.warn_depth
            {
                eprintln!(
                    "[warn] traversal depth {} exceeded warn threshold {}",
                    pending_dirs.len() + stack_len,
                    cli.warn_depth
                );
                depth_warned = true;
            }
            finalize_plain_entry(
                out.as_mut(),
                entry,
                &frame.prefix,
                is_last,
                &mut pending_dirs,
            )?;
        }
    }

    Ok(())
}

fn finalize_pending_dir(
    out: &mut dyn WriteColor,
    mut pending: PlainPending,
    pending_dirs: &mut Vec<PlainPending>,
) -> io::Result<()> {
    pending.entry.size = Some(pending.total_size);
    finalize_plain_entry(
        out,
        pending.entry,
        &pending.prefix,
        pending.is_last,
        pending_dirs,
    )
}

fn finalize_plain_entry(
    out: &mut dyn WriteColor,
    entry: Entry,
    prefix: &str,
    is_last: bool,
    pending_dirs: &mut Vec<PlainPending>,
) -> io::Result<()> {
    if let Some(size) = entry.size {
        if let Some(parent) = pending_dirs.last_mut() {
            parent.record_child_size(size);
        }
    }
    write_plain_entry(out, prefix, &entry, is_last)
}

fn write_plain_entry(
    out: &mut dyn WriteColor,
    prefix: &str,
    entry: &Entry,
    is_last: bool,
) -> io::Result<()> {
    let connector = if is_last { "└── " } else { "├── " };
    write!(out, "{}{}", prefix, connector)?;

    if let Some(status) = entry.git_status {
        if let Some(color) = match status {
            'M' => Some(Color::Yellow),
            'A' => Some(Color::Green),
            'D' => Some(Color::Red),
            'R' => Some(Color::Cyan),
            _ => None,
        } {
            let mut spec = ColorSpec::new();
            spec.set_fg(Some(color));
            out.set_color(&spec)?;
        }
        write!(out, "[{status}] ")?;
        out.reset()?;
    }

    if let Some(size) = entry.size {
        write!(out, "[{size}] ")?;
    }

    match entry.kind {
        EntryKind::Dir => {
            let mut spec = ColorSpec::new();
            spec.set_fg(Some(Color::Blue));
            out.set_color(&spec)?;
        }
        EntryKind::Symlink => {
            let mut spec = ColorSpec::new();
            spec.set_fg(Some(Color::Cyan));
            out.set_color(&spec)?;
        }
        _ => {}
    }

    write!(out, "{}", entry.name)?;
    out.reset()?;

    if let Some(target) = &entry.symlink_target {
        write!(out, " -> {}", target)?;
    }
    if entry.loop_detected {
        write!(out, "  [skipped: circular link]")?;
    }
    if let Some(error) = &entry.error {
        write!(out, "  [error: {}]", error)?;
    }
    writeln!(out)?;
    Ok(())
}

fn write_csv_entry<W: Write>(out: &mut W, entry: &Entry) -> io::Result<()> {
    csv_escape(out, &entry.name)?;
    write!(out, ",")?;
    csv_escape(out, &entry.path)?;
    write!(out, ",{}", entry.depth)?;
    write!(out, ",{}", entry_kind_label(entry.kind))?;
    write!(out, ",")?;
    if let Some(size) = entry.size {
        write!(out, "{size}")?;
    }
    write!(out, ",")?;
    if let Some(mtime) = &entry.mtime {
        csv_escape(out, mtime)?;
    }
    write!(out, ",")?;
    if let Some(perm) = &entry.perm {
        csv_escape(out, perm)?;
    }
    write!(out, ",")?;
    if let Some(target) = &entry.symlink_target {
        csv_escape(out, target)?;
    }
    write!(out, ",{}", entry.loop_detected)?;
    write!(out, ",")?;
    if let Some(err) = &entry.error {
        csv_escape(out, err)?;
    }
    write!(out, ",")?;
    if let Some(status) = entry.git_status {
        write!(out, "{status}")?;
    }
    writeln!(out)?;
    Ok(())
}

fn csv_escape<W: Write>(out: &mut W, value: &str) -> io::Result<()> {
    let needs_escape = value.contains(',') || value.contains('\"') || value.contains('\n');
    if needs_escape {
        write!(out, "\"")?;
        for ch in value.chars() {
            if ch == '"' {
                write!(out, "\"\"")?;
            } else {
                write!(out, "{ch}")?;
            }
        }
        write!(out, "\"")?;
    } else {
        write!(out, "{value}")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// JSON 出力モード
// ---------------------------------------------------------------------
fn run_tree_json(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
) -> Result<()> {
    let mut stdout = BufWriter::new(std::io::stdout().lock());

    let mut root_meta = EntryMeta::from_path(root);
    git.apply(&mut root_meta);
    let root_security = canonical_root_for_security(root, &root_meta);
    let root_guard = root_security.as_deref();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Some(real) = root_security.clone() {
        visited.insert(real);
    } else {
        visited.insert(root.to_path_buf());
    }

    write!(&mut stdout, "[")?;
    let mut first = true;
    let mut emit = |entry: &Entry| -> io::Result<()> {
        if !first {
            write!(&mut stdout, ",")?;
        } else {
            first = false;
        }
        writeln!(&mut stdout)?;
        serde_json::to_writer(&mut stdout, entry)?;
        Ok(())
    };

    let root_entry = Entry::from_meta(&root_meta, 0);
    emit(&root_entry)?;

    if !root_meta.points_to_directory() || matches!(cli.max_depth, Some(1)) {
        writeln!(&mut stdout)?;
        write!(&mut stdout, "]\n")?;
        stdout.flush()?;
        return Ok(());
    }

    let mut stack: Vec<Frame> = Vec::new();
    if let Some(frame) = read_dir_frame(
        root,
        "",
        1,
        cli,
        include_glob,
        exclude_glob,
        filters,
        git,
        jobs,
    )? {
        stack.push(frame);
    }

    while let Some(frame) = stack.last_mut() {
        if frame.idx >= frame.entries.len() {
            stack.pop();
            continue;
        }

        let idx = frame.idx;
        let is_last = idx + 1 == frame.entries.len();
        let entry_meta = &mut frame.entries[idx];
        frame.idx += 1;

        let (entry, descend, child_prefix) = handle_entry(
            entry_meta,
            &frame.prefix,
            frame.depth,
            is_last,
            cli,
            &mut visited,
            root_guard,
        );

        emit(&entry)?;

        if descend {
            let child_path = entry_meta.path.clone();
            if let Some(frame) = read_dir_frame(
                &child_path,
                &child_prefix,
                frame.depth + 1,
                cli,
                include_glob,
                exclude_glob,
                filters,
                git,
                jobs,
            )? {
                stack.push(frame);
            }
        }
    }

    writeln!(&mut stdout)?;
    write!(&mut stdout, "]\n")?;
    stdout.flush()?;
    Ok(())
}

fn run_tree_ndjson(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
) -> Result<()> {
    let mut stdout = BufWriter::new(std::io::stdout().lock());

    let mut root_meta = EntryMeta::from_path(root);
    git.apply(&mut root_meta);
    let root_security = canonical_root_for_security(root, &root_meta);
    let root_guard = root_security.as_deref();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Some(real) = root_security.clone() {
        visited.insert(real);
    } else {
        visited.insert(root.to_path_buf());
    }

    let root_entry = Entry::from_meta(&root_meta, 0);
    serde_json::to_writer(&mut stdout, &root_entry)?;
    writeln!(&mut stdout)?;

    if !root_meta.points_to_directory() || matches!(cli.max_depth, Some(1)) {
        stdout.flush()?;
        return Ok(());
    }

    let mut stack: Vec<Frame> = Vec::new();
    if let Some(frame) = read_dir_frame(
        root,
        "",
        1,
        cli,
        include_glob,
        exclude_glob,
        filters,
        git,
        jobs,
    )? {
        stack.push(frame);
    }

    while let Some(frame) = stack.last_mut() {
        if frame.idx >= frame.entries.len() {
            stack.pop();
            continue;
        }

        let idx = frame.idx;
        let is_last = idx + 1 == frame.entries.len();
        let entry_meta = &mut frame.entries[idx];
        frame.idx += 1;

        let (entry, descend, child_prefix) = handle_entry(
            entry_meta,
            &frame.prefix,
            frame.depth,
            is_last,
            cli,
            &mut visited,
            root_guard,
        );

        serde_json::to_writer(&mut stdout, &entry)?;
        writeln!(&mut stdout)?;

        if descend {
            let child_path = entry_meta.path.clone();
            if let Some(frame) = read_dir_frame(
                &child_path,
                &child_prefix,
                frame.depth + 1,
                cli,
                include_glob,
                exclude_glob,
                filters,
                git,
                jobs,
            )? {
                stack.push(frame);
            }
        }
    }

    stdout.flush()?;
    Ok(())
}

fn run_tree_csv(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
) -> Result<()> {
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    writeln!(
        &mut stdout,
        "name,path,depth,kind,size,mtime,perm,symlink_target,loop_detected,error,git_status"
    )?;

    let mut root_meta = EntryMeta::from_path(root);
    git.apply(&mut root_meta);
    let root_security = canonical_root_for_security(root, &root_meta);
    let root_guard = root_security.as_deref();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Some(real) = root_security.clone() {
        visited.insert(real);
    } else {
        visited.insert(root.to_path_buf());
    }

    let root_entry = Entry::from_meta(&root_meta, 0);
    write_csv_entry(&mut stdout, &root_entry)?;

    if !root_meta.points_to_directory() || matches!(cli.max_depth, Some(1)) {
        stdout.flush()?;
        return Ok(());
    }

    let mut stack: Vec<Frame> = Vec::new();
    if let Some(frame) = read_dir_frame(
        root,
        "",
        1,
        cli,
        include_glob,
        exclude_glob,
        filters,
        git,
        jobs,
    )? {
        stack.push(frame);
    }

    while let Some(frame) = stack.last_mut() {
        if frame.idx >= frame.entries.len() {
            stack.pop();
            continue;
        }

        let idx = frame.idx;
        let is_last = idx + 1 == frame.entries.len();
        let entry_meta = &mut frame.entries[idx];
        frame.idx += 1;

        let (entry, descend, child_prefix) = handle_entry(
            entry_meta,
            &frame.prefix,
            frame.depth,
            is_last,
            cli,
            &mut visited,
            root_guard,
        );

        write_csv_entry(&mut stdout, &entry)?;

        let idx = frame.idx;
        let is_last = idx + 1 == frame.entries.len();
        let entry_meta = &mut frame.entries[idx];
        frame.idx += 1;

        let (entry, descend, child_prefix) = handle_entry(
            entry_meta,
            &frame.prefix,
            frame.depth,
            is_last,
            cli,
            &mut visited,
        );

        write_csv_entry(&mut stdout, &entry)?;

        if descend {
            let child_path = entry_meta.path.clone();
            if let Some(frame) = read_dir_frame(
                &child_path,
                &child_prefix,
                frame.depth + 1,
                cli,
                include_glob,
                exclude_glob,
                filters,
                git,
                jobs,
            )? {
                stack.push(frame);
            }
        }
    }

    stdout.flush()?;
    Ok(())
}

fn run_tree_yaml(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
) -> Result<()> {
    let mut root_meta = EntryMeta::from_path(root);
    git.apply(&mut root_meta);
    let root_security = canonical_root_for_security(root, &root_meta);
    let root_guard = root_security.as_deref();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Some(real) = root_security.clone() {
        visited.insert(real);
    } else {
        visited.insert(root.to_path_buf());
    }

    let mut root_entry = Entry::from_meta(&root_meta, 0);
    let mut children = Vec::new();

    if root_meta.points_to_directory() && !matches!(cli.max_depth, Some(1)) {
        children = build_yaml_children(
            &root_meta,
            1,
            cli,
            include_glob,
            exclude_glob,
            filters,
            git,
            jobs,
            &mut visited,
            root_guard,
        )?;

        let mut total = 0u64;
        let mut has_sizes = false;
        for child in &children {
            if let Some(size) = child.entry.size {
                total = total.saturating_add(size);
                has_sizes = true;
            }
        }
        if has_sizes {
            root_entry.size = Some(total);
        } else {
            root_entry.size = None;
        }
    }

    let doc = YamlNode {
        entry: root_entry,
        children,
    };

    let mut stdout = BufWriter::new(std::io::stdout().lock());
    write_yaml_node(&mut stdout, &doc, 0, false)?;
    stdout.flush()?;
    Ok(())
}

fn build_yaml_children(
    parent_meta: &EntryMeta,
    depth: usize,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
    visited: &mut HashSet<PathBuf>,
    root_guard: Option<&Path>,
) -> Result<Vec<YamlNode>> {
    let mut nodes = Vec::new();
    if let Some(frame) = read_dir_frame(
        &parent_meta.path,
        "",
        depth,
        cli,
        include_glob,
        exclude_glob,
        filters,
        git,
        jobs,
    )? {
        for mut meta in frame.entries.into_iter() {
            let node = build_yaml_node(
                &mut meta,
                frame.depth,
                cli,
                include_glob,
                exclude_glob,
                filters,
                git,
                jobs,
                visited,
                root_guard,
            )?;
            nodes.push(node);
        }
    }

    Ok(nodes)
}

fn build_yaml_node(
    meta: &mut EntryMeta,
    depth: usize,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
    visited: &mut HashSet<PathBuf>,
    root_guard: Option<&Path>,
) -> Result<YamlNode> {
    let (mut entry, descend, _child_prefix) =
        handle_entry(meta, "", depth, true, cli, visited, root_guard);

    let mut children = Vec::new();
    if descend {
        entry.size = None;
        children = build_yaml_children(
            meta,
            depth + 1,
            cli,
            include_glob,
            exclude_glob,
            filters,
            git,
            jobs,
            visited,
            root_guard,
        )?;
        let mut total = 0u64;
        let mut has_sizes = false;
        for child in &children {
            if let Some(size) = child.entry.size {
                total = total.saturating_add(size);
                has_sizes = true;
            }
        }
        if has_sizes {
            entry.size = Some(total);
        }
    }

    Ok(YamlNode { entry, children })
}

fn run_tree_html(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
) -> Result<()> {
    let entries = collect_entries_flat(root, cli, include_glob, exclude_glob, filters, git, jobs)?;
    let json = serde_json::to_string(&entries)?;
    let escaped = escape_script_data(&json);

    let mut stdout = BufWriter::new(std::io::stdout().lock());
    writeln!(&mut stdout, "<!DOCTYPE html>")?;
    writeln!(&mut stdout, "<html lang=\"en\">")?;
    writeln!(&mut stdout, "<head>")?;
    writeln!(&mut stdout, "  <meta charset=\"utf-8\">")?;
    writeln!(&mut stdout, "  <title>printree</title>")?;
    writeln!(
        &mut stdout,
        "  <style>body {{ font-family: monospace; white-space: pre; margin: 2rem; }}</style>"
    )?;
    writeln!(&mut stdout, "</head>")?;
    writeln!(&mut stdout, "<body>")?;
    writeln!(
        &mut stdout,
        "<script type=\"application/json\" id=\"tree-data\">{}</script>",
        escaped
    )?;
    writeln!(&mut stdout, "<pre id=\"tree-output\"></pre>")?;
    writeln!(
        &mut stdout,
        "<script>const data=JSON.parse(document.getElementById('tree-data').textContent);\nconst lines=data.map(e=>`${{'    '.repeat(e.depth)}}${{e.name}}`);\ndocument.getElementById('tree-output').textContent=lines.join('\\n');</script>"
    )?;
    writeln!(&mut stdout, "</body>")?;
    writeln!(&mut stdout, "</html>")?;
    stdout.flush()?;
    Ok(())
}

fn collect_entries_flat(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
) -> Result<Vec<Entry>> {
    let mut root_meta = EntryMeta::from_path(root);
    git.apply(&mut root_meta);
    let root_security = canonical_root_for_security(root, &root_meta);
    let root_guard = root_security.as_deref();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Some(real) = root_security.clone() {
        visited.insert(real);
    } else {
        visited.insert(root.to_path_buf());
    }

    let mut entries = Vec::new();
    entries.push(Entry::from_meta(&root_meta, 0));

    if !root_meta.points_to_directory() || matches!(cli.max_depth, Some(1)) {
        return Ok(entries);
    }

    let mut stack: Vec<Frame> = Vec::new();
    if let Some(frame) = read_dir_frame(
        root,
        "",
        1,
        cli,
        include_glob,
        exclude_glob,
        filters,
        git,
        jobs,
    )? {
        stack.push(frame);
    }

    while let Some(frame) = stack.last_mut() {
        if frame.idx >= frame.entries.len() {
            stack.pop();
            continue;
        }

        let idx = frame.idx;
        let is_last = idx + 1 == frame.entries.len();
        let entry_meta = &mut frame.entries[idx];
        frame.idx += 1;

        let (entry, descend, child_prefix) = handle_entry(
            entry_meta,
            &frame.prefix,
            frame.depth,
            is_last,
            cli,
            &mut visited,
            root_guard,
        );

        if descend {
            let child_path = entry_meta.path.clone();
            if let Some(frame) = read_dir_frame(
                &child_path,
                &child_prefix,
                frame.depth + 1,
                cli,
                include_glob,
                exclude_glob,
                filters,
                git,
                jobs,
            )? {
                stack.push(frame);
            }
        }

        entries.push(entry);
    }

    Ok(entries)
}

fn escape_script_data(data: &str) -> String {
    data.replace("</script", "<\\/script")
}

fn write_yaml_node<W: Write>(
    out: &mut W,
    node: &YamlNode,
    indent: usize,
    with_dash: bool,
) -> io::Result<()> {
    let indent_str = " ".repeat(indent);
    let (line_prefix, child_indent) = if with_dash {
        (format!("{}- ", indent_str), indent + 2)
    } else {
        (indent_str.clone(), indent)
    };

    writeln!(
        out,
        "{}name: {}",
        line_prefix,
        serde_json::to_string(&node.entry.name).unwrap()
    )?;
    write_yaml_fields(out, child_indent, node)?;
    Ok(())
}

fn write_yaml_fields<W: Write>(out: &mut W, indent: usize, node: &YamlNode) -> io::Result<()> {
    let indent_str = " ".repeat(indent);
    yaml_write_string(out, indent, "path", &node.entry.path)?;
    writeln!(out, "{}depth: {}", indent_str, node.entry.depth)?;
    writeln!(
        out,
        "{}kind: {}",
        indent_str,
        entry_kind_label(node.entry.kind)
    )?;
    if let Some(size) = node.entry.size {
        writeln!(out, "{}size: {}", indent_str, size)?;
    }
    if let Some(mtime) = &node.entry.mtime {
        yaml_write_string(out, indent, "mtime", mtime)?;
    }
    if let Some(perm) = &node.entry.perm {
        yaml_write_string(out, indent, "perm", perm)?;
    }
    if let Some(target) = &node.entry.symlink_target {
        yaml_write_string(out, indent, "symlink_target", target)?;
    }
    writeln!(
        out,
        "{}loop_detected: {}",
        indent_str, node.entry.loop_detected
    )?;
    if let Some(err) = &node.entry.error {
        yaml_write_string(out, indent, "error", err)?;
    }
    if let Some(status) = node.entry.git_status {
        writeln!(out, "{}git_status: {}", indent_str, status)?;
    }
    if !node.children.is_empty() {
        writeln!(out, "{}children:", indent_str)?;
        for child in &node.children {
            write_yaml_node(out, child, indent + 2, true)?;
        }
    }
    Ok(())
}

fn yaml_write_string<W: Write>(
    out: &mut W,
    indent: usize,
    key: &str,
    value: &str,
) -> io::Result<()> {
    let indent_str = " ".repeat(indent);
    let quoted = serde_json::to_string(value).unwrap();
    writeln!(out, "{}{}: {}", indent_str, key, quoted)
}

fn entry_kind_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::Symlink => "symlink",
        EntryKind::Unknown => "unknown",
    }
}

// ---------------------------------------------------------------------
// ヘルパー関数
// ---------------------------------------------------------------------
fn read_dir_frame(
    path: &Path,
    prefix: &str,
    depth: usize,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
    filters: &Filters,
    git: &GitTracker,
    jobs: &JobPool,
) -> Result<Option<Frame>> {
    let rd = match fs::read_dir(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} [permission denied: {}]", path.display(), e);
            return Ok(None);
        }
    };

    let mut seeds: Vec<EntrySeed> = Vec::new();
    for e in rd {
        match e {
            Ok(de) => {
                let file_name = de.file_name();
                if !cli.hidden && is_hidden(&file_name) {
                    continue;
                }

                let (file_type_hint, file_type_error) = match de.file_type() {
                    Ok(ft) => (Some(ft), None),
                    Err(err) => (None, Some(err.to_string())),
                };

                if let Some(ft) = file_type_hint {
                    if !allow_type(&ft, &cli.types) {
                        continue;
                    }
                }

                let fullp = de.path();
                if !match_globs(path, &fullp, include_glob, exclude_glob, cli.match_mode) {
                    continue;
                }

                seeds.push(EntrySeed {
                    path: fullp,
                    name: file_name,
                    file_type_hint,
                    file_type_error,
                });
            }
            Err(err) => {
                eprintln!("[read_dir error] {}: {err}", path.display());
            }
        }
    }

    if seeds.is_empty() {
        return Ok(None);
    }

    let metas = build_entry_metas(seeds, jobs);
    let mut entries = Vec::new();
    for mut meta in metas {
        if !filters.allows(&meta) {
            continue;
        }
        git.apply(&mut meta);
        entries.push(meta);
    }

    if entries.is_empty() {
        return Ok(None);
    }

    if matches!(cli.sort, SortMode::Name) {
        entries.sort_by(|a, b| a.sort_key().cmp(b.sort_key()));
    }
    if cli.dirs_first {
        entries.sort_by(|a, b| {
            let ad = a.points_to_directory();
            let bd = b.points_to_directory();
            bd.cmp(&ad).then_with(|| a.sort_key().cmp(b.sort_key()))
        });
    }

    Ok(Some(Frame {
        entries,
        idx: 0,
        prefix: prefix.to_string(),
        depth,
    }))
}

fn build_entry_metas(seeds: Vec<EntrySeed>, jobs: &JobPool) -> Vec<EntryMeta> {
    if seeds.is_empty() {
        return Vec::new();
    }

    if !jobs.is_parallel() || seeds.len() <= 1 {
        return seeds.into_iter().map(EntryMeta::from_seed).collect();
    }

    let workers = jobs.workers().min(seeds.len());
    let chunk = (seeds.len() + workers - 1) / workers;
    let mut results: Vec<EntryMeta> = Vec::with_capacity(seeds.len());

    thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        for chunk_slice in seeds.chunks(chunk.max(1)) {
            let tx = tx.clone();
            let chunk_vec: Vec<EntrySeed> = chunk_slice.to_vec();
            scope.spawn(move || {
                let metas: Vec<EntryMeta> =
                    chunk_vec.into_iter().map(EntryMeta::from_seed).collect();
                let _ = tx.send(metas);
            });
        }
        drop(tx);
        for mut part in rx {
            results.append(&mut part);
        }
    });

    results
}
