mod cli;
mod core;
mod utils;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Cmd, GitignoreMode};

#[cfg(windows)]
fn enable_utf8_output() {
    use windows_sys::Win32::System::Console::SetConsoleOutputCP;
    unsafe {
        SetConsoleOutputCP(65001);
    }
}

#[cfg(windows)]
fn set_console_encoding(encoding: &crate::cli::EncodingMode) {
    use windows_sys::Win32::System::Console::SetConsoleOutputCP;
    unsafe {
        match encoding {
            crate::cli::EncodingMode::Utf8 => {
                SetConsoleOutputCP(65001); // UTF-8
            }
            crate::cli::EncodingMode::Sjis => {
                SetConsoleOutputCP(932); // CP932 (Shift-JIS)
            }
            crate::cli::EncodingMode::Auto => {
                // Do nothing, use system default
            }
        }
    }
}

fn main() -> Result<()> {
    #[cfg(windows)]
    enable_utf8_output();
    let cli = Cli::parse();
    
    #[cfg(windows)]
    set_console_encoding(&cli.encoding);

    match &cli.cmd {
        Some(Cmd::Diff { rev_a, rev_b, path, format }) => {
            core::diff::run_diff(rev_a, rev_b, path.as_deref(), *format)
        }
        None => match cli.gitignore {
            GitignoreMode::On => core::tree_gitignore::run_tree_gitignore(&cli),
            GitignoreMode::Off => core::tree::run_tree(&cli),
        },
    }
}
