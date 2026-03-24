//! Replay — self-improving software orchestration.

mod agent;
mod beads;
mod engine;
mod tui;

pub const VERSION: &str = "0.1.0";

use anyhow::Result;

fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tui::run()
}
