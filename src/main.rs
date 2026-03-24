//! Replay — self-improving software orchestration.

mod agent;
mod beads;
mod engine;
mod tui;

pub const VERSION: &str = "0.1.0";

use anyhow::Result;
use rustyline::DefaultEditor;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let verbose = std::env::args().any(|a| a == "-v" || a == "--verbose");
    let target = std::env::current_dir()?;

    let mut rl = DefaultEditor::new()?;
    let replay_dir = dirs::home_dir()
        .expect("no home directory")
        .join(".replay");
    std::fs::create_dir_all(&replay_dir)?;
    let history_path = replay_dir.join("history");
    let _ = rl.load_history(&history_path);

    loop {
        let prompt = "\n\x1b[48;5;236m \u{203a} \x1b[0m ";
        match rl.readline(prompt) {
            Ok(line) => {
                let instruction = line.trim();
                if instruction.is_empty() {
                    continue;
                }

                rl.add_history_entry(instruction)?;
                println!();

                if let Err(e) = agent::execute(instruction, &target, verbose).await {
                    eprintln!("error: {e:#}");
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof) => {
                let _ = rl.save_history(&history_path);
                break;
            }
            Err(e) => {
                eprintln!("error: {e:#}");
                break;
            }
        }
    }

    Ok(())
}
