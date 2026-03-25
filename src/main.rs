//! Replay — self-improving software orchestration.

mod agent;
mod ansi;
mod app;
mod beads;
mod clipboard;
mod compact;
mod config;
mod markdown;
mod models;
mod display;
mod engine;
mod session;
mod process_manager;
mod spawn_tool;
mod subagent;
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

    // Set initial state from saved config
    {
        let saved_model = config::saved_model()
            .and_then(|id| models::find_by_id(&id))
            .filter(|m| models::is_available(m))
            .unwrap_or(models::default_model());

        let mut s = state.lock().unwrap();
        s.project_path = target.to_string_lossy().to_string();
        s.model_name = saved_model.name.to_string();
        s.selected_model_id = saved_model.model_id.to_string();
        s.context_window = saved_model.context_window;

        if let Some(v) = config::display("show_usage") { s.show_usage = v; }
        if let Some(v) = config::display("show_model") { s.show_model = v; }
        if let Some(v) = config::display("show_context") { s.show_context = v; }
        if let Some(v) = config::display("show_project") { s.show_project = v; }
        s.reasoning_effort = config::get("reasoning_effort");
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
    let mut agent_cancel: Option<Arc<AtomicBool>> = None;

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
                            let current_name = s.model_name.clone();
                            let options: Vec<app::MenuOption> = models::MODELS.iter().map(|m| {
                                app::MenuOption {
                                    label: m.name.to_string(),
                                    description: Some(m.provider.to_string()),
                                    enabled: models::is_available(m),
                                }
                            }).collect();

                            let picker_state = Arc::clone(&state);
                            s.show_menu(
                                format!("Select model (currently: {current_name})"),
                                options,
                                move |selected| {
                                    if let Some(idx) = selected {
                                        if let Some(m) = models::MODELS.get(idx) {
                                            let mut s = picker_state.lock().unwrap();
                                            s.selected_model_id = m.model_id.to_string();
                                            s.model_name = m.name.to_string();
                                            s.context_window = m.context_window;
                                            s.show_model = true;
                                            drop(s);
                                            config::save_model(m.model_id);
                                        }
                                    }
                                },
                            );
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
                        "/effort" => {
                            let current = s.reasoning_effort.clone().unwrap_or_else(|| "medium".to_string());
                            let options = ["low", "medium", "high", "xhigh"].iter().map(|&e| {
                                app::MenuOption {
                                    label: e.to_string(),
                                    description: None,
                                    enabled: true,
                                }
                            }).collect();

                            let picker_state = Arc::clone(&state);
                            s.show_menu(
                                format!("Set reasoning effort (currently: {current})"),
                                options,
                                move |selected| {
                                    if let Some(idx) = selected {
                                        let effort = ["low", "medium", "high", "xhigh"][idx];
                                        let mut s = picker_state.lock().unwrap();
                                        s.reasoning_effort = Some(effort.to_string());
                                        drop(s);
                                        config::set("reasoning_effort", effort);
                                    }
                                },
                            );
                        }
                        "/compact" => {
                            s.status_message = Some("Compacting context...".to_string());
                            drop(s);
                            let model_id = state.lock().unwrap().selected_model_id.clone();
                            let model = models::find_by_id(&model_id)
                                .unwrap_or(models::default_model());
                            match compact::compact(&mut conversation, model).await {
                                Ok(()) => {
                                    let mut s = state.lock().unwrap();
                                    s.status_message = None;
                                    s.push_output("Context compacted.".to_string());
                                }
                                Err(e) => {
                                    let mut s = state.lock().unwrap();
                                    s.status_message = None;
                                    s.push_output(format!("Compaction failed: {e:#}"));
                                }
                            }
                        }
                        "/clear" => {
                            drop(s);
                            let _ = event_tx.send(AppEvent::Clear);
                        }
                        "/ps" | "/jobs" => {
                            let ps = s.format_ps();
                            s.push_output(ps);
                        }
                        "/clean" => {
                            let removed = s.jobs.iter().filter(|j| j.status != app::JobStatus::Running).count();
                            s.jobs.retain(|j| j.status == app::JobStatus::Running);
                            s.push_output(format!("Cleaned {removed} completed job(s)."));
                        }
                        cmd if cmd.starts_with("/attach ") => {
                            if let Some(id_str) = cmd.strip_prefix("/attach ") {
                                if let Ok(id) = id_str.trim().parse::<u32>() {
                                    s.push_output(format!("Attached to process #{id}. Esc or Ctrl-D to detach."));
                                    s.attached_process = Some(id);
                                    // TODO: wire attached_writer from process manager
                                } else {
                                    s.push_output(format!("Invalid process ID: {id_str}"));
                                }
                            }
                        }
                        "/help" => {
                            s.push_output("/clear          Clear conversation context and output".to_string());
                            s.push_output("/compact        Compress conversation history".to_string());
                            s.push_output("/usage          Toggle token usage display".to_string());
                            s.push_output("/model          Switch model".to_string());
                            s.push_output("/effort         Set reasoning effort".to_string());
                            s.push_output("/context        Toggle context window display".to_string());
                            s.push_output("/project        Toggle project path display".to_string());
                            s.push_output("/couch [on|off] Toggle couch/gamepad mode".to_string());
                            s.push_output("/jobs           Show background terminals".to_string());
                            s.push_output("/attach N       Attach to background terminal N".to_string());
                            s.push_output("/clean          Remove completed terminals".to_string());
                            s.push_output("/help           Show this help".to_string());
                        }
                        _ => {
                            s.push_output(format!("Unknown command: {instruction}"));
                        }
                    }
                    // Persist display preferences (skip if show_menu consumed the lock)
                    if let Ok(s) = state.try_lock() {
                        config::save_display("show_usage", s.show_usage);
                        config::save_display("show_model", s.show_model);
                        config::save_display("show_context", s.show_context);
                        config::save_display("show_project", s.show_project);
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
                let cb_event_tx = event_tx.clone();
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

                            // Detect background bash processes — send to main loop for async spawn
                            if name == "bash" && output.contains("\"background\"") {
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output) {
                                    if parsed.get("background").and_then(|v| v.as_bool()).unwrap_or(false) {
                                        let mode = parsed.get("mode").and_then(|v| v.as_str()).unwrap_or("pipe").to_string();
                                        let cmd = parsed.get("command").and_then(|v| v.as_str()).unwrap_or("???").to_string();
                                        let _ = cb_event_tx.send(AppEvent::SpawnBackground {
                                            command: cmd,
                                            mode,
                                        });
                                    }
                                }
                            }

                            // For tasks, show the ANSI-styled output
                            if *name == "tasks" && !output.is_empty() {
                                s.push_ansi(output.clone());
                            }
                            if cb_verbose {
                                let icon = if *success { "\x1b[32m●\x1b[0m" } else { "\x1b[31m●\x1b[0m" };
                                s.push_output(format!("  {icon} {name} output: {output}"));
                            }
                        }
                        llm_code_sdk::tools::ToolEvent::Usage { input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens } => {
                            s.total_input_tokens += input_tokens;
                            s.last_input_tokens = *input_tokens;
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

                // Run agent with cancellation flag
                agent_cancel = Some(Arc::clone(&cancelled));

                let agent_state = Arc::clone(&state);
                let (model_id, effort, last_input, ctx_window) = {
                    let s = state.lock().unwrap();
                    (s.selected_model_id.clone(), s.reasoning_effort.clone(),
                     s.last_input_tokens, s.context_window)
                };
                let model = models::find_by_id(&model_id)
                    .unwrap_or(models::default_model());

                // Auto-compact if approaching context limit
                if compact::should_compact(last_input, ctx_window) {
                    let mut s = state.lock().unwrap();
                    s.status_message = Some("Compacting context...".to_string());
                    drop(s);
                    if let Err(e) = compact::compact(&mut conversation, model).await {
                        let mut s = state.lock().unwrap();
                        s.push_output(format!("compaction failed: {e:#}"));
                    } else {
                        let mut s = state.lock().unwrap();
                        s.status_message = None;
                    }
                }

                // Create spawn tool for subagents
                let spawn = Arc::new(spawn_tool::SpawnTool::new(
                    &target,
                    Arc::new(Mutex::new(model_id.clone())),
                    Arc::clone(&state),
                )) as Arc<dyn llm_code_sdk::Tool>;

                let agent_future = agent::execute(
                    &instruction,
                    &target,
                    callback,
                    survey_cb,
                    &mut conversation,
                    Arc::clone(&cancelled),
                    model,
                    effort,
                    Some(spawn),
                );

                // Race agent against interrupt events.
                // The cancel flag is also checked inside the ToolRunner between
                // iterations and tool calls, so even in-flight work stops promptly.
                let result = tokio::select! {
                    result = agent_future => result,
                    _ = async {
                        // Wait for interrupt events while agent runs
                        loop {
                            match event_rx.recv().await {
                                Some(AppEvent::Interrupt) => {
                                    cancelled.store(true, Ordering::SeqCst);
                                    break;
                                }
                                Some(AppEvent::Quit) => {
                                    cancelled.store(true, Ordering::SeqCst);
                                    break;
                                }
                                None => break,
                                _ => {} // ignore other events while agent runs
                            }
                        }
                    } => Err(anyhow::anyhow!("interrupted")),
                };

                agent_cancel = None;

                {
                    let mut s = agent_state.lock().unwrap();
                    s.agent_active = false;
                    if cancelled.load(Ordering::SeqCst) {
                        s.push_output("(interrupted)".to_string());
                    } else if let Err(e) = result {
                        s.push_output(format!("error: {e:#}"));
                    }
                    s.push_output(String::new());
                }

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
                // Handled in the select! loop while agent is running.
                // If we get here, no agent is active — ignore.
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
            AppEvent::SpawnBackground { command, mode } => {
                let spawn_mode = match mode.as_str() {
                    "tty" => process_manager::SpawnMode::Pty,
                    "interactive" => process_manager::SpawnMode::Pipe,
                    _ => process_manager::SpawnMode::PipeNoStdin,
                };
                let mode_label = if mode == "tty" { "PTY" } else { "interactive" };

                // Create a process manager for this spawn
                let mut pm = process_manager::ProcessManager::new(&target);
                match pm.spawn(&command, spawn_mode).await {
                    Ok((id, mut stdout_rx, mut stderr_rx, exit_rx)) => {
                        let mut s = state.lock().unwrap();
                        let job_id = s.add_job(command.clone());
                        s.push_output(format!("🖥 #{job_id} Background {mode_label}: {command}"));
                        s.push_output("  Use /jobs to see status, /attach to interact".to_string());

                        // Spawn output collector that feeds into AppState
                        let out_state = Arc::clone(&state);
                        tokio::spawn(async move {
                            loop {
                                tokio::select! {
                                    chunk = stdout_rx.recv() => {
                                        match chunk {
                                            Some(data) => {
                                                let text = String::from_utf8_lossy(&data);
                                                let mut s = out_state.lock().unwrap();
                                                s.push_output(format!("  [#{job_id}] {}", text.trim()));
                                            }
                                            None => break,
                                        }
                                    }
                                    chunk = stderr_rx.recv() => {
                                        match chunk {
                                            Some(data) => {
                                                let text = String::from_utf8_lossy(&data);
                                                let mut s = out_state.lock().unwrap();
                                                s.push_output(format!("  [#{job_id}] {}", text.trim()));
                                            }
                                            None => break,
                                        }
                                    }
                                }
                            }
                            // Process exited
                            let code = exit_rx.await.unwrap_or(1);
                            let mut s = out_state.lock().unwrap();
                            let status = if code == 0 {
                                app::JobStatus::Done
                            } else {
                                app::JobStatus::Failed
                            };
                            s.update_job(job_id, status, 0);
                            s.push_output(format!("🖥 #{job_id} exited (code {code})"));
                        });
                    }
                    Err(e) => {
                        let mut s = state.lock().unwrap();
                        s.push_output(format!("Failed to spawn background process: {e}"));
                    }
                }
            }
            AppEvent::Attach(id) => {
                let mut s = state.lock().unwrap();
                s.attached_process = Some(id);
                s.push_output(format!("Attached to process #{id}. Esc or Ctrl-D to detach."));
            }
            AppEvent::Detach => {
                let mut s = state.lock().unwrap();
                if let Some(pid) = s.attached_process.take() {
                    s.attached_writer = None;
                    s.push_output(format!("Detached from process #{pid}"));
                }
            }
            AppEvent::ProcessInput(_data) => {
                // TODO: wire to process manager when integrated
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
