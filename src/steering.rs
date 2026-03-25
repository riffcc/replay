//! Unified input — rustyline always owns the prompt.
//!
//! A background thread runs rustyline continuously.
//! Lines are sent through a channel to wherever they're needed:
//! - At the prompt: next instruction (styled prompt)
//! - During agent execution: mid-prompt steering (no prompt shown)
//!
//! Double enter (within 300ms) during agent execution = interrupt.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustyline::DefaultEditor;
use tokio::sync::mpsc;

const DOUBLE_ENTER_WINDOW: Duration = Duration::from_millis(300);
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
const PROMPT: &str = "\n\x1b[48;5;236m \u{203a} \x1b[0m ";
const STEERING_PROMPT: &str = "";

/// Events from the input thread.
#[derive(Debug)]
pub enum InputEvent {
    Line(String),
    Eof,
}

/// Result of running the agent with steering.
pub enum SteerOutcome<T> {
    Done(T, Vec<String>),
    Interrupted(String),
}

/// The input handle. Owns the rustyline thread and provides lines.
pub struct Input {
    rx: mpsc::UnboundedReceiver<InputEvent>,
    steering: Arc<AtomicBool>,
}

impl Input {
    /// Start the input thread with rustyline.
    pub fn start(history_path: std::path::PathBuf) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let steering = Arc::new(AtomicBool::new(false));
        let steering_clone = Arc::clone(&steering);

        std::thread::spawn(move || {
            let mut rl = match DefaultEditor::new() {
                Ok(rl) => rl,
                Err(_) => return,
            };
            let _ = rl.load_history(&history_path);

            loop {
                let prompt = if steering_clone.load(Ordering::Relaxed) {
                    STEERING_PROMPT
                } else {
                    PROMPT
                };

                match rl.readline(prompt) {
                    Ok(line) => {
                        let trimmed = line.trim().to_string();
                        if !trimmed.is_empty() {
                            let _ = rl.add_history_entry(&trimmed);
                        }
                        if tx.send(InputEvent::Line(trimmed)).is_err() {
                            break;
                        }
                    }
                    Err(rustyline::error::ReadlineError::Interrupted | rustyline::error::ReadlineError::Eof) => {
                        let _ = rl.save_history(&history_path);
                        let _ = tx.send(InputEvent::Eof);
                        break;
                    }
                    Err(_) => {
                        let _ = tx.send(InputEvent::Eof);
                        break;
                    }
                }
            }
        });

        Self { rx, steering }
    }

    /// Wait for the next non-empty line (the prompt).
    pub async fn next_line(&mut self) -> Option<String> {
        self.steering.store(false, Ordering::Relaxed);
        loop {
            match self.rx.recv().await {
                Some(InputEvent::Line(line)) => {
                    if !line.is_empty() {
                        return Some(line);
                    }
                }
                Some(InputEvent::Eof) | None => return None,
            }
        }
    }

    /// Run a future with mid-prompt steering.
    pub async fn run_with_steering<F, T>(&mut self, future: F) -> SteerOutcome<T>
    where
        F: std::future::Future<Output = T>,
    {
        self.steering.store(true, Ordering::Relaxed);
        let mut queued: Vec<String> = Vec::new();
        let mut last_enter: Option<Instant> = None;

        tokio::pin!(future);

        loop {
            tokio::select! {
                result = &mut future => {
                    self.steering.store(false, Ordering::Relaxed);
                    return SteerOutcome::Done(result, queued);
                }
                event = self.rx.recv() => {
                    match event {
                        Some(InputEvent::Line(line)) => {
                            let now = Instant::now();
                            if line.is_empty() {
                                let is_double = last_enter
                                    .map(|prev| now.duration_since(prev) < DOUBLE_ENTER_WINDOW)
                                    .unwrap_or(false);

                                if is_double {
                                    self.steering.store(false, Ordering::Relaxed);
                                    let msg = queued.pop().unwrap_or_default();
                                    return SteerOutcome::Interrupted(msg);
                                }
                                last_enter = Some(now);
                            } else {
                                print!("{DIM}  (queued: {line}){RESET}\n");
                                std::io::Write::flush(&mut std::io::stdout()).ok();
                                queued.push(line);
                                last_enter = Some(now);
                            }
                        }
                        Some(InputEvent::Eof) | None => {
                            self.steering.store(false, Ordering::Relaxed);
                            return SteerOutcome::Interrupted(String::new());
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    self.steering.store(false, Ordering::Relaxed);
                    return SteerOutcome::Interrupted(String::new());
                }
            }
        }
    }
}
