use encoding_rs::{Encoding, SHIFT_JIS, UTF_16LE};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, DirEntry, FileType, Metadata};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::Serialize;
use termcolor::{Color, ColorSpec, StandardStream, WriteColor};

use crate::cli::{Cli, Format, SortMode};
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

    match cli.format {
        Format::Json => run_tree_json(&root, cli, &include_glob, &exclude_glob),
        Format::Plain => run_tree_plain(&root, cli, &include_glob, &exclude_glob),
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
    perm_win: Option<u32>,
    is_symlink: bool,
    symlink_target: Option<PathBuf>,
    canonical_path: Option<PathBuf>,
    loop_detected: bool,
    error: Option<String>,
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

impl EntryMeta {
    fn build(
        entry: DirEntry,
        file_type_hint: Option<FileType>,
        file_type_error: Option<String>,
    ) -> Self {
        let path = entry.path();
        let name = entry.file_name();
        let mut errors = Vec::new();
        if let Some(err) = file_type_error {
            errors.push(err);
        }

        let mut file_type = file_type_hint;
        if file_type.is_none() {
            if let Ok(ft) = entry.file_type() {
                file_type = Some(ft);
            }
        }

        let metadata = entry
            .metadata()
            .map_err(|err| {
                errors.push(err.to_string());
                err
            })
            .ok();

        if file_type.is_none() {
            file_type = metadata.as_ref().map(|m| m.file_type());
        }

        let is_symlink = file_type.map(|ft| ft.is_symlink()).unwrap_or(false);
        Self::construct(path, name, file_type, metadata, is_symlink, errors)
    }

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

        let perm = meta
            .perm_unix
            .map(|perm| format!("{perm:03o}"))
            .or_else(|| meta.perm_win.map(|perm| format!("{perm:08X}")));

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
) -> (Entry, bool, String) {
    let mut loop_detected = false;
    let mut descend = entry_meta.points_to_directory();
    let mut canonical_to_record: Option<PathBuf> = None;

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
) -> Result<()> {
    let mut out = make_encoded_writer(cli);
    let mut bold = ColorSpec::new();
    bold.set_bold(true);
    out.set_color(&bold)?;
    writeln!(&mut out, "{}", root.display())?;
    out.reset()?;

    let root_meta = EntryMeta::from_path(root);

    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Some(real) = root_meta
        .canonical_path
        .clone()
        .or_else(|| fs::canonicalize(root).ok())
    {
        visited.insert(real);
    } else {
        visited.insert(root.to_path_buf());
    }

    if !root_meta.points_to_directory() || matches!(cli.max_depth, Some(1)) {
        return Ok(());
    }

    let mut stack: Vec<Frame> = Vec::new();
    if let Some(frame) = read_dir_frame(root, "", 1, cli, include_glob, exclude_glob)? {
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
        );

        write_plain_entry(out.as_mut(), &frame.prefix, &entry, is_last)?;

        if descend {
            let child_path = entry_meta.path.clone();
            if let Some(frame) = read_dir_frame(
                &child_path,
                &child_prefix,
                frame.depth + 1,
                cli,
                include_glob,
                exclude_glob,
            )? {
                stack.push(frame);
            }
        }
    }

    Ok(())
}

fn write_plain_entry(
    out: &mut dyn WriteColor,
    prefix: &str,
    entry: &Entry,
    is_last: bool,
) -> io::Result<()> {
    let connector = if is_last { "└── " } else { "├── " };
    write!(out, "{}{}", prefix, connector)?;

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

// ---------------------------------------------------------------------
// JSON 出力モード
// ---------------------------------------------------------------------
fn run_tree_json(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<PatternList>,
    exclude_glob: &Option<PatternList>,
) -> Result<()> {
    let mut stdout = BufWriter::new(std::io::stdout().lock());

    let root_meta = EntryMeta::from_path(root);
    let mut visited: HashSet<PathBuf> = HashSet::new();
    if let Some(real) = root_meta
        .canonical_path
        .clone()
        .or_else(|| fs::canonicalize(root).ok())
    {
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
    if let Some(frame) = read_dir_frame(root, "", 1, cli, include_glob, exclude_glob)? {
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
            )? {
                stack.push(frame);
            }
        }
    }

    stdout.flush()?;
    Ok(())
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

                let meta = EntryMeta::build(de, file_type_hint, file_type_error);
                entries.push(meta);
            }
            Err(err) => {
                eprintln!("[read_dir error] {}: {err}", path.display());
            }
        }
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
