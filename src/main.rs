//! Replay — self-improving software orchestration.
//!
//! `replay` always launches the Rewind TUI. All interaction happens there.
//! The engine runs only when the user explicitly triggers it from the TUI.

mod agent;
mod beads;
pub mod engine;

pub const VERSION: &str = "0.1.0";

use anyhow::{Context, Result};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

fn parse_args() -> PathBuf {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--target" {
            if i + 1 < args.len() {
                return PathBuf::from(&args[i + 1]);
            }
            eprintln!("error: --target requires a path");
            std::process::exit(1);
        }
        i += 1;
    }
    std::env::current_dir().expect("failed to get current directory")
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let _target = parse_args().canonicalize()
        .context("--target path does not exist")?;

    // Replay always launches the TUI. The user drives everything from there.
    launch_rewind()
}

/// Launch the Rewind TUI binary.
fn launch_rewind() -> Result<()> {
    let status = std::process::Command::new("rewind")
        .spawn()
        .context("failed to launch rewind — is it installed?")?
        .wait()
        .context("rewind process failed")?;

    if !status.success() {
        anyhow::bail!("rewind exited with status: {status}");
    }

    Ok(())
}
