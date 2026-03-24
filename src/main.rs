//! Replay — self-improving software orchestration.

mod agent;
mod beads;
mod display;
mod engine;
mod session;
mod tui;

pub const VERSION: &str = "0.1.0";

use std::path::PathBuf;

use anyhow::Result;
use rustyline::DefaultEditor;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    let resume = args.iter().any(|a| a == "-r");
    let list_sessions = args.iter().any(|a| a == "--resume");
    let list_all = args.iter().any(|a| a == "--all");
    let target = std::env::current_dir()?;

    // --resume: list sessions
    if list_sessions {
        let sessions = if list_all {
            session::list_all()?
        } else {
            session::list_for_project(&target)?
        };
        print!("{}", session::format_list(&sessions));
        return Ok(());
    }

    let mut rl = DefaultEditor::new()?;
    let replay_dir = dirs::home_dir()
        .expect("no home directory")
        .join(".replay");
    std::fs::create_dir_all(&replay_dir)?;
    let history_path = replay_dir.join("history");
    let _ = rl.load_history(&history_path);

    // -r: resume last session
    let (mut conversation, mut session_path) = if resume {
        match session::latest(&target)? {
            Some(path) => {
                let history = session::load(&path)?;
                let turns = history.len() / 2;
                eprintln!("Resuming session ({turns} turns): {}", path.display());
                (history, Some(path))
            }
            None => {
                eprintln!("No previous session found.");
                (Vec::new(), None)
            }
        }
    } else {
        (Vec::new(), None)
    };

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

                let (callback, display_state) = display::create_callback(verbose);

                tokio::select! {
                    result = agent::execute(instruction, &target, callback, &mut conversation) => {
                        display_state.lock().unwrap().flush();
                        if let Err(e) = result {
                            eprintln!("error: {e:#}");
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        display_state.lock().unwrap().flush();
                        eprintln!("\ninterrupted");
                    }
                }

                // Auto-save after each turn
                match session::save(&target, session_path.as_deref(), &conversation) {
                    Ok(path) => session_path = Some(path),
                    Err(e) => eprintln!("warning: failed to save session: {e:#}"),
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
