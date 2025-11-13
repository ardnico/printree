use encoding_rs::{Encoding, SHIFT_JIS, UTF_16LE};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;
use termcolor::{Color, ColorSpec, StandardStream, WriteColor};

use crate::cli::{Cli, Format, SortMode};
use crate::utils::{allow_type, build_globset, color_choice, is_hidden, match_globs, Frame};

/// ディレクトリツリーのメイン実行関数
pub fn run_tree(cli: &Cli) -> Result<()> {
    let root = cli.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let include_glob = build_globset(&cli.includes)?;
    let exclude_glob = build_globset(&cli.excludes)?;

    if cli.format == Format::Json {
        run_tree_json(&root, cli, &include_glob, &exclude_glob)
    } else {
        run_tree_plain(&root, cli, &include_glob, &exclude_glob)
    }
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
    include_glob: &Option<globset::GlobSet>,
    exclude_glob: &Option<globset::GlobSet>,
) -> Result<()> {
    let mut out = make_encoded_writer(cli);
    out.set_color(ColorSpec::new().set_bold(true))?;
    writeln!(&mut out, "{}", root.display())?;
    out.reset()?;

    let mut visited: HashSet<PathBuf> = HashSet::new();
    let root_real = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    visited.insert(root_real.clone());

    let mut stack: Vec<Frame> = Vec::new();
    if let Some(frame) = read_dir_frame(root, "", 1, cli, include_glob, exclude_glob)? {
        stack.push(frame);
    }

    while let Some(top) = stack.last_mut() {
        if top.idx >= top.entries.len() {
            stack.pop();
            continue;
        }

        let is_last = top.idx + 1 == top.entries.len();
        let entry = &top.entries[top.idx];
        top.idx += 1;

        let name = entry.file_name();
        let connector = if is_last { "└── " } else { "├── " };

        let ty = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                writeln!(
                    &mut out,
                    "{}{}{}  [permission denied: {}]",
                    top.prefix,
                    connector,
                    name.to_string_lossy(),
                    e
                )?;
                continue;
            }
        };

        if ty.is_dir() {
            out.set_color(ColorSpec::new().set_fg(Some(Color::Blue)))?;
        } else if ty.is_symlink() {
            out.set_color(ColorSpec::new().set_fg(Some(Color::Cyan)))?;
        }

        write!(&mut out, "{}{}", top.prefix, connector)?;
        out.reset()?;
        write!(&mut out, "{}", name.to_string_lossy())?;

        // ---- シンボリックリンク検出とループ防止 ----
        if ty.is_symlink() {
            match fs::canonicalize(entry.path()) {
                Ok(real_path) => {
                    if visited.contains(&real_path) {
                        writeln!(
                            &mut out,
                            " -> {}  [skipped: circular link]",
                            real_path.display()
                        )?;
                        continue;
                    } else {
                        writeln!(&mut out, " -> {}", real_path.display())?;
                    }
                }
                Err(_) => {
                    writeln!(&mut out, " -> [broken symlink]")?;
                    continue;
                }
            }
        } else {
            writeln!(&mut out)?;
        }

        // 子階層プレフィックス
        let child_prefix = if is_last {
            format!("{}    ", top.prefix)
        } else {
            format!("{}│   ", top.prefix)
        };

        // 降下するか判定
        let mut descend = ty.is_dir();
        if !descend && ty.is_symlink() && cli.follow_symlinks {
            if let Ok(target_real) = fs::canonicalize(entry.path()) {
                if visited.insert(target_real.clone()) {
                    if let Ok(meta) = fs::metadata(entry.path()) {
                        if meta.is_dir() {
                            descend = true;
                        }
                    }
                } else {
                    continue;
                }
            } else {
                continue;
            }
        }

        if descend {
            if let Some(maxd) = cli.max_depth {
                if top.depth + 1 > maxd {
                    continue;
                }
            }
            if let Some(frame) = read_dir_frame(
                &entry.path(),
                &child_prefix,
                top.depth + 1,
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

// ---------------------------------------------------------------------
// JSON 出力モード
// ---------------------------------------------------------------------
fn run_tree_json(
    root: &Path,
    cli: &Cli,
    include_glob: &Option<globset::GlobSet>,
    exclude_glob: &Option<globset::GlobSet>,
) -> Result<()> {
    #[derive(Serialize)]
    struct JsonEntry<'a> {
        path: &'a str,
        name: &'a str,
        depth: usize,
        kind: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        symlink_target: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        loop_detected: Option<bool>,
    }

    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    let mut visited: HashSet<PathBuf> = HashSet::new();

    let root_real = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    visited.insert(root_real.clone());

    // ルート出力
    let root_display = root.display().to_string();
    let root_name = root.file_name().and_then(|s| s.to_str()).unwrap_or(".");
    serde_json::to_writer(
        &mut stdout,
        &JsonEntry {
            path: &root_display,
            name: root_name,
            depth: 0,
            kind: "dir",
            symlink_target: None,
            error: None,
            loop_detected: None,
        },
    )?;
    writeln!(&mut stdout)?;

    let root_ft = fs::symlink_metadata(root).ok().map(|m| m.file_type());
    if root_ft.map_or(false, |ft| !ft.is_dir()) || matches!(cli.max_depth, Some(1)) {
        stdout.flush()?;
        return Ok(());
    }

    let mut stack: Vec<Frame> = Vec::new();
    if let Some(frame) = read_dir_frame(root, "", 1, cli, include_glob, exclude_glob)? {
        stack.push(frame);
    }

    while let Some(top) = stack.last_mut() {
        if top.idx >= top.entries.len() {
            stack.pop();
            continue;
        }

        let entry = &top.entries[top.idx];
        top.idx += 1;

        let name_os = entry.file_name();
        let name = name_os.to_string_lossy();
        let path = entry.path();
        let path_s = path.display().to_string();

        match entry.file_type() {
            Ok(ft) => {
                let mut kind = "file";
                if ft.is_dir() {
                    kind = "dir";
                } else if ft.is_symlink() {
                    kind = "symlink";
                }

                let mut symlink_target: Option<String> = None;
                let mut loop_detected: Option<bool> = None;

                if ft.is_symlink() {
                    match fs::canonicalize(&path) {
                        Ok(real_path) => {
                            let real_str = real_path.display().to_string();
                            if visited.contains(&real_path) {
                                loop_detected = Some(true);
                            } else {
                                visited.insert(real_path);
                            }
                            symlink_target = Some(real_str);
                        }
                        Err(_) => {
                            symlink_target = Some("[broken symlink]".into());
                        }
                    }
                }

                serde_json::to_writer(
                    &mut stdout,
                    &JsonEntry {
                        path: &path_s,
                        name: &name,
                        depth: top.depth,
                        kind,
                        symlink_target: symlink_target.as_deref(),
                        error: None,
                        loop_detected,
                    },
                )?;
                writeln!(&mut stdout)?;

                // 降下判定
                let mut descend = ft.is_dir();
                if !descend && ft.is_symlink() && cli.follow_symlinks && loop_detected != Some(true)
                {
                    if let Ok(m) = fs::metadata(&path) {
                        descend = m.is_dir();
                    }
                }

                if descend {
                    if let Some(maxd) = cli.max_depth {
                        if top.depth + 1 > maxd {
                            continue;
                        }
                    }
                    if let Some(frame) =
                        read_dir_frame(&path, "", top.depth + 1, cli, include_glob, exclude_glob)?
                    {
                        stack.push(frame);
                    }
                }
            }
            Err(e) => {
                let e_str = e.to_string();
                serde_json::to_writer(
                    &mut stdout,
                    &JsonEntry {
                        path: &path_s,
                        name: &name,
                        depth: top.depth,
                        kind: "unknown",
                        symlink_target: None,
                        error: Some(&e_str),
                        loop_detected: None,
                    },
                )?;
                writeln!(&mut stdout)?;
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
    include_glob: &Option<globset::GlobSet>,
    exclude_glob: &Option<globset::GlobSet>,
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
                let name = de.file_name();
                if !cli.hidden && is_hidden(&name) {
                    continue;
                }
                if let Ok(ft) = de.file_type() {
                    if !allow_type(&ft, &cli.types) {
                        continue;
                    }
                }
                let fullp = de.path();
                if !match_globs(path, &fullp, include_glob, exclude_glob, cli.match_mode) {
                    continue;
                }
                entries.push(de);
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
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    }
    if cli.dirs_first {
        entries.sort_by(|a, b| {
            let ad = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let bd = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
            bd.cmp(&ad).then_with(|| a.file_name().cmp(&b.file_name()))
        });
    }

    Ok(Some(Frame {
        path: path.to_path_buf(),
        entries,
        idx: 0,
        prefix: prefix.to_string(),
        depth,
    }))
}
