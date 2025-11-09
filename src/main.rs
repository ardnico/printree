mod cli;
mod core;
mod utils;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Cmd, GitignoreMode};

fn main() -> Result<()> {
    let cli = Cli::parse();

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
