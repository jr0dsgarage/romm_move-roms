use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

mod app;
mod classifier;
mod model;
mod scanner;
mod tui;

use model::TransferMode;

#[derive(Debug, Parser)]
#[command(author, version, about = "Scan and preview ROM file moves into RomM folder structure")]
struct Cli {
    /// Source library root. Output will target <source>/roms.
    source: PathBuf,

    /// Move files instead of copying them.
    #[arg(short = 'm', long = "move")]
    move_files: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let transfer_mode = if cli.move_files {
        TransferMode::Move
    } else {
        TransferMode::Copy
    };

    app::run(cli.source, transfer_mode)
}
