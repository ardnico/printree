mod cli;
mod core;
mod utils;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Cmd};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.cmd {
        Some(Cmd::Diff { rev_a, rev_b, path }) => core::diff::run_diff(rev_a, rev_b, path.as_deref()),
        None => match cli.gitignore {
            cli::GitignoreMode::On => core::tree_gitignore::run_tree_gitignore(&cli),
            cli::GitignoreMode::Off => core::tree::run_tree(&cli),
        },
    }
}
