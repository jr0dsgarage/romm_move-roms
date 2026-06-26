use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

mod app;
mod classifier;
mod model;
mod scanner;
mod tui;

#[derive(Debug, Parser)]
#[command(author, version, about = "Scan and preview ROM file moves into RomM folder structure")]
struct Cli {
    /// Source library root. Output will target <source>/roms.
    source: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    app::run(cli.source)
}
