use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
#[derive(Parser, Debug)]
#[command(name = "printree")]
#[command(about = "Print directory tree recursively", long_about = None)]
#[command(
    version,
    about = "Fast, memory-light directory tree & git diff printer"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Cmd>,

    /// Root path (tree mode)
    pub path: Option<PathBuf>,

    /// Max depth (1 = only root)
    #[arg(long)]
    pub max_depth: Option<usize>,

    /// Show dotfiles
    #[arg(long, action = ArgAction::SetTrue)]
    pub hidden: bool,

    /// Follow symlinks
    #[arg(long, action = ArgAction::SetTrue)]
    pub follow_symlinks: bool,

    /// Sort mode
    #[arg(long, value_enum, default_value_t = SortMode::None)]
    pub sort: SortMode,

    /// Directories first when sorting
    #[arg(long, action = ArgAction::SetTrue)]
    pub dirs_first: bool,

    /// Include glob(s). Multiple allowed.
    #[arg(long = "include")]
    pub includes: Vec<String>,

    /// Exclude glob(s). Multiple allowed.
    #[arg(long = "exclude")]
    pub excludes: Vec<String>,

    /// Pattern syntax for include/exclude filters
    #[arg(long = "pattern-syntax", value_enum, default_value_t = PatternSyntax::Glob)]
    pub pattern_syntax: PatternSyntax,

    /// name/path match basis for globs
    #[arg(long, value_enum, default_value_t = MatchMode::Name)]
    pub match_mode: MatchMode,

    /// Regex filter applied after glob matching
    #[arg(long = "filter-regex")]
    pub filter_regex: Option<String>,

    /// File size filter, e.g. ">1MB" or "<=10k"
    #[arg(long = "filter-size")]
    pub filter_size: Option<String>,

    /// Modified-time filter window like "3d", "10m", "2h"
    #[arg(long = "filter-mtime")]
    pub filter_mtime: Option<String>,

    /// Permission filter (octal, e.g. 755)
    #[arg(long = "filter-perm")]
    pub filter_perm: Option<String>,

    /// Type filter: file|dir|symlink (repeatable)
    #[arg(long = "type", value_enum)]
    pub types: Vec<TypeFilter>,

    /// Use .gitignore rules
    #[arg(long, value_enum, default_value_t = GitignoreMode::Off)]
    pub gitignore: GitignoreMode,

    /// Color output
    #[arg(long, value_enum, default_value_t = ColorMode::Never)]
    pub color: ColorMode,

    /// Output format
    #[arg(long, value_enum, default_value_t = Format::Plain)]
    pub format: Format,

    /// Output text encoding

    #[arg(
        long,
        value_enum,
        default_value = "utf8",
        help = "Output encoding: utf8 | utf8bom | utf16le | sjis | auto"
    )]
    pub encoding: EncodingMode,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Diff two git revisions
    Diff {
        #[arg(long = "rev-a")]
        rev_a: String,
        #[arg(long = "rev-b")]
        rev_b: String,
        #[arg(long)]
        path: Option<PathBuf>,

        /// Output format (plain/json)
        #[arg(long, value_enum, default_value_t = Format::Plain)]
        format: Format,
    },
}

#[derive(ValueEnum, Clone, Debug)]
pub enum EncodingMode {
    Utf8,
    Utf8bom,
    Utf16le,
    Sjis,
    Auto,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum SortMode {
    None,
    Name,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum MatchMode {
    Name,
    Path,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum PatternSyntax {
    Glob,
    Regex,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum TypeFilter {
    File,
    Dir,
    Symlink,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum GitignoreMode {
    On,
    Off,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
pub enum Format {
    Plain,
    Json,
    Ndjson,
    Csv,
    Yaml,
    Html,
}
