//! Entrypoint for the `zerovdd` CLI binary.

use anyhow::Result;
use clap::Parser;

mod cmd;

fn main() -> Result<()> {
    let cli = cmd::Cli::parse();
    cmd::run(cli)
}
