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
    let resume_latest = args.iter().any(|a| a == "-r");
    let list_all = args.iter().any(|a| a == "--all");
    let target = std::env::current_dir()?;

    // --resume: with number resumes that session, without number lists
    let resume_index = args.iter().position(|a| a == "--resume").and_then(|pos| {
        args.get(pos + 1).and_then(|v| v.parse::<usize>().ok())
    });
    let list_sessions = args.iter().any(|a| a == "--resume") && resume_index.is_none();

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

    // Resume: -r for latest, --resume N for specific
    let resume_path = if let Some(idx) = resume_index {
        let sessions = if list_all {
            session::list_all()?
        } else {
            session::list_for_project(&target)?
        };
        sessions.get(idx).map(|s| s.path.clone())
    } else if resume_latest {
        session::latest(&target)?
    } else {
        None
    };

    let (mut conversation, mut session_path) = if let Some(path) = resume_path {
        let history = session::load(&path)?;
        // Replay the conversation
        for msg in &history {
            let text = serde_json::to_value(&msg.content)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            if msg.role == "user" {
                println!("\x1b[48;5;236m \u{203a} \x1b[0m {text}");
            } else {
                termimad::print_text(&text);
            }
            println!();
        }
        (history, Some(path))
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
