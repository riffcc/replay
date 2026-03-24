//! Replay — self-improving software orchestration.

mod agent;
mod beads;
mod engine;
mod tui;

pub const VERSION: &str = "0.1.0";

use std::io::{self, BufRead, Write};

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let target = std::env::current_dir()?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        write!(stdout, "\n\x1b[48;5;236m \u{203a} \x1b[0m ")?;
        stdout.flush()?;

        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }

        println!();

        let instruction = line.trim();
        if instruction.is_empty() {
            continue;
        }

        if let Err(e) = agent::execute(instruction, &target).await {
            eprintln!("error: {e:#}");
        }
    }

    Ok(())
}
