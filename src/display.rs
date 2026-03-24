//! Display system for agent tool calls.
//!
//! Handles progressive output: grouping reads/writes on one line,
//! colored status bullets, and in-place updates.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Mutex;

use llm_code_sdk::tools::ToolEvent;

/// ANSI color codes.
const YELLOW: &str = "\x1b[33m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";

/// Current display state for progressive output.
#[derive(Debug)]
enum ActiveGroup {
    Reads { files: Vec<String> },
    Writes { files: Vec<String> },
}

/// Display state shared across tool events.
pub struct DisplayState {
    active: Option<ActiveGroup>,
    /// The last non-grouped line content (without bullet), for redrawing on result.
    pending_line: Option<String>,
    verbose: bool,
}

impl DisplayState {
    pub fn new(verbose: bool) -> Self {
        Self {
            active: None,
            pending_line: None,
            verbose,
        }
    }

    pub fn handle(&mut self, event: &ToolEvent) {
        match event {
            ToolEvent::Text { text } => {
                self.flush();
                termimad::print_text(text);
            }
            ToolEvent::ToolCall { name, input } => {
                self.handle_call(name, input);
            }
            ToolEvent::ToolResult { name, success, output } => {
                self.handle_result(name, *success, output);
            }
        }
    }

    fn handle_call(&mut self, name: &str, input: &HashMap<String, serde_json::Value>) {
        let s = |key: &str| input.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();

        match name {
            "read" => {
                let file = s("path");
                if let Some(ActiveGroup::Reads { files }) = &mut self.active {
                    files.push(file);
                    self.redraw_group();
                } else {
                    self.flush();
                    self.active = Some(ActiveGroup::Reads { files: vec![file] });
                    self.redraw_group();
                }
            }
            "write" => {
                let file = s("path");
                if let Some(ActiveGroup::Writes { files }) = &mut self.active {
                    files.push(file);
                    self.redraw_group();
                } else {
                    self.flush();
                    self.active = Some(ActiveGroup::Writes { files: vec![file] });
                    self.redraw_group();
                }
            }
            "bash" => {
                self.flush();
                let cmd = s("command");
                let display_cmd = if cmd.len() > 80 {
                    format!("{}...", &cmd[..77])
                } else {
                    cmd
                };
                let line = format!(">_ Bash({DIM}{display_cmd}{RESET})");
                self.print_pending(&line);
            }
            "grep" => {
                self.flush();
                let pattern = s("pattern");
                let line = format!("\u{1F50E} Grep({DIM}{pattern}{RESET})");
                self.print_pending(&line);
            }
            "glob" => {
                self.flush();
                let pattern = s("pattern");
                let line = format!("\u{1F4C1} Glob({DIM}{pattern}{RESET})");
                self.print_pending(&line);
            }
            "search" => {
                self.flush();
                let query = s("query");
                let line = format!("\u{1F50D} Search({DIM}{query}{RESET})");
                self.print_pending(&line);
            }
            "list_directory" => {
                self.flush();
                let path = s("path");
                let line = format!("\u{1F4C2} List({DIM}{path}{RESET})");
                self.print_pending(&line);
            }
            "activate_skill" => {
                // Skills get a one-line result on completion, no pending line
                self.flush();
            }
            "skill_resource" => {
                self.flush();
                let skill = s("skill");
                let path = s("path");
                let line = format!("\u{1F4CE} Resource({DIM}{skill}/{path}{RESET})");
                self.print_pending(&line);
            }
            _ => {
                self.flush();
                let line = format!("{name}");
                self.print_pending(&line);
            }
        }

        if self.verbose {
            if name != "activate_skill" {
                println!();
            }
            println!("  input: {}", serde_json::to_string_pretty(input).unwrap_or_default());
        }
    }

    fn handle_result(&mut self, name: &str, success: bool, output: &str) {
        match name {
            "read" | "write" => {
                self.update_group_status(success);
                if self.verbose {
                    println!();
                    println!("  output: {output}");
                }
            }
            "activate_skill" => {
                // One-line skill activation message
                let skill_name = output
                    .lines()
                    .next()
                    .and_then(|l| l.strip_prefix("# Skill: "))
                    .unwrap_or("unknown");
                if success {
                    println!("\u{1F916} {skill_name} skill activated");
                } else {
                    println!("{RED}●{RESET} skill activation failed");
                }
            }
            _ => {
                let bullet = if success {
                    format!("{GREEN}●{RESET}")
                } else {
                    format!("{RED}●{RESET}")
                };
                if let Some(line) = self.pending_line.take() {
                    let mut out = io::stdout();
                    write!(out, "\r\x1b[2K{bullet} {line}").ok();
                    out.flush().ok();
                    println!();
                } else {
                    println!("{bullet} {name}");
                }
                if self.verbose {
                    println!("  output: {output}");
                }
            }
        }
    }

    /// Print a line with a yellow pending bullet, storing it for later update.
    fn print_pending(&mut self, line: &str) {
        self.pending_line = Some(line.to_string());
        let mut out = io::stdout();
        write!(out, "{YELLOW}●{RESET} {line}").ok();
        out.flush().ok();
    }

    /// Redraw the current group line in place.
    fn redraw_group(&self) {
        let mut out = io::stdout();
        write!(out, "\r\x1b[2K").ok();

        match &self.active {
            Some(ActiveGroup::Reads { files }) => {
                let file_list = files.join(", ");
                write!(out, "{YELLOW}●{RESET} \u{1F4DA} Read({DIM}{file_list}{RESET})").ok();
            }
            Some(ActiveGroup::Writes { files }) => {
                let file_list = files.join(", ");
                write!(out, "{YELLOW}●{RESET} \u{1F4DD} Write({DIM}{file_list}{RESET})").ok();
            }
            None => {}
        }
        out.flush().ok();
    }

    /// Update the group's status bullet.
    fn update_group_status(&self, success: bool) {
        let bullet = if success {
            format!("{GREEN}●{RESET}")
        } else {
            format!("{RED}●{RESET}")
        };
        let mut out = io::stdout();
        write!(out, "\r\x1b[2K").ok();

        match &self.active {
            Some(ActiveGroup::Reads { files }) => {
                let file_list = files.join(", ");
                write!(out, "{bullet} \u{1F4DA} Read({DIM}{file_list}{RESET})").ok();
            }
            Some(ActiveGroup::Writes { files }) => {
                let file_list = files.join(", ");
                write!(out, "{bullet} \u{1F4DD} Write({DIM}{file_list}{RESET})").ok();
            }
            None => {}
        }
        out.flush().ok();
    }

    pub fn flush(&mut self) {
        if self.active.is_some() {
            println!();
            self.active = None;
        }
        if self.pending_line.is_some() {
            // Pending line that never got a result — just newline
            println!();
            self.pending_line = None;
        }
    }
}

/// Create a thread-safe display callback for the agent.
pub fn create_callback(verbose: bool) -> (llm_code_sdk::tools::ToolEventCallback, std::sync::Arc<Mutex<DisplayState>>) {
    let state = std::sync::Arc::new(Mutex::new(DisplayState::new(verbose)));
    let state_clone = std::sync::Arc::clone(&state);

    let callback: llm_code_sdk::tools::ToolEventCallback = std::sync::Arc::new(move |event| {
        let mut display = state_clone.lock().unwrap();
        display.handle(&event);
    });

    (callback, state)
}
