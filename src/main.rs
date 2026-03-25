//! Replay — self-improving software orchestration.

mod agent;
mod ansi;
mod app;
mod beads;
mod markdown;
mod display;
mod engine;
mod session;
mod survey_ui;
mod throbber;
mod tui;
mod voice;

pub const VERSION: &str = "0.1.0";

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tokio::sync::mpsc;

use app::{App, AppEvent, AppState};

/// Extract text content from a MessageParam for display purposes.
fn message_text(msg: &llm_code_sdk::MessageParam) -> String {
    match &msg.content {
        llm_code_sdk::MessageContent::Text(s) => s.clone(),
        llm_code_sdk::MessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| {
                if let llm_code_sdk::ContentBlockParam::Text { text, .. } = b {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

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

    // --resume listing doesn't need TUI
    if list_sessions {
        let sessions = if list_all {
            session::list_all()?
        } else {
            session::list_for_project(&target)?
        };
        print!("{}", session::format_list(&sessions));
        return Ok(());
    }

    // Resume session if requested
    let resume_path = if let Some(idx) = resume_index {
        let sessions = if list_all { session::list_all()? } else { session::list_for_project(&target)? };
        sessions.get(idx).map(|s| s.path.clone())
    } else if resume_latest {
        session::latest(&target)?
    } else {
        None
    };

    let (mut conversation, mut session_path) = if let Some(path) = resume_path {
        let history = session::load(&path)?;
        (history, Some(path))
    } else {
        (Vec::new(), None)
    };

    // Shared state between TUI and agent
    let state = Arc::new(Mutex::new(AppState::new()));
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();

    // Replay resumed conversation into output
    for msg in &conversation {
        let text = message_text(msg);
        let mut s = state.lock().unwrap();
        if msg.role == "user" {
            s.push_output(format!("\u{203a} {text}"));
        } else {
            s.push_markdown(text);
        }
        s.push_output(String::new());
    }

    // Set initial state
    {
        let mut s = state.lock().unwrap();
        s.project_path = target.to_string_lossy().to_string();
        s.model_name = "MiniMax-M2.7".to_string();
    }

    // Run TUI on a dedicated thread (it blocks for rendering)
    let tui_state = Arc::clone(&state);
    let tui_tx = event_tx.clone();
    let tui_handle = std::thread::spawn(move || {
        let mut app = App::new(tui_state, tui_tx);
        let result = app.run();
        App::cleanup().ok();
        result
    });

    // Main async loop: process events from the TUI
    let mut agent_cancel: Option<tokio::sync::oneshot::Sender<()>> = None;

    loop {
        let Some(event) = event_rx.recv().await else {
            break;
        };

        match event {
            AppEvent::Submit(instruction) => {
                if instruction.is_empty() {
                    continue;
                }

                // Handle slash commands
                if instruction.starts_with('/') {
                    let mut s = state.lock().unwrap();
                    match instruction.as_str() {
                        "/usage always" | "/usage on" | "/usage display always" | "/usage display on" => {
                            s.show_usage = true;
                        }
                        "/usage off" | "/usage display off" => {
                            s.show_usage = false;
                        }
                        "/usage" => {
                            s.show_usage = !s.show_usage;
                        }
                        "/model display always" | "/model display on" | "/model on" => {
                            s.show_model = true;
                        }
                        "/model display off" | "/model off" => {
                            s.show_model = false;
                        }
                        "/model" => {
                            s.show_model = !s.show_model;
                        }
                        "/context display always" | "/context display on" | "/context on" => {
                            s.show_context = true;
                        }
                        "/context display off" | "/context off" => {
                            s.show_context = false;
                        }
                        "/context" => {
                            s.show_context = !s.show_context;
                        }
                        "/project display always" | "/project display on" | "/project on" => {
                            s.show_project = true;
                        }
                        "/project display off" | "/project off" => {
                            s.show_project = false;
                        }
                        "/project" => {
                            s.show_project = !s.show_project;
                        }
                        "/couch" | "/couch on" => {
                            s.couch_mode = true;
                            s.couch_mode_notify = 30;
                            s.push_output("🎮 Couch mode on".to_string());
                        }
                        "/couch off" => {
                            s.couch_mode = false;
                            s.couch_mode_notify = 30;
                            s.push_output("🎮 Couch mode off".to_string());
                        }
                        "/clear" => {
                            drop(s);
                            let _ = event_tx.send(AppEvent::Clear);
                        }
                        "/help" => {
                            s.push_output("/clear          Clear conversation context and output".to_string());
                            s.push_output("/usage          Toggle token usage display".to_string());
                            s.push_output("/model          Toggle model name display".to_string());
                            s.push_output("/context        Toggle context window display".to_string());
                            s.push_output("/project        Toggle project path display".to_string());
                            s.push_output("/couch [on|off] Toggle couch/gamepad mode".to_string());
                            s.push_output("/help           Show this help".to_string());
                        }
                        _ => {
                            s.push_output(format!("Unknown command: {instruction}"));
                        }
                    }
                    continue;
                }

                // Echo user input to output
                {
                    let mut s = state.lock().unwrap();
                    s.push_output(format!("\u{203a} {instruction}"));
                    s.push_output(String::new());
                    s.agent_active = true;
                    s.throbber_state = 1;
                }

                // Cancellation flag: set to true when interrupt is received.
                // The agent callback checks this to avoid processing events after cancel.
                let cancelled = Arc::new(AtomicBool::new(false));

                // Create agent callback that writes to shared state
                let cb_state = Arc::clone(&state);
                let cb_verbose = verbose;
                let cb_cancelled = Arc::clone(&cancelled);
                let callback: llm_code_sdk::tools::ToolEventCallback = Arc::new(move |event| {
                    // If cancellation was requested, ignore this event to avoid
                    // overwriting the "(interrupted)" message with late results.
                    if cb_cancelled.load(Ordering::SeqCst) {
                        return;
                    }

                    let mut s = cb_state.lock().unwrap();
                    match &event {
                        llm_code_sdk::tools::ToolEvent::Text { text } => {
                            s.throbber_state = 1;
                            s.push_markdown(text.clone());
                        }
                        llm_code_sdk::tools::ToolEvent::ToolCall { name, input } => {
                            s.throbber_state = 2;
                            // Survey tool is displayed as the interactive UI, not as a tool call line
                            if name == "survey" {
                                // The survey UI will appear via pending_survey in AppState
                            } else {
                                let detail = tool_summary(name, input);
                                let emoji = tool_emoji(name);
                                if detail.is_empty() {
                                    s.push_output(format!("{emoji} {name}"));
                                } else {
                                    s.push_output(format!("{emoji} {name}({detail})"));
                                }
                                if cb_verbose {
                                    s.push_output(format!("  input: {}", serde_json::to_string_pretty(input).unwrap_or_default()));
                                }
                            }
                        }
                        llm_code_sdk::tools::ToolEvent::ToolResult { name, success, output } => {
                            s.throbber_state = 1;
                            let icon = if *success { "\x1b[32m●\x1b[0m" } else { "\x1b[31m●\x1b[0m" };
                            // For tasks, show the ANSI-styled output
                            if *name == "tasks" && !output.is_empty() {
                                s.push_ansi(output.clone());
                            }
                            if cb_verbose {
                                s.push_output(format!("  {icon} {name} output: {output}"));
                            }
                        }
                        llm_code_sdk::tools::ToolEvent::Usage { input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens } => {
                            s.total_input_tokens += input_tokens;
                            s.total_output_tokens += output_tokens;
                            s.total_cache_read += cache_read_tokens;
                            s.total_cache_creation += cache_creation_tokens;
                        }
                    }
                });

                let survey_state = Arc::clone(&state);
                let survey_cancelled = Arc::clone(&cancelled);
                let survey_cb: llm_code_sdk::tools::SurveyCallback = Arc::new(move |req| {
                    // If cancelled, return empty response immediately
                    if survey_cancelled.load(Ordering::SeqCst) {
                        return llm_code_sdk::tools::SurveyResponse { selected: vec![] };
                    }

                    let (tx, rx) = std::sync::mpsc::channel();
                    {
                        let mut s = survey_state.lock().unwrap();
                        let option_count = req.options.len();
                        s.pending_survey = Some(app::PendingSurvey {
                            prompt: req.prompt.clone(),
                            options: req.options.clone(),
                            multi: req.multi,
                            cursor: 0,
                            selected: vec![false; option_count],
                            response_tx: tx,
                        });
                    }
                    // Block until the TUI sends the response
                    rx.recv().unwrap_or(llm_code_sdk::tools::SurveyResponse { selected: vec![] })
                });

                // Run agent
                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
                agent_cancel = Some(cancel_tx);

                let agent_state = Arc::clone(&state);
                let agent_target = target.clone();
                let agent_cancelled = Arc::clone(&cancelled);

                let agent_future = agent::execute(
                    &instruction,
                    &agent_target,
                    callback,
                    survey_cb,
                    &mut conversation,
                );

                tokio::select! {
                    result = agent_future => {
                        let mut s = agent_state.lock().unwrap();
                        s.agent_active = false;
                        // If cancelled was set, the cancel branch already printed "(interrupted)".
                        // Don't print error/result over that message.
                        if agent_cancelled.load(Ordering::SeqCst) {
                            s.push_output(String::new());
                        } else if let Err(e) = result {
                            s.push_output(format!("error: {e:#}"));
                            s.push_output(String::new());
                        } else {
                            s.push_output(String::new());
                        }
                    }
                    _ = async { cancel_rx.await.ok() } => {
                        cancelled.store(true, Ordering::SeqCst);
                        let mut s = agent_state.lock().unwrap();
                        s.agent_active = false;
                        s.push_output("(interrupted)".to_string());
                        s.push_output(String::new());
                    }
                }

                agent_cancel = None;

                // Auto-save
                match session::save(&target, session_path.as_deref(), &conversation) {
                    Ok(path) => session_path = Some(path),
                    Err(e) => {
                        let mut s = state.lock().unwrap();
                        s.push_output(format!("warning: failed to save session: {e:#}"));
                    }
                }
            }
            AppEvent::Interrupt => {
                if let Some(cancel) = agent_cancel.take() {
                    let _ = cancel.send(());
                }
            }
            AppEvent::Clear => {
                conversation.clear();
                let mut s = state.lock().unwrap();
                s.clear();
            }
            AppEvent::VoiceAudio(samples) => {
                let tx = event_tx.clone();
                let progress_state = Arc::clone(&state);
                let on_progress: voice::ProgressCallback = Arc::new(move |msg| {
                    let mut s = progress_state.lock().unwrap();
                    s.status_message = Some(msg.to_string());
                });
                tokio::spawn(async move {
                    match voice::transcribe(&samples, Some(on_progress)).await {
                        Ok(text) => { let _ = tx.send(AppEvent::VoiceTranscription(text)); }
                        Err(e) => { let _ = tx.send(AppEvent::VoiceTranscription(format!("(error: {e})"))); }
                    }
                });
            }
            AppEvent::VoiceTranscription(text) => {
                let mut s = state.lock().unwrap();
                if text.starts_with("(error:") {
                    s.status_message = Some(text);
                } else if !text.trim().is_empty() {
                    // Insert into input buffer — user can review before sending
                    s.pending_insert = Some(text);
                }
            }
            AppEvent::Quit => {
                break;
            }
        }
    }

    // Wait for TUI thread
    let _ = tui_handle.join();

    Ok(())
}

fn tool_emoji(name: &str) -> &'static str {
    match name {
        "read" => "\u{1F4DA}",
        "write" => "\u{1F4DD}",
        "bash" => ">_",
        "grep" => "\u{1F50E}",
        "glob" => "\u{1F4C1}",
        "search" => "\u{1F50D}",
        "list_directory" => "\u{1F4C2}",
        "tasks" => "\u{1F4CB}",
        "activate_skill" => "\u{1F916}",
        "skill_resource" => "\u{1F4CE}",
        "ask_question" => "\u{1F4DA} DeepWiki \u{2014} Ask",
        "read_wiki_contents" => "\u{1F4DA} DeepWiki \u{2014} Read",
        "read_wiki_structure" => "\u{1F4DA} DeepWiki \u{2014} Structure",
        "survey" => "\u{1F4CB}",
        _ => "\u{2022}",
    }
}

fn tool_summary(name: &str, input: &std::collections::HashMap<String, serde_json::Value>) -> String {
    let s = |key: &str| input.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match name {
        "bash" => {
            let cmd = s("command");
            if cmd.len() > 80 { format!("{}...", &cmd[..77]) } else { cmd }
        }
        "read" => s("path"),
        "write" => s("path"),
        "grep" => s("pattern"),
        "glob" => s("pattern"),
        "search" => s("query"),
        "list_directory" => s("path"),
        "tasks" => {
            let op = s("operation");
            let id = s("id");
            let title = s("title");
            if !id.is_empty() { format!("{op} {id}") }
            else if !title.is_empty() { format!("{op}: {title}") }
            else { op }
        }
        "activate_skill" => s("name"),
        "ask_question" => {
            let repo = s("repoName");
            let q = s("question");
            if repo.is_empty() { q } else { format!("{repo}: {q}") }
        }
        "read_wiki_contents" | "read_wiki_structure" => s("repoName"),
        _ => String::new(),
    }
}
