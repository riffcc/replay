//! Replay — self-improving software orchestration.

mod agent;
mod beads;
mod display;
mod engine;
mod session;
mod steering;
mod survey_ui;
mod tui;

pub const VERSION: &str = "0.1.0";

use std::path::PathBuf;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    let resume_latest = args.iter().any(|a| a == "-r");
    let list_all = args.iter().any(|a| a == "--all");
    let target = std::env::current_dir()?;

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

    let replay_dir = dirs::home_dir()
        .expect("no home directory")
        .join(".replay");
    std::fs::create_dir_all(&replay_dir)?;
    let history_path = replay_dir.join("history");

    // Resume session if requested
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

    // Start unified input (rustyline in background thread)
    let mut input = steering::Input::start(history_path);

    loop {
        let Some(instruction) = input.next_line().await else {
            break;
        };

        println!();
        run_turn(&instruction, &target, verbose, &mut conversation, &mut input).await;

        // Auto-save
        match session::save(&target, session_path.as_deref(), &conversation) {
            Ok(path) => session_path = Some(path),
            Err(e) => eprintln!("warning: failed to save session: {e:#}"),
        }
    }

    Ok(())
}

async fn run_turn(
    instruction: &str,
    target: &std::path::Path,
    verbose: bool,
    conversation: &mut Vec<llm_code_sdk::MessageParam>,
    input: &mut steering::Input,
) {
    let (callback, display_state) = display::create_callback(verbose);
    let survey_cb: llm_code_sdk::tools::SurveyCallback = std::sync::Arc::new(|req| {
        survey_ui::run_survey(&req)
    });

    let agent_future = agent::execute(instruction, target, callback, survey_cb, conversation);

    match input.run_with_steering(agent_future).await {
        steering::SteerOutcome::Done(result, queued) => {
            display_state.lock().unwrap().flush();
            if let Err(e) = result {
                eprintln!("error: {e:#}");
            }
            // Process queued follow-up messages
            for msg in queued {
                println!("\n\x1b[48;5;236m \u{203a} \x1b[0m {msg}\n");
                Box::pin(run_turn(&msg, target, verbose, conversation, input)).await;
            }
        }
        steering::SteerOutcome::Interrupted(feedback) => {
            display_state.lock().unwrap().flush();
            eprintln!("\x1b[2minterrupted\x1b[0m");
            if !feedback.is_empty() {
                println!("\n\x1b[48;5;236m \u{203a} \x1b[0m {feedback}\n");
                Box::pin(run_turn(&feedback, target, verbose, conversation, input)).await;
            }
        }
    }
}
