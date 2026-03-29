//! TUI application — ratatui-based layout with output pane and input field.
//!
//! Keeps the same visual feel as the REPL but with proper separation
//! between output and input areas.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use tokio::sync::mpsc;

/// A line in the output log.
#[derive(Clone)]
pub struct OutputLine {
    /// Plain text content (for re-wrapping).
    pub content: String,
    /// Styled spans for rendering (optional — if None, render as raw).
    pub styled: Option<Vec<Span<'static>>>,
}

/// Dim a color by blending it toward gray (for context lines in diffs).
fn dim_color(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => {
            // Blend 60% toward gray
            let gray: u8 = 100;
            Color::Rgb(
                ((r as u16 * 40 + gray as u16 * 60) / 100) as u8,
                ((g as u16 * 40 + gray as u16 * 60) / 100) as u8,
                ((b as u16 * 40 + gray as u16 * 60) / 100) as u8,
            )
        }
        _ => Color::DarkGray,
    }
}

/// Stored metadata for a tool call, used for expanded view.
#[derive(Clone, Debug)]
pub struct ToolCallRecord {
    /// Tool name (e.g. "write", "bash", "read").
    pub name: String,
    /// Summary text shown in collapsed view (e.g. "src/agent.rs").
    pub detail: String,
    /// Index into raw_output where the collapsed line lives.
    pub raw_index: usize,
    /// Tool result output text (captured on ToolResult).
    pub output: Option<String>,
    /// Diffstat: (added, removed) — only for write tools.
    pub diffstat: Option<(usize, usize)>,
    /// Diff lines from metadata (unified-style: +added, -removed, context).
    pub diff: Option<Vec<String>>,
    /// Full metadata from ToolResult.
    pub metadata: Option<serde_json::Value>,
}

/// Wrap a string to a given width with word-boundary reflow.
/// Words that don't fit on the current line move to the next line.
/// Words longer than `width` are force-broken by character.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for raw_line in text.split('\n') {
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut col: usize = 0;
        for word in WordIter::new(raw_line) {
            let wlen = word.chars().count();
            if col > 0 && col + wlen > width {
                lines.push(std::mem::take(&mut current));
                col = 0;
            }
            if wlen > width && col == 0 {
                let mut chars = word.chars();
                while col < wlen {
                    let take = (wlen - col).min(width);
                    let chunk: String = chars.by_ref().take(take).collect();
                    col += take;
                    if col < wlen {
                        lines.push(chunk);
                        col = 0;
                    } else {
                        current.push_str(&chunk);
                        col = take;
                    }
                }
            } else {
                current.push_str(word);
                col += wlen;
            }
        }
        lines.push(current);
    }
    lines
}

/// Iterator that yields words and whitespace runs as separate tokens.
struct WordIter<'a> {
    rest: &'a str,
}

impl<'a> WordIter<'a> {
    fn new(s: &'a str) -> Self {
        Self { rest: s }
    }
}

impl<'a> Iterator for WordIter<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        if self.rest.is_empty() {
            return None;
        }
        let bytes = self.rest.as_bytes();
        let is_space = bytes[0] == b' ' || bytes[0] == b'\t';
        let end = self.rest
            .char_indices()
            .skip(1)
            .find(|(_, c)| (*c == ' ' || *c == '\t') != is_space)
            .map(|(i, _)| i)
            .unwrap_or(self.rest.len());
        let token = &self.rest[..end];
        self.rest = &self.rest[end..];
        Some(token)
    }
}

/// Wrap a styled Line to a given width with word-boundary reflow.
fn wrap_styled_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line.clone()];
    }

    let total_width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total_width <= width {
        return vec![line.clone()];
    }

    let mut segments: Vec<(&str, Style)> = Vec::new();
    for span in &line.spans {
        let style = span.style;
        let mut rest: &str = &span.content;
        while !rest.is_empty() {
            let bytes = rest.as_bytes();
            let is_space = bytes[0] == b' ' || bytes[0] == b'\t';
            let end = rest.char_indices()
                .skip(1)
                .find(|(_, c)| (*c == ' ' || *c == '\t') != is_space)
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            segments.push((&rest[..end], style));
            rest = &rest[end..];
        }
    }

    let mut result: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut col: usize = 0;

    for (seg, style) in &segments {
        let slen = seg.chars().count();
        if col > 0 && col + slen > width {
            result.push(Line::from(std::mem::take(&mut current_spans)));
            col = 0;
        }
        if slen > width && col == 0 {
            let mut chars_remaining = seg.char_indices().peekable();
            let mut taken = 0;
            while taken < slen {
                let take = (slen - taken).min(width);
                let start_byte = chars_remaining.peek().map(|(i, _)| *i).unwrap_or(seg.len());
                for _ in 0..take {
                    chars_remaining.next();
                }
                let end_byte = chars_remaining.peek().map(|(i, _)| *i).unwrap_or(seg.len());
                let chunk = &seg[start_byte..end_byte];
                taken += take;
                if taken < slen {
                    current_spans.push(Span::styled(chunk.to_string(), *style));
                    result.push(Line::from(std::mem::take(&mut current_spans)));
                    col = 0;
                } else {
                    current_spans.push(Span::styled(chunk.to_string(), *style));
                    col = take;
                }
            }
        } else {
            current_spans.push(Span::styled(seg.to_string(), *style));
            col += slen;
        }
    }

    if !current_spans.is_empty() {
        result.push(Line::from(current_spans));
    }

    if result.is_empty() {
        result.push(Line::raw(""));
    }

    result
}

/// Voice VU meter — Codex-style braille visualization.
pub struct VoiceMeter {
    history: std::collections::VecDeque<char>,
    noise_ema: f64,
    env: f64,
}

const VU_SYMBOLS: [char; 7] = ['⠤', '⠴', '⠶', '⠷', '⡷', '⡿', '⣿'];

impl VoiceMeter {
    pub fn new() -> Self {
        let mut history = std::collections::VecDeque::with_capacity(4);
        for _ in 0..4 {
            history.push_back('⠤');
        }
        Self {
            history,
            noise_ema: 0.02,
            env: 0.0,
        }
    }

    /// Feed a peak level (0.0 - 1.0) and get the 4-char braille string.
    pub fn update(&mut self, peak: f32) -> String {
        const ALPHA_NOISE: f64 = 0.05;
        const ATTACK: f64 = 0.80;
        const RELEASE: f64 = 0.25;

        let latest_peak = peak as f64;

        if latest_peak > self.env {
            self.env = ATTACK * latest_peak + (1.0 - ATTACK) * self.env;
        } else {
            self.env = RELEASE * latest_peak + (1.0 - RELEASE) * self.env;
        }

        let rms_approx = self.env * 0.7;
        self.noise_ema = (1.0 - ALPHA_NOISE) * self.noise_ema + ALPHA_NOISE * rms_approx;
        let ref_level = self.noise_ema.max(0.01);
        let fast_signal = 0.8 * latest_peak + 0.2 * self.env;
        let target = 2.0f64;
        let raw = (fast_signal / (ref_level * target)).max(0.0);
        let k = 1.6f64;
        let compressed = (raw.ln_1p() / k.ln_1p()).min(1.0);
        let idx = (compressed * (VU_SYMBOLS.len() as f64 - 1.0))
            .round()
            .clamp(0.0, VU_SYMBOLS.len() as f64 - 1.0) as usize;
        let level_char = VU_SYMBOLS[idx];

        if self.history.len() >= 4 {
            self.history.pop_front();
        }
        self.history.push_back(level_char);

        self.history.iter().collect()
    }
}

/// Info about an active or completed job (subagent).
#[derive(Clone)]
pub struct JobInfo {
    pub id: u32,
    pub task: String,
    pub status: JobStatus,
    pub started: std::time::Instant,
    pub tool_calls: usize,
}

#[derive(Clone, PartialEq)]
pub enum JobStatus {
    Running,
    Done,
    Failed,
}

/// Job browser view mode.
#[derive(Clone, PartialEq)]
pub enum JobBrowserMode {
    /// Browsing the job list.
    List,
    /// Viewing a job's output (read-only).
    ViewOutput(u32),
    /// Context menu on a job.
    ContextMenu(u32, usize), // job_id, menu cursor
}

/// State for the interactive job browser.
#[derive(Clone)]
pub struct JobBrowser {
    pub mode: JobBrowserMode,
    pub cursor: usize,
    /// Timestamp when A was first pressed (for hold-to-attach).
    pub a_held_since: Option<std::time::Instant>,
}

impl JobBrowser {
    pub fn new() -> Self {
        Self {
            mode: JobBrowserMode::List,
            cursor: 0,
            a_held_since: None,
        }
    }
}

const JOB_CONTEXT_MENU: [&str; 3] = ["Attach (interactive)", "Stop", "Set Timeout"];

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            JobStatus::Running => write!(f, "running"),
            JobStatus::Done => write!(f, "done"),
            JobStatus::Failed => write!(f, "failed"),
        }
    }
}

/// An interactive menu — used for model pickers, permission prompts, and agent surveys.
/// Agent survey — the SDK's SurveyTool creates these. Separate from Menu.
pub struct PendingSurvey {
    pub prompt: String,
    pub options: Vec<llm_code_sdk::tools::SurveyOption>,
    pub multi: bool,
    pub cursor: usize,
    pub selected: Vec<bool>,
    pub response_tx: std::sync::mpsc::Sender<llm_code_sdk::tools::SurveyResponse>,
}

/// Interactive menu — model picker, permission prompts, etc.
/// Completely separate from the agent's survey system.
pub struct Menu {
    pub prompt: String,
    pub options: Vec<MenuOption>,
    pub cursor: usize,
    /// Called with the selected index on Enter, or None on Esc.
    pub on_select: Box<dyn FnOnce(Option<usize>) + Send>,
}

#[derive(Clone)]
pub struct MenuOption {
    pub label: String,
    pub description: Option<String>,
    /// Greyed out and unselectable when false.
    pub enabled: bool,
}

/// Raw entry for re-wrapping on resize.
#[derive(Clone)]
enum RawEntry {
    Plain(String),
    Markdown(String),
    Ansi(String),
}

/// State shared between the TUI and the agent.
pub struct AppState {
    /// Raw output entries (not wrapped).
    raw_output: Vec<RawEntry>,
    /// Pre-wrapped output lines for display.
    pub output: Vec<OutputLine>,
    /// Current terminal width for wrapping.
    pub term_width: usize,
    /// Scroll offset from bottom (0 = at bottom).
    pub scroll_offset: usize,
    /// Whether the agent is currently working.
    pub agent_active: bool,
    /// Messages queued while agent is running.
    pub queued_messages: Vec<String>,
    /// Transient status message shown in the input border.
    pub status_message: Option<String>,
    /// Throbber frame for animation.
    pub throbber_frame: u8,
    /// Throbber state: 0=idle, 1=thinking, 2=tool
    pub throbber_state: u8,
    /// Cumulative token usage for this session.
    pub total_input_tokens: u64,
    /// Input tokens from the last API call (actual context usage).
    pub last_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read: u64,
    pub total_cache_creation: u64,
    /// Status bar visibility flags.
    pub show_usage: bool,
    pub show_model: bool,
    pub show_context: bool,
    pub show_project: bool,
    pub show_rates: bool,
    /// Active provider-pressure snapshot for the selected route.
    pub provider_pressure: Option<crate::provider_pressure::ProviderPressureSnapshot>,
    /// Model name.
    pub model_name: String,
    /// Currently selected model ID (for agent::execute).
    pub selected_model_id: String,
    /// Reasoning effort for models that support it (low, medium, high).
    pub reasoning_effort: Option<String>,
    /// Project directory path.
    pub project_path: String,
    /// Context window size (for percentage calculation).
    pub context_window: u64,
    /// Couch mode — gamepad-friendly, surveys for all input.
    pub couch_mode: bool,
    /// Couch mode notification countdown (frames remaining).
    pub couch_mode_notify: u8,
    /// Voice recording active.
    pub recording: bool,
    /// Voice meter state (Codex-style braille VU).
    pub voice_meter: VoiceMeter,
    /// Agent survey (from SDK SurveyTool).
    pub pending_survey: Option<PendingSurvey>,
    /// Active jobs (subagents / background terminals).
    pub jobs: Vec<JobInfo>,
    next_job_id: u32,
    /// Job browser state (None = not browsing).
    pub job_browser: Option<JobBrowser>,
    /// Attached to a background terminal (job ID). Keystrokes forwarded.
    pub attached_process: Option<u32>,
    /// Writer channels for background processes, keyed by job ID.
    pub process_writers: std::collections::HashMap<u32, tokio::sync::mpsc::Sender<Vec<u8>>>,
    /// Process handles for termination, keyed by job ID.
    pub process_handles: std::collections::HashMap<u32, replay_pty::ProcessHandle>,
    /// Shared background process registry from BashTool.
    pub bash_process_registry: Option<std::sync::Arc<tokio::sync::Mutex<llm_code_sdk::tools::BgProcessRegistry>>>,
    /// Interactive menu (model picker, permission prompts, etc.).
    pub active_menu: Option<Menu>,
    /// Text to insert into the input buffer (set by main loop, consumed by TUI thread).
    pub pending_insert: Option<String>,
    /// Tool call records for expanded view (indexed by sequential call order).
    pub tool_calls: Vec<ToolCallRecord>,
    /// Expanded output view (Ctrl+O toggle).
    pub expanded_view: bool,
    /// Syntax highlighter for expanded view.
    pub highlighter: crate::highlight::Highlighter,
    /// Token rate tracking for animation.
    pub last_token_update: std::time::Instant,
    pub token_rate: f64,        // tokens per second (smoothed)
    pub token_flash: u8,        // frames remaining for highlight effect (0 = none)
    prev_total_tokens: u64,
}

impl AppState {
    pub fn new() -> Self {
        let (w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        Self {
            raw_output: Vec::new(),
            output: Vec::new(),
            term_width: w as usize,
            scroll_offset: 0,
            agent_active: false,
            queued_messages: Vec::new(),
            status_message: None,
            throbber_frame: 0,
            throbber_state: 0,
            total_input_tokens: 0,
            last_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read: 0,
            total_cache_creation: 0,
            show_usage: true,
            show_model: false,
            show_context: true,
            show_project: true,
            show_rates: false,
            provider_pressure: None,
            couch_mode: false,
            couch_mode_notify: 0,
            recording: false,
            voice_meter: VoiceMeter::new(),
            pending_survey: None,
            jobs: Vec::new(),
            next_job_id: 1,
            job_browser: None,
            attached_process: None,
            process_writers: std::collections::HashMap::new(),
            process_handles: std::collections::HashMap::new(),
            bash_process_registry: None,
            active_menu: None,
            pending_insert: None,
            tool_calls: Vec::new(),
            expanded_view: false,
            highlighter: crate::highlight::Highlighter::new(),
            model_name: String::new(),
            selected_model_id: String::new(),
            reasoning_effort: None,
            project_path: String::new(),
            context_window: 200_000, // default, updated from model info
            last_token_update: std::time::Instant::now(),
            token_rate: 0.0,
            token_flash: 0,
            prev_total_tokens: 0,
        }
    }

    /// Show an interactive menu. Calls `on_select` with the chosen index (or None on cancel).
    pub fn show_menu(
        &mut self,
        prompt: impl Into<String>,
        options: Vec<MenuOption>,
        on_select: impl FnOnce(Option<usize>) + Send + 'static,
    ) {
        self.active_menu = Some(Menu {
            prompt: prompt.into(),
            options,
            cursor: 0,
            on_select: Box::new(on_select),
        });
    }

    /// Clear all conversation state (output, tokens, scroll).
    pub fn clear(&mut self) {
        self.raw_output.clear();
        self.output.clear();
        self.total_input_tokens = 0;
        self.last_input_tokens = 0;
        self.total_output_tokens = 0;
        self.total_cache_read = 0;
        self.total_cache_creation = 0;
        self.token_rate = 0.0;
        self.prev_total_tokens = 0;
        self.scroll_offset = 0;
    }

    /// Register a new job and return its ID.
    pub fn add_job(&mut self, task: String) -> u32 {
        let id = self.next_job_id;
        self.next_job_id += 1;
        self.jobs.push(JobInfo {
            id,
            task,
            status: JobStatus::Running,
            started: std::time::Instant::now(),
            tool_calls: 0,
        });
        id
    }

    /// Update a job's status.
    pub fn update_job(&mut self, id: u32, status: JobStatus, tool_calls: usize) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.status = status;
            job.tool_calls = tool_calls;
        }
    }

    /// Format job list for /ps display.
    pub fn format_ps(&self) -> String {
        if self.jobs.is_empty() {
            return "No background terminals.".to_string();
        }

        let mut out = String::from("Background terminals\n\n");
        for job in &self.jobs {
            let elapsed = job.started.elapsed().as_secs();
            let icon = match job.status {
                JobStatus::Running => "◐",
                JobStatus::Done => "✔",
                JobStatus::Failed => "✗",
            };
            let status_text = match job.status {
                JobStatus::Running => format!("Running, {elapsed}s"),
                JobStatus::Done => format!("Success, ran for {elapsed}s"),
                JobStatus::Failed => format!("Failed, after {elapsed}s"),
            };
            let task_preview = if job.task.len() > 60 {
                format!("{}...", &job.task[..57])
            } else {
                job.task.clone()
            };
            out.push_str(&format!("  {icon} {task_preview} ({status_text})\n"));
        }

        let running = self.jobs.iter().filter(|j| j.status == JobStatus::Running).count();
        out.push('\n');
        if running > 0 {
            out.push_str(&format!(
                "  {running} background terminal{} running \u{00b7} /jobs to view \u{00b7} /clean to close\n",
                if running == 1 { "" } else { "s" },
            ));
        } else {
            out.push_str("  No terminals running \u{00b7} /clean to remove completed\n");
        }
        out
    }

    pub fn push_output(&mut self, content: String) {
        self.raw_output.push(RawEntry::Plain(content.clone()));
        for line in wrap_text(&content, self.term_width) {
            self.output.push(OutputLine { content: line, styled: None });
        }
        self.scroll_offset = 0;
    }

    /// Push a tool call line and record it for later update / expanded view.
    pub fn push_tool_call(&mut self, name: &str, detail: &str, emoji: &str) {
        let line = if detail.is_empty() {
            format!("{emoji} {name}")
        } else {
            format!("{emoji} {name}({detail})")
        };
        let raw_index = self.raw_output.len();
        self.raw_output.push(RawEntry::Plain(line.clone()));
        for wrapped in wrap_text(&line, self.term_width) {
            self.output.push(OutputLine { content: wrapped, styled: None });
        }
        self.tool_calls.push(ToolCallRecord {
            name: name.to_string(),
            detail: detail.to_string(),
            raw_index,
            output: None,
            diffstat: None,
            diff: None,
            metadata: None,
        });
        self.scroll_offset = 0;
    }

    /// Update the most recent matching tool call with result metadata.
    /// Rewrites the collapsed line to include diffstat if available.
    pub fn update_tool_result(&mut self, name: &str, output: String, metadata: Option<&serde_json::Value>) {
        // Find the last tool call record with this name
        let record = self.tool_calls.iter_mut().rev().find(|r| r.name == name);
        let record = match record {
            Some(r) => r,
            None => return,
        };

        record.output = Some(output);
        record.metadata = metadata.cloned();

        // Extract diffstat and diff hunks from metadata
        if let Some(meta) = metadata {
            let added = meta.get("added_lines").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let removed = meta.get("removed_lines").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if added > 0 || removed > 0 {
                record.diffstat = Some((added, removed));
            }
            if let Some(hunks) = meta.get("hunks").and_then(|v| v.as_array()) {
                let mut diff = Vec::new();
                for entry in hunks {
                    let op = entry.get("op").and_then(|v| v.as_str()).unwrap_or(" ");
                    let line = entry.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let text = entry.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    diff.push(format!("{op}\t{line}\t{text}"));
                }
                if !diff.is_empty() {
                    record.diff = Some(diff);
                }
            }
        }

        // Rewrite the raw entry with rich info for write tools
        if record.name == "write" {
            let emoji = "\u{1F4DD}";
            let path = record.detail.clone();

            // Extract metadata fields
            let meta = record.metadata.as_ref();
            let summary = meta.and_then(|m| m.get("summary")).and_then(|v| v.as_str()).unwrap_or("");
            let reason = meta.and_then(|m| m.get("reason")).and_then(|v| v.as_str()).unwrap_or("");
            let operation = meta.and_then(|m| m.get("operation")).and_then(|v| v.as_str()).unwrap_or("write");

            // Operation label: human words, no underscores
            let op_word = match operation {
                "write" => "create",
                "overwrite" => "overwrite",
                "replace" | "replace_function" | "replace_symbol" | "replace_lines" => "edit",
                "delete" => "delete",
                "insert_after" => "edit",
                "edit" => "edit",
                _ => "write",
            };

            let diffstat = match record.diffstat {
                Some((a, r)) => format!(" \x1b[32m+{a}\x1b[0m \x1b[31m-{r}\x1b[0m"),
                None => String::new(),
            };

            // Main line: 📝 edit src/app.rs +5 -3
            let mut parts = Vec::new();
            parts.push(format!("{emoji} {op_word} {path}{diffstat}"));

            // Subline: the auto-summary from tree-sitter / compound edit
            if !summary.is_empty() {
                parts.push(format!("  \x1b[2m{summary}\x1b[0m"));
            }

            // Reason from the model
            if !reason.is_empty() {
                parts.push(format!("  \x1b[2m\x1b[3m\u{21B3} {reason}\x1b[0m"));
            }

            let new_line = parts.join("\n");
            let raw_idx = record.raw_index;
            self.raw_output[raw_idx] = RawEntry::Ansi(new_line);
            self.rewrap(self.term_width);
        }
    }

    /// Push ANSI-colored content — parses escape codes into styled spans.
    pub fn push_ansi(&mut self, content: String) {
        self.raw_output.push(RawEntry::Ansi(content.clone()));
        let styled_lines = crate::ansi::parse_to_lines(&content);
        for line in styled_lines {
            for wrapped in wrap_styled_line(&line, self.term_width) {
                let plain: String = wrapped.spans.iter().map(|s| s.content.as_ref()).collect();
                self.output.push(OutputLine {
                    content: plain,
                    styled: Some(wrapped.spans),
                });
            }
        }
        self.scroll_offset = 0;
    }

    /// Push markdown content — renders with styles and wraps.
    pub fn push_markdown(&mut self, content: String) {
        self.raw_output.push(RawEntry::Markdown(content.clone()));
        let styled_lines = crate::markdown::render(&content);
        for line in styled_lines {
            for wrapped in wrap_styled_line(&line, self.term_width) {
                let plain: String = wrapped.spans.iter().map(|s| s.content.as_ref()).collect();
                self.output.push(OutputLine {
                    content: plain,
                    styled: Some(wrapped.spans),
                });
            }
        }
        self.scroll_offset = 0;
    }

    /// Tick token rate animation. Call each frame (~80ms).
    pub fn tick_tokens(&mut self) {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_token_update).as_secs_f64();
        let current_total = self.total_input_tokens + self.total_output_tokens;
        let delta = current_total.saturating_sub(self.prev_total_tokens);

        if delta > 0 && elapsed > 0.0 {
            // Smoothed rate: blend new measurement with previous
            let instant_rate = delta as f64 / elapsed;
            self.token_rate = self.token_rate * 0.6 + instant_rate * 0.4;
            self.token_flash = 6; // highlight for ~6 frames
            self.last_token_update = now;
            self.prev_total_tokens = current_total;
        } else if elapsed > 1.0 {
            // Decay rate when idle
            self.token_rate *= 0.8;
            if self.token_rate < 1.0 {
                self.token_rate = 0.0;
            }
            self.last_token_update = now;
            self.prev_total_tokens = current_total;
        }

        if self.token_flash > 0 {
            self.token_flash -= 1;
        }
    }

    /// Re-wrap all output for a new terminal width.
    pub fn rewrap(&mut self, new_width: usize) {
        self.term_width = new_width;
        self.output.clear();
        for raw in &self.raw_output {
            match raw {
                RawEntry::Plain(text) => {
                    for line in wrap_text(text, new_width) {
                        self.output.push(OutputLine { content: line, styled: None });
                    }
                }
                RawEntry::Markdown(text) => {
                    let styled_lines = crate::markdown::render(text);
                    for line in styled_lines {
                        for wrapped in wrap_styled_line(&line, new_width) {
                            let plain: String = wrapped.spans.iter().map(|s| s.content.as_ref()).collect();
                            self.output.push(OutputLine {
                                content: plain,
                                styled: Some(wrapped.spans),
                            });
                        }
                    }
                }
                RawEntry::Ansi(text) => {
                    let styled_lines = crate::ansi::parse_to_lines(text);
                    for line in styled_lines {
                        for wrapped in wrap_styled_line(&line, new_width) {
                            let plain: String = wrapped.spans.iter().map(|s| s.content.as_ref()).collect();
                            self.output.push(OutputLine {
                                content: plain,
                                styled: Some(wrapped.spans),
                            });
                        }
                    }
                }
            }
        }
        self.scroll_offset = 0;
    }

    /// Rebuild output from raw entries, inserting expanded tool details when expanded_view is on.
    pub fn rebuild_output(&mut self) {
        self.output.clear();
        let width = self.term_width;

        // Build index of which raw_output positions have expanded tool details.
        // Only collect the index, not the data — we'll borrow when rendering.
        let expanded_indices: std::collections::HashSet<usize> = if self.expanded_view {
            self.tool_calls.iter()
                .filter(|r| r.output.is_some())
                .map(|r| r.raw_index)
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        // Render raw entries. We need to split borrows: iterate raw_output
        // by index so we can call render_expanded_tool (which borrows self.tool_calls).
        for i in 0..self.raw_output.len() {
            match &self.raw_output[i] {
                RawEntry::Plain(text) => {
                    let text = text.clone();
                    for line in wrap_text(&text, width) {
                        self.output.push(OutputLine { content: line, styled: None });
                    }
                }
                RawEntry::Markdown(text) => {
                    let styled_lines = crate::markdown::render(text);
                    for line in styled_lines {
                        for wrapped in wrap_styled_line(&line, width) {
                            let plain: String = wrapped.spans.iter().map(|s| s.content.as_ref()).collect();
                            self.output.push(OutputLine {
                                content: plain,
                                styled: Some(wrapped.spans),
                            });
                        }
                    }
                }
                RawEntry::Ansi(text) => {
                    let styled_lines = crate::ansi::parse_to_lines(text);
                    for line in styled_lines {
                        for wrapped in wrap_styled_line(&line, width) {
                            let plain: String = wrapped.spans.iter().map(|s| s.content.as_ref()).collect();
                            self.output.push(OutputLine {
                                content: plain,
                                styled: Some(wrapped.spans),
                            });
                        }
                    }
                }
            }

            if expanded_indices.contains(&i) {
                // Find the tool record for this index and clone just that one
                if let Some(record) = self.tool_calls.iter().find(|r| r.raw_index == i) {
                    let record = record.clone();
                    self.render_expanded_tool(&record, width);
                }
            }
        }
        self.scroll_offset = 0;
    }

    /// Build styled spans for a diff line with inline character-level highlighting.
    /// `text` is this line's content, `other` is the paired line to diff against.
    /// Only the characters that differ get the highlighted background; the rest
    /// are shown in the base color.
    fn inline_diff_spans(
        text: &str,
        line_no: usize,
        op: &str,
        base_style: Style,
        highlight_style: Style,
        dim: Style,
        bar: &str,
        num_width: usize,
        other: &str,
    ) -> Vec<Span<'static>> {
        // Find common prefix length (in chars)
        let text_chars: Vec<char> = text.chars().collect();
        let other_chars: Vec<char> = other.chars().collect();

        let mut prefix_len = 0;
        while prefix_len < text_chars.len()
            && prefix_len < other_chars.len()
            && text_chars[prefix_len] == other_chars[prefix_len]
        {
            prefix_len += 1;
        }

        // Find common suffix length (in chars), not overlapping with prefix
        let mut suffix_len = 0;
        while suffix_len < text_chars.len().saturating_sub(prefix_len)
            && suffix_len < other_chars.len().saturating_sub(prefix_len)
            && text_chars[text_chars.len() - 1 - suffix_len] == other_chars[other_chars.len() - 1 - suffix_len]
        {
            suffix_len += 1;
        }

        let prefix: String = text_chars[..prefix_len].iter().collect();
        let middle_end = text_chars.len().saturating_sub(suffix_len);
        let changed: String = text_chars[prefix_len..middle_end].iter().collect();
        let suffix: String = text_chars[middle_end..].iter().collect();

        let mut spans = vec![
            Span::styled(format!("  {bar} "), dim),
            Span::styled(format!("{:>w$}", line_no, w = num_width), base_style),
            Span::styled(format!(" {op} "), base_style),
        ];

        if !prefix.is_empty() {
            spans.push(Span::styled(prefix, base_style));
        }
        if !changed.is_empty() {
            spans.push(Span::styled(changed, highlight_style));
        }
        if !suffix.is_empty() {
            spans.push(Span::styled(suffix, base_style));
        }

        // Edge case: if text is empty, show nothing after the gutter
        if text_chars.is_empty() {
            // spans already have gutter, that's fine
        }

        spans
    }

    /// Render expanded tool details below the tool call line.
    fn render_expanded_tool(&mut self, record: &ToolCallRecord, _width: usize) {
        let dim = Style::default().fg(Color::DarkGray);
        let green = Style::default().fg(Color::Green);
        let green_bg = Style::default().fg(Color::Green).bg(Color::Rgb(0, 40, 0));
        let red = Style::default().fg(Color::Red);
        let red_bg = Style::default().fg(Color::Red).bg(Color::Rgb(40, 0, 0));
        let bar = "\u{2502}"; // │

        // Write tools: show colored diff with line numbers and inline highlighting
        if record.name == "write" {
            if let Some(diff) = &record.diff {
                // Parse entries
                struct DiffEntry { op: String, line_no: usize, text: String }
                let entries: Vec<DiffEntry> = diff.iter().filter_map(|e| {
                    let parts: Vec<&str> = e.splitn(3, '\t').collect();
                    if parts.len() < 3 { return None; }
                    Some(DiffEntry {
                        op: parts[0].to_string(),
                        line_no: parts[1].parse().unwrap_or(0),
                        text: parts[2].to_string(),
                    })
                }).collect();

                let max_line = entries.iter().map(|e| e.line_no).max().unwrap_or(1);
                let num_width = format!("{max_line}").len();
                let ext = crate::highlight::extension_from_path(&record.detail);
                let hl = &self.highlighter;

                // Group consecutive -/+ blocks for inline diffing
                let mut i = 0;
                while i < entries.len() {
                    let e = &entries[i];

                    if e.op == "..." {
                        // Gap marker between hunks
                        let spans = vec![
                            Span::styled(format!("  {bar} "), dim),
                            Span::styled(format!("{:>w$}", "~", w = num_width), dim),
                            Span::styled("   \u{2504}\u{2504}\u{2504}", dim), // ┄┄┄
                        ];
                        let plain = format!("  {bar} {:>w$}   ┄┄┄", "~", w = num_width);
                        self.output.push(OutputLine { content: plain, styled: Some(spans) });
                        i += 1;
                        continue;
                    }

                    if e.op == " " {
                        // Context line — syntax highlighted but dimmed
                        let mut spans = vec![
                            Span::styled(format!("  {bar} "), dim),
                            Span::styled(format!("{:>w$}", e.line_no, w = num_width), dim),
                            Span::styled("   ", dim),
                        ];
                        // Dim the syntax colors for context
                        for mut span in hl.highlight_line(&e.text, ext, None) {
                            if let Some(fg) = span.style.fg {
                                // Desaturate: blend toward gray
                                span.style = span.style.fg(dim_color(fg));
                            } else {
                                span.style = dim;
                            }
                            spans.push(span);
                        }
                        let plain = format!("  {bar} {:>w$}   {}", e.line_no, e.text, w = num_width);
                        self.output.push(OutputLine { content: plain, styled: Some(spans) });
                        i += 1;
                        continue;
                    }

                    // Collect consecutive removed lines
                    let mut removed: Vec<&DiffEntry> = Vec::new();
                    while i < entries.len() && entries[i].op == "-" {
                        removed.push(&entries[i]);
                        i += 1;
                    }
                    // Collect consecutive added lines
                    let mut added: Vec<&DiffEntry> = Vec::new();
                    while i < entries.len() && entries[i].op == "+" {
                        added.push(&entries[i]);
                        i += 1;
                    }

                    // Pair up removed/added for inline diff, render unpaired lines whole
                    let pairs = removed.len().min(added.len());

                    for j in 0..removed.len() {
                        let r = removed[j];
                        if j < pairs {
                            // Paired: inline diff against added[j]
                            let a = added[j];
                            let spans = Self::inline_diff_spans(
                                &r.text, r.line_no, "-", red, red_bg, dim, bar, num_width,
                                &a.text,
                            );
                            let plain = format!("  {bar} {:>w$} - {}", r.line_no, r.text, w = num_width);
                            self.output.push(OutputLine { content: plain, styled: Some(spans) });
                        } else {
                            // Unpaired removed: syntax highlighted with red background
                            let mut spans = vec![
                                Span::styled(format!("  {bar} "), dim),
                                Span::styled(format!("{:>w$}", r.line_no, w = num_width), red),
                                Span::styled(" - ", red),
                            ];
                            spans.extend(hl.highlight_diff_line(&r.text, ext, Color::Rgb(40, 0, 0)));
                            let plain = format!("  {bar} {:>w$} - {}", r.line_no, r.text, w = num_width);
                            self.output.push(OutputLine { content: plain, styled: Some(spans) });
                        }
                    }

                    for j in 0..added.len() {
                        let a = added[j];
                        if j < pairs {
                            // Paired: inline diff against removed[j]
                            let r = removed[j];
                            let spans = Self::inline_diff_spans(
                                &a.text, a.line_no, "+", green, green_bg, dim, bar, num_width,
                                &r.text,
                            );
                            let plain = format!("  {bar} {:>w$} + {}", a.line_no, a.text, w = num_width);
                            self.output.push(OutputLine { content: plain, styled: Some(spans) });
                        } else {
                            // Unpaired added: syntax highlighted with green background
                            let mut spans = vec![
                                Span::styled(format!("  {bar} "), dim),
                                Span::styled(format!("{:>w$}", a.line_no, w = num_width), green),
                                Span::styled(" + ", green),
                            ];
                            spans.extend(hl.highlight_diff_line(&a.text, ext, Color::Rgb(0, 40, 0)));
                            let plain = format!("  {bar} {:>w$} + {}", a.line_no, a.text, w = num_width);
                            self.output.push(OutputLine { content: plain, styled: Some(spans) });
                        }
                    }
                }
            }
            return;
        }

        let output = match &record.output {
            Some(o) if !o.is_empty() => o,
            _ => return,
        };

        // Read tool: show code content with line numbers
        if record.name == "read" {
            let line_count = record.metadata.as_ref()
                .and_then(|m| m.get("lines")).and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let layer = record.metadata.as_ref()
                .and_then(|m| m.get("layer")).and_then(|v| v.as_str())
                .unwrap_or("raw");
            let ext = crate::highlight::extension_from_path(&record.detail);
            let hl = &self.highlighter;

            // Extract just the code content (skip headers/separators)
            let lines: Vec<&str> = output.lines().collect();
            let display_limit = 30;
            let mut in_code = false;
            let mut code_lines: Vec<&str> = Vec::new();

            for line in &lines {
                if line.starts_with("```") {
                    in_code = !in_code;
                    continue;
                }
                if in_code {
                    code_lines.push(line);
                }
            }

            // If no fenced code found, show the raw output lines
            if code_lines.is_empty() {
                code_lines = lines.iter()
                    .filter(|l| !l.starts_with("───") && !l.starts_with("##") && !l.is_empty())
                    .copied()
                    .collect();
            }

            let num_width = if code_lines.is_empty() { 1 } else {
                format!("{}", code_lines.len()).len()
            };

            // Header
            let header = format!("{layer} · {line_count} lines");
            let header_line = Line::from(vec![
                Span::styled(format!("  {bar} "), dim),
                Span::styled(header, dim),
            ]);
            self.output.push(OutputLine {
                content: format!("  {bar} {layer} · {line_count} lines"),
                styled: Some(header_line.spans),
            });

            for (j, line) in code_lines.iter().enumerate().take(display_limit) {
                let line_no = j + 1;
                let mut spans = vec![
                    Span::styled(format!("  {bar} "), dim),
                    Span::styled(format!("{:>w$}", line_no, w = num_width), dim),
                    Span::styled(" \u{2502} ", dim),
                ];
                spans.extend(hl.highlight_line(line, ext, None));
                let plain = format!("  {bar} {:>w$} \u{2502} {line}", line_no, w = num_width);
                self.output.push(OutputLine { content: plain, styled: Some(spans) });
            }

            if code_lines.len() > display_limit {
                let remaining = code_lines.len() - display_limit;
                let truncated = Line::from(vec![
                    Span::styled(format!("  {bar} "), dim),
                    Span::styled(format!("... {remaining} more lines"), dim),
                ]);
                self.output.push(OutputLine {
                    content: format!("  {bar} ... {remaining} more lines"),
                    styled: Some(truncated.spans),
                });
            }
            return;
        }

        // Generic tool: show output lines dimmed
        {
            for line in output.lines().take(20) {
                let styled_line = Line::from(vec![
                    Span::styled(format!("  {bar} "), dim),
                    Span::styled(line.to_string(), dim),
                ]);
                self.output.push(OutputLine {
                    content: format!("  {bar} {line}"),
                    styled: Some(styled_line.spans),
                });
            }
            let total_lines = output.lines().count();
            if total_lines > 20 {
                let remaining = total_lines - 20;
                let truncated = Line::from(vec![
                    Span::styled(format!("  {bar} "), dim),
                    Span::styled(format!("... {remaining} more lines"), dim),
                ]);
                self.output.push(OutputLine {
                    content: format!("  {bar} ... {remaining} more lines"),
                    styled: Some(truncated.spans),
                });
            }
        }
    }
}

/// Events from the TUI to the main loop.
pub enum AppEvent {
    /// User submitted a line of input.
    Submit(String),
    /// User wants to quit.
    Quit,
    /// User double-entered to interrupt.
    Interrupt,
    /// Clear conversation context.
    Clear,
    /// Voice audio captured, needs transcription (runs in async context).
    VoiceAudio(Vec<f32>),
    /// Voice transcription completed.
    VoiceTranscription(String),
    /// Spawn a background process (from bash tty/interactive mode).
    SpawnBackground { command: String, mode: String },
    /// Attach to a background terminal.
    Attach(u32),
    /// Detach from background terminal.
    Detach,
    /// Forward raw bytes to attached process.
    ProcessInput(Vec<u8>),
}

/// The TUI application.
pub struct App {
    state: Arc<Mutex<AppState>>,
    input_buffer: String,
    input_cursor: usize,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    last_enter: Option<std::time::Instant>,
    last_quit_attempt: Option<std::time::Instant>,
    /// Input history for up/down cycling.
    history: Vec<String>,
    /// Current position in history (None = not browsing).
    history_index: Option<usize>,
    /// Saved current input when browsing history.
    history_stash: String,
    /// Gamepad support.
    gilrs: Option<gilrs::Gilrs>,
    /// Start button hold tracking for couch mode toggle.
    start_held_since: Option<std::time::Instant>,
    /// When Select was last pressed (for short-tap vs hold detection).
    voice_press_time: Option<std::time::Instant>,
    /// Whether current recording was started by a short tap (toggle mode).
    voice_toggled: bool,
    /// Active voice capture.
    voice_capture: Option<crate::voice::VoiceCapture>,
    /// Left stick repeat throttle — last time we fired a stick-driven action.
    stick_last_action: Option<std::time::Instant>,
    /// Previous stick Y direction to detect new deflections.
    stick_prev_y: i8,
    /// Byte offset in input_buffer where voice transcription text starts (for replacement).
    voice_insert_start: Option<usize>,
    /// Last time we sent audio for streaming transcription.
    voice_last_transcribe: Option<std::time::Instant>,
}

const DOUBLE_ENTER_MS: u64 = 300;

/// Available slash commands with descriptions.
const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/attach", "Attach to background terminal (e.g. /attach 1)"),
    ("/clean", "Remove completed background terminals"),
    ("/clear", "Clear conversation context and output"),
    ("/compact", "Compress conversation history"),
    ("/couch", "Toggle couch/gamepad mode"),
    ("/context", "Toggle context window display"),
    ("/effort", "Set reasoning effort (low/medium/high)"),
    ("/help", "Show available commands"),
    ("/jobs", "Show background terminals"),
    ("/model", "Switch model or toggle model display"),
    ("/project", "Toggle project path display"),
    ("/ps", "Show background terminals"),
    ("/rates", "Toggle provider pressure display"),
    ("/usage", "Toggle token usage display"),
];

/// Get matching slash commands for the current input prefix.
fn slash_suggestions(input: &str) -> Vec<(&'static str, &'static str)> {
    if !input.starts_with('/') || input.contains(' ') {
        return Vec::new();
    }
    SLASH_COMMANDS.iter()
        .filter(|(cmd, _)| cmd.starts_with(input) && *cmd != input)
        .copied()
        .collect()
}

// Throbber animation chars
const THROBBER_FRAMES: [[&str; 4]; 8] = [
    ["▇", "▅", "▃", "▁"],
    ["▅", "▇", "▅", "▃"],
    ["▃", "▅", "▇", "▅"],
    ["▁", "▃", "▅", "▇"],
    ["▃", "▅", "▇", "▅"],
    ["▅", "▇", "▅", "▃"],
    ["▇", "▅", "▃", "▁"],
    ["▅", "▃", "▁", "▃"],
];

impl App {
    pub fn new(state: Arc<Mutex<AppState>>, event_tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            state,
            input_buffer: String::new(),
            input_cursor: 0,
            event_tx,
            last_enter: None,
            last_quit_attempt: None,
            history: Vec::new(),
            history_index: None,
            history_stash: String::new(),
            gilrs: gilrs::Gilrs::new().ok(),
            start_held_since: None,
            voice_press_time: None,
            voice_toggled: false,
            voice_capture: None,
            stick_last_action: None,
            stick_prev_y: 0,
            voice_insert_start: None,
            voice_last_transcribe: None,
        }
    }

    /// Run the TUI event loop. Blocks until quit.
    pub fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        crossterm::execute!(
            io::stdout(),
            EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
            crossterm::event::EnableBracketedPaste,
        )?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

        loop {
            // Tick animations
            {
                let mut state = self.state.lock().unwrap();
                if state.agent_active {
                    state.throbber_frame = (state.throbber_frame + 1) % 8;
                }
                state.tick_tokens();
                if state.couch_mode_notify > 0 {
                    state.couch_mode_notify -= 1;
                }
            }

            // Check for text to insert from voice transcription
            {
                let mut state = self.state.lock().unwrap();
                if let Some(text) = state.pending_insert.take() {
                    // If we have a voice insert region, replace it
                    if let Some(start) = self.voice_insert_start {
                        let end = self.input_cursor.min(self.input_buffer.len());
                        if start <= end && start <= self.input_buffer.len() {
                            self.input_buffer.replace_range(start..end, &text);
                            self.input_cursor = start + text.len();
                        }
                    } else {
                        // First insert — mark the start position
                        self.voice_insert_start = Some(self.input_cursor);
                        self.input_buffer.insert_str(self.input_cursor, &text);
                        self.input_cursor += text.len();
                    }
                }
            }

            // Poll gamepad
            self.poll_gamepad();

            terminal.draw(|frame| self.render(frame))?;

            // Poll with short timeout for animation
            if event::poll(Duration::from_millis(80))? {
                let ev = event::read()?;

                if let Event::Resize(w, _) = ev {
                    let mut state = self.state.lock().unwrap();
                    state.rewrap(w as usize);
                    continue;
                }

                if let Event::Mouse(mouse) = ev {
                    match mouse.kind {
                        event::MouseEventKind::ScrollUp => {
                            let mut state = self.state.lock().unwrap();
                            let max_scroll = state.output.len().saturating_sub(1);
                            state.scroll_offset = (state.scroll_offset + 3).min(max_scroll);
                        }
                        event::MouseEventKind::ScrollDown => {
                            let mut state = self.state.lock().unwrap();
                            state.scroll_offset = state.scroll_offset.saturating_sub(3);
                        }
                        _ => {}
                    }
                    continue;
                }

                if let Event::Paste(text) = ev {
                    // Multiline paste — normalize line endings and insert
                    let text = text.replace("\r\n", "\n").replace('\r', "\n");
                    self.input_buffer.insert_str(self.input_cursor, &text);
                    self.input_cursor += text.len();
                    self.voice_insert_start = None;
                    continue;
                }

                let key = match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => key,
                    _ => continue,
                };

                // Attached terminal mode — forward all keystrokes
                {
                    let state = self.state.lock().unwrap();
                    if state.attached_process.is_some() {
                        drop(state);
                        // Ctrl-D or Esc detaches
                        if key.code == KeyCode::Esc
                            || (key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL))
                        {
                            let mut s = self.state.lock().unwrap();
                            let pid = s.attached_process.take().unwrap();
                            s.push_output(format!("Detached from #{pid}"));
                        } else {
                            // Forward keystroke as bytes to the process via registry
                            let bytes = key_to_bytes(key.code, key.modifiers);
                            if !bytes.is_empty() {
                                let state = self.state.lock().unwrap();
                                if let Some(pid) = state.attached_process {
                                    if let Some(registry) = &state.bash_process_registry {
                                        if let Some(writer) = registry.blocking_lock().writer(pid) {
                                            let _ = writer.try_send(bytes);
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }
                }

                // Survey and menu input take priority
                if self.handle_job_browser_key(key.code) || self.handle_survey_key(key.code, key.modifiers) || self.handle_menu_key(key.code) {
                    continue;
                }

                    let prefix_width: usize = 2;
                    let input_content_width = crossterm::terminal::size()
                        .map(|(w, _)| w.saturating_sub(2) as usize)
                        .unwrap_or(78);
                    match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if !self.input_buffer.is_empty() {
                                // Clear the input buffer
                                self.input_buffer.clear();
                                self.input_cursor = 0;
                                self.history_index = None;
                            } else {
                                let state = self.state.lock().unwrap();
                                if state.agent_active {
                                    drop(state);
                                    let _ = self.event_tx.send(AppEvent::Interrupt);
                                } else {
                                    drop(state);
                                    if self.try_quit() {
                                        return Ok(());
                                    }
                                }
                            }
                        }
                        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            // Ctrl+V: check clipboard for images/files
                            // (text paste is handled by bracketed paste above)
                            match crate::clipboard::read() {
                                crate::clipboard::ClipboardContent::Image(path) => {
                                    let ref_text = format!("[image: {}]", path.display());
                                    self.input_buffer.insert_str(self.input_cursor, &ref_text);
                                    self.input_cursor += ref_text.len();
                                    let mut state = self.state.lock().unwrap();
                                    state.status_message = Some("Image pasted from clipboard".to_string());
                                }
                                crate::clipboard::ClipboardContent::Files(files) => {
                                    let refs: Vec<String> = files.iter()
                                        .map(|f| f.display().to_string())
                                        .collect();
                                    let text = refs.join("\n");
                                    self.input_buffer.insert_str(self.input_cursor, &text);
                                    self.input_cursor += text.len();
                                }
                                crate::clipboard::ClipboardContent::Text(text) => {
                                    // Fallback if bracketed paste didn't fire
                                    self.input_buffer.insert_str(self.input_cursor, &text);
                                    self.input_cursor += text.len();
                                }
                                crate::clipboard::ClipboardContent::Empty => {}
                            }
                            self.voice_insert_start = None;
                        }
                        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            let mut s = self.state.lock().unwrap();
                            s.expanded_view = !s.expanded_view;
                            let label = if s.expanded_view { "expanded" } else { "compact" };
                            s.status_message = Some(format!("Output view: {label}"));
                            // Rebuild output from raw entries with new view mode
                            s.rebuild_output();
                        }
                        KeyCode::Esc => {
                            let state = self.state.lock().unwrap();
                            if state.agent_active {
                                drop(state);
                                let _ = self.event_tx.send(AppEvent::Interrupt);
                            } else {
                                drop(state);
                                if self.try_quit() {
                                    return Ok(());
                                }
                            }
                        }
                        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                            // Shift-enter: insert newline (terminals that support kitty protocol)
                            self.input_buffer.insert(self.input_cursor, '\n');
                            self.input_cursor += 1;
                        }
                        // Many terminals send Shift+Enter as Char('j') with SHIFT
                        // (Enter = ^J = 0x0A, Shift adds the modifier but the key stays 'j')
                        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::SHIFT)
                            && !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            self.input_buffer.insert(self.input_cursor, '\n');
                            self.input_cursor += 1;
                        }
                        KeyCode::Enter => {
                            // Check for trailing backslash — means "insert newline, don't submit"
                            if self.input_cursor > 0 && self.input_buffer.as_bytes().get(self.input_cursor - 1) == Some(&b'\\') {
                                // Replace the backslash with a newline
                                self.input_cursor -= 1;
                                self.input_buffer.remove(self.input_cursor);
                                self.input_buffer.insert(self.input_cursor, '\n');
                                self.input_cursor += 1;
                            } else {
                                let now = std::time::Instant::now();

                                // Check double-enter for interrupt
                                let is_double = self.last_enter
                                    .map(|prev| now.duration_since(prev).as_millis() < DOUBLE_ENTER_MS as u128)
                                    .unwrap_or(false);
                                self.last_enter = Some(now);

                                let state = self.state.lock().unwrap();
                                if state.agent_active && is_double && self.input_buffer.is_empty() {
                                    drop(state);
                                    let _ = self.event_tx.send(AppEvent::Interrupt);
                                } else {
                                    drop(state);
                                    if !self.input_buffer.is_empty() {
                                        let line = self.input_buffer.clone();

                                        // Handle TUI-local slash commands that work even during agent execution
                                        if self.handle_local_slash(&line) {
                                            self.input_buffer.clear();
                                            self.input_cursor = 0;
                                        } else {
                                            self.history.push(line.clone());
                                            self.history_index = None;
                                            self.input_buffer.clear();
                                            self.input_cursor = 0;
                                            let _ = self.event_tx.send(AppEvent::Submit(line));
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.input_cursor = move_to_segment_start(
                                &self.input_buffer,
                                self.input_cursor,
                                input_content_width,
                                prefix_width,
                            );
                        }
                        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.input_cursor = move_to_segment_end(
                                &self.input_buffer,
                                self.input_cursor,
                                input_content_width,
                                prefix_width,
                            );
                        }
                        KeyCode::Char(c) => {
                            self.input_buffer.insert(self.input_cursor, c);
                            self.input_cursor += c.len_utf8();
                            // Clear status and voice region on manual typing
                            self.last_quit_attempt = None;
                            self.voice_insert_start = None;
                            let mut s = self.state.lock().unwrap();
                            s.status_message = None;
                        }
                        KeyCode::Backspace => {
                            if self.input_cursor > 0 {
                                self.input_cursor -= 1;
                                while self.input_cursor > 0 && !self.input_buffer.is_char_boundary(self.input_cursor) {
                                    self.input_cursor -= 1;
                                }
                                self.input_buffer.remove(self.input_cursor);
                            }
                        }
                        KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                            self.input_cursor = move_word_left(&self.input_buffer, self.input_cursor);
                        }
                        KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                            self.input_cursor = move_word_right(&self.input_buffer, self.input_cursor);
                        }
                        KeyCode::Left => {
                            self.input_cursor = prev_char_boundary(&self.input_buffer, self.input_cursor);
                        }
                        KeyCode::Right => {
                            self.input_cursor = next_char_boundary(&self.input_buffer, self.input_cursor);
                        }
                        KeyCode::Tab => {
                            // Complete first matching slash command
                            let suggestions = slash_suggestions(&self.input_buffer);
                            if let Some((cmd, _)) = suggestions.first() {
                                self.input_buffer = cmd.to_string();
                                self.input_cursor = self.input_buffer.len();
                            }
                        }
                        KeyCode::Home => {
                            self.input_cursor = 0;
                        }
                        KeyCode::End => {
                            self.input_cursor = self.input_buffer.len();
                        }
                        KeyCode::Up => {
                            // If agent is active and there are queued messages, pop last one for editing
                            {
                                let mut s = self.state.lock().unwrap();
                                if s.agent_active && !s.queued_messages.is_empty() {
                                    let msg = s.queued_messages.pop().unwrap();
                                    drop(s);
                                    self.input_buffer = msg;
                                    self.input_cursor = self.input_buffer.len();
                                    continue;
                                }
                            }
                            if self.history.is_empty() {
                                continue;
                            }
                            match self.history_index {
                                None => {
                                    // Start browsing from the end
                                    self.history_stash = self.input_buffer.clone();
                                    self.history_index = Some(self.history.len() - 1);
                                }
                                Some(idx) if idx > 0 => {
                                    self.history_index = Some(idx - 1);
                                }
                                _ => {} // Already at oldest
                            }
                            if let Some(idx) = self.history_index {
                                self.input_buffer = self.history[idx].clone();
                                self.input_cursor = self.input_buffer.len();
                            }
                        }
                        KeyCode::Down => {
                            match self.history_index {
                                Some(idx) if idx + 1 < self.history.len() => {
                                    self.history_index = Some(idx + 1);
                                    self.input_buffer = self.history[idx + 1].clone();
                                    self.input_cursor = self.input_buffer.len();
                                }
                                Some(_) => {
                                    // Past the end — restore stash
                                    self.history_index = None;
                                    self.input_buffer = self.history_stash.clone();
                                    self.input_cursor = self.input_buffer.len();
                                }
                                None => {} // Not browsing
                            }
                        }
                        KeyCode::PageUp => {
                            let mut state = self.state.lock().unwrap();
                            let max_scroll = state.output.len().saturating_sub(1);
                            state.scroll_offset = (state.scroll_offset + 20).min(max_scroll);
                        }
                        KeyCode::PageDown => {
                            let mut state = self.state.lock().unwrap();
                            state.scroll_offset = state.scroll_offset.saturating_sub(20);
                        }
                        _ => {}
                    }
            }
        }
    }

    fn render(&self, frame: &mut Frame) {
        let state = self.state.lock().unwrap();

        // Input area height: account for soft-wrapping within each line.
        // The first visual segment of each hard line includes the prompt/indent prefix,
        // but wrapped continuation segments do not. We use the same model here and for
        // cursor positioning so multiline input stays visually aligned.
        let prefix_width: u16 = 2; // "› " or "  "
        let input_content_width = frame.area().width.saturating_sub(2) as usize;
        let input_line_count: usize = wrapped_input_line_count(
            &self.input_buffer,
            input_content_width,
            prefix_width as usize,
        );
        let input_height = (input_line_count as u16 + 2).max(3);

        // Layout: output, input, status bar
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(input_height),
                Constraint::Length(1),
            ])
            .split(frame.area());

        // ── Output pane ──
        let output_area = chunks[0];
        let visible_height = output_area.height as usize;
        let total_lines = state.output.len();

        // Pad with empty lines so content is bottom-aligned
        let padding = visible_height.saturating_sub(total_lines);
        let mut all_lines: Vec<Line> = Vec::with_capacity(padding + total_lines);
        for _ in 0..padding {
            all_lines.push(Line::raw(""));
        }
        for line in &state.output {
            if let Some(spans) = &line.styled {
                all_lines.push(Line::from(spans.clone()));
            } else {
                all_lines.push(Line::raw(&line.content));
            }
        }

        // Render job browser if open
        if let Some(browser) = &state.job_browser {
            all_lines.push(Line::raw(""));
            match &browser.mode {
                JobBrowserMode::List => {
                    all_lines.push(Line::styled(
                        "  Background terminals",
                        Style::default().fg(Color::White).bold(),
                    ));
                    all_lines.push(Line::raw(""));
                    for (i, job) in state.jobs.iter().enumerate() {
                        let at_cursor = i == browser.cursor;
                        let elapsed = job.started.elapsed().as_secs();
                        let icon = match job.status {
                            JobStatus::Running => "◐",
                            JobStatus::Done => "✔",
                            JobStatus::Failed => "✗",
                        };
                        let status_text = match job.status {
                            JobStatus::Running => format!("Running, {elapsed}s"),
                            JobStatus::Done => format!("Success, ran for {elapsed}s"),
                            JobStatus::Failed => format!("Failed, after {elapsed}s"),
                        };
                        let arrow = if at_cursor { "\u{203a}" } else { " " };
                        let label_color = if at_cursor { Color::Cyan } else { Color::White };
                        let status_color = if at_cursor { Color::Cyan } else { Color::DarkGray };

                        all_lines.push(Line::from(vec![
                            Span::raw(format!("  {arrow} {icon} ")),
                            Span::styled(&job.task, Style::default().fg(label_color)),
                            Span::styled(format!(" ({status_text})"), Style::default().fg(status_color)),
                        ]));
                    }
                    all_lines.push(Line::raw(""));
                    all_lines.push(Line::styled(
                        "  Enter: view output · Tab/X: actions · Esc: close",
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                JobBrowserMode::ViewOutput(job_id) => {
                    if let Some(job) = state.jobs.iter().find(|j| j.id == *job_id) {
                        all_lines.push(Line::styled(
                            format!("  Output: {} (#{job_id})", job.task),
                            Style::default().fg(Color::Cyan).bold(),
                        ));
                        all_lines.push(Line::raw(""));
                        // TODO: show buffered output from the process
                        all_lines.push(Line::styled(
                            "  (output streaming not yet wired)",
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    all_lines.push(Line::raw(""));
                    all_lines.push(Line::styled(
                        "  Esc: back to list",
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                JobBrowserMode::ContextMenu(job_id, menu_cursor) => {
                    if let Some(job) = state.jobs.iter().find(|j| j.id == *job_id) {
                        all_lines.push(Line::styled(
                            format!("  {} (#{job_id})", job.task),
                            Style::default().fg(Color::White).bold(),
                        ));
                    }
                    all_lines.push(Line::raw(""));
                    for (i, option) in JOB_CONTEXT_MENU.iter().enumerate() {
                        let at_cursor = i == *menu_cursor;
                        let arrow = if at_cursor { "\u{203a}" } else { " " };
                        let color = if at_cursor { Color::Cyan } else { Color::White };
                        all_lines.push(Line::from(Span::styled(
                            format!("  {arrow} {option}"),
                            Style::default().fg(color),
                        )));
                    }
                    all_lines.push(Line::raw(""));
                    all_lines.push(Line::styled(
                        "  Enter: select · Esc: back",
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        }

        // Render agent survey if pending
        if let Some(survey) = &state.pending_survey {
            all_lines.push(Line::raw(""));
            all_lines.push(Line::styled(
                format!("  {}", survey.prompt),
                Style::default().fg(Color::Cyan),
            ));
            all_lines.push(Line::raw(""));

            let max_label = survey.options.iter().map(|o| o.label.len()).max().unwrap_or(0);

            for (i, opt) in survey.options.iter().enumerate() {
                let at_cursor = i == survey.cursor;
                let is_selected = survey.selected[i];
                let num = i + 1;
                let arrow = if at_cursor { "\u{203a}" } else { " " };
                let check = if survey.multi {
                    if is_selected { "[x]" } else { "[ ]" }
                } else {
                    ""
                };
                let label_color = if at_cursor { Color::Cyan } else { Color::White };
                let desc_color = if at_cursor { Color::Cyan } else { Color::DarkGray };
                let desc = opt.description.as_deref().unwrap_or("");
                let pad = " ".repeat(max_label.saturating_sub(opt.label.len()));

                let mut spans = vec![
                    Span::raw(format!("  {arrow} ")),
                ];
                if survey.multi {
                    let check_style = if is_selected { Color::Green } else { Color::DarkGray };
                    spans.push(Span::styled(format!("{check} "), Style::default().fg(check_style)));
                }
                spans.push(Span::styled(format!("{num}. {}", opt.label), Style::default().fg(label_color)));
                spans.push(Span::raw(pad));
                if !desc.is_empty() {
                    spans.push(Span::styled(format!("  {desc}"), Style::default().fg(desc_color)));
                }
                all_lines.push(Line::from(spans));
            }

            all_lines.push(Line::raw(""));
            let hint = if survey.multi {
                "  space to select | enter to submit | esc to cancel"
            } else {
                "  enter to submit | esc to cancel"
            };
            all_lines.push(Line::styled(hint, Style::default().fg(Color::DarkGray)));
        }

        // Render interactive menu if active
        if let Some(menu) = &state.active_menu {
            all_lines.push(Line::raw(""));
            all_lines.push(Line::styled(
                format!("  {}", menu.prompt),
                Style::default().fg(Color::Cyan),
            ));
            all_lines.push(Line::raw(""));

            let max_label = menu.options.iter().map(|o| o.label.len()).max().unwrap_or(0);

            for (i, opt) in menu.options.iter().enumerate() {
                let at_cursor = i == menu.cursor;
                let num = i + 1;
                let arrow = if at_cursor { "\u{203a}" } else { " " };

                if opt.enabled {
                    let label_color = if at_cursor { Color::Cyan } else { Color::White };
                    let desc_color = if at_cursor { Color::Cyan } else { Color::DarkGray };
                    let desc = opt.description.as_deref().unwrap_or("");
                    let pad = " ".repeat(max_label.saturating_sub(opt.label.len()));
                    all_lines.push(Line::from(vec![
                        Span::raw(format!("  {arrow} ")),
                        Span::styled(format!("{num}. {}", opt.label), Style::default().fg(label_color)),
                        Span::raw(pad),
                        Span::styled(format!("  {desc}"), Style::default().fg(desc_color)),
                    ]));
                } else {
                    let desc = opt.description.as_deref().unwrap_or("");
                    let pad = " ".repeat(max_label.saturating_sub(opt.label.len()));
                    let dim = Style::default().fg(Color::DarkGray);
                    all_lines.push(Line::from(vec![
                        Span::styled(format!("    {num}. {}", opt.label), dim),
                        Span::styled(pad, dim),
                        Span::styled(format!("  {desc}"), dim),
                    ]));
                }
            }

            all_lines.push(Line::raw(""));
            all_lines.push(Line::styled(
                "  enter to select | esc to cancel",
                Style::default().fg(Color::DarkGray),
            ));
        }

        // Scroll position: 0 = at bottom (most recent), so convert to top-based offset
        let padded_total = all_lines.len();
        let max_scroll = padded_total.saturating_sub(visible_height);
        let clamped_offset = state.scroll_offset.min(max_scroll);
        let scroll_top = max_scroll.saturating_sub(clamped_offset);

        let output_widget = Paragraph::new(all_lines)
            .scroll((scroll_top as u16, 0));
        frame.render_widget(output_widget, output_area);

        // ── Input pane ── (chunks[1])
        let input_area = chunks[1];

        let input_block = Block::default()
            .borders(Borders::TOP)
            .border_type(ratatui::widgets::BorderType::Plain)
            .border_style(Style::default().fg(Color::DarkGray));

        // Build input content: › prefix + buffer
        let prompt_span = Span::styled("\u{203a} ", Style::default().fg(Color::White).bold());

        let input_lines: Vec<Line> = if self.input_buffer.is_empty() {
            vec![Line::from(vec![prompt_span])]
        } else {
            let lines: Vec<&str> = self.input_buffer.split('\n').collect();
            lines.iter().enumerate().map(|(i, text)| {
                if i == 0 {
                    Line::from(vec![prompt_span.clone(), Span::raw(*text)])
                } else {
                    Line::from(vec![Span::styled("  ", Style::default()), Span::raw(*text)])
                }
            }).collect()
        };

        let input_text = Paragraph::new(input_lines)
            .style(Style::default().bg(Color::Rgb(30, 30, 30)))
            .block(input_block)
            .wrap(Wrap { trim: false });
        frame.render_widget(input_text, input_area);

        // Cursor position — account for soft-wrapping.
        // The first visual segment of each hard line is prefixed, but soft-wrapped
        // continuation segments are not, so only add the prefix on segment starts.
        let (visual_line, visual_col, continuation) = visual_cursor_position(
            &self.input_buffer,
            self.input_cursor,
            input_content_width,
            prefix_width as usize,
        );
        let cursor_x = input_area.x + if continuation { 0 } else { prefix_width } + visual_col as u16;
        let cursor_y = input_area.y + 1 + visual_line as u16;
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));

        // ── Slash command suggestions ──
        let suggestions = slash_suggestions(&self.input_buffer);
        if !suggestions.is_empty() {
            let suggestion_lines: Vec<Line> = suggestions.iter().map(|(cmd, desc)| {
                Line::from(vec![
                    Span::styled(format!("  {cmd}"), Style::default().fg(Color::Cyan)),
                    Span::styled(format!("  {desc}"), Style::default().fg(Color::DarkGray)),
                ])
            }).collect();
            let suggestion_height = suggestion_lines.len() as u16;
            // Position above the input area
            let suggestion_y = input_area.y.saturating_sub(suggestion_height);
            let suggestion_area = Rect::new(
                input_area.x,
                suggestion_y,
                input_area.width,
                suggestion_height,
            );
            let suggestion_widget = Paragraph::new(suggestion_lines)
                .style(Style::default().bg(Color::Rgb(40, 40, 40)));
            frame.render_widget(suggestion_widget, suggestion_area);
        }

        // ── Status bar ── (chunks[2])
        let status_area = chunks[2];
        let mut status_parts: Vec<Span> = Vec::new();

        // Left side: recording meter > throbber > status message
        if state.recording {
            let meter: String = state.voice_meter.history.iter().collect();
            status_parts.push(Span::styled(
                format!(" \u{1F3A4} {meter} recording "),
                Style::default().fg(Color::Red),
            ));
        } else if state.agent_active {
            let frame_idx = state.throbber_frame as usize % THROBBER_FRAMES.len();
            let chars = &THROBBER_FRAMES[frame_idx];
            status_parts.push(Span::styled(
                format!(" {} working ", chars.join("")),
                Style::default().fg(Color::Yellow),
            ));
        } else if let Some(pid) = state.attached_process {
            status_parts.push(Span::styled(
                format!(" 🖥 Attached to #{pid} — Esc to detach "),
                Style::default().fg(Color::Cyan),
            ));
        } else if let Some(msg) = &state.status_message {
            status_parts.push(Span::styled(
                format!(" {msg} "),
                Style::default().fg(Color::DarkGray),
            ));
        }

        // Right-aligned parts as individually styled spans
        let dim = Style::default().fg(Color::DarkGray);
        let sep = Span::styled(" · ", dim);
        let mut right_spans: Vec<Span> = Vec::new();

        if state.couch_mode {
            right_spans.push(Span::styled("🎮", Style::default()));
            right_spans.push(sep.clone());
        }

        if state.show_model && !state.model_name.is_empty() {
            right_spans.push(Span::styled(state.model_name.clone(), dim));
        }

        if state.show_usage && (state.total_input_tokens > 0 || state.total_output_tokens > 0) {
            if !right_spans.is_empty() { right_spans.push(sep.clone()); }

            // Color based on token rate
            let token_color = token_rate_color(state.token_rate, state.token_flash);

            let cache_info = if state.total_cache_read > 0 {
                format!(" ({}↓ cache)", format_tokens(state.total_cache_read))
            } else {
                String::new()
            };

            // Input tokens + green arrow
            right_spans.push(Span::styled(
                format_tokens(state.total_input_tokens),
                Style::default().fg(token_color),
            ));
            right_spans.push(Span::styled("↑", Style::default().fg(Color::Green)));
            right_spans.push(Span::styled(" ", dim));
            // Output tokens + red arrow
            right_spans.push(Span::styled(
                format_tokens(state.total_output_tokens),
                Style::default().fg(token_color),
            ));
            right_spans.push(Span::styled("↓", Style::default().fg(Color::Red)));
            if !cache_info.is_empty() {
                right_spans.push(Span::styled(cache_info, dim));
            }
        }

        if state.show_context {
            if !right_spans.is_empty() { right_spans.push(sep.clone()); }
            let used = state.last_input_tokens;
            let pct = if state.context_window > 0 && used > 0 {
                100u64.saturating_sub((used * 100) / state.context_window)
            } else if used == 0 && state.total_input_tokens > 0 {
                // No last_input_tokens yet (e.g. resumed session) — estimate from cumulative
                100
            } else {
                100
            };
            let ctx_color = if pct > 50 {
                Color::DarkGray
            } else if pct > 20 {
                Color::Yellow
            } else {
                Color::Red
            };
            right_spans.push(Span::styled(
                format!("{pct}% context left"),
                Style::default().fg(ctx_color),
            ));
        }

        if state.show_project && !state.project_path.is_empty() {
            if !right_spans.is_empty() { right_spans.push(sep.clone()); }
            right_spans.push(Span::styled(state.project_path.clone(), dim));
        }

        // Calculate gap for right-alignment
        let left_len: usize = status_parts.iter().map(|s| s.content.len()).sum();
        let right_len: usize = right_spans.iter().map(|s| s.content.len()).sum::<usize>() + 1;
        let gap = (status_area.width as usize).saturating_sub(left_len + right_len);

        status_parts.push(Span::raw(" ".repeat(gap)));
        status_parts.extend(right_spans);
        status_parts.push(Span::raw(" "));

        let status_line = Paragraph::new(Line::from(status_parts));
        frame.render_widget(status_line, status_area);
    }

}

const QUIT_WINDOW_MS: u128 = 2000;

/// Color for token counter based on rate and flash state.
/// Idle = dim, slow = gray, medium = white, fast = yellow, blazing = orange
fn token_rate_color(rate: f64, flash: u8) -> Color {
    if flash > 4 {
        // Fresh burst — bright flash
        return Color::White;
    }
    if flash > 2 {
        return Color::Rgb(200, 200, 200);
    }

    if rate < 1.0 {
        Color::DarkGray
    } else if rate < 100.0 {
        Color::Gray
    } else if rate < 500.0 {
        Color::White
    } else if rate < 2000.0 {
        Color::Yellow
    } else if rate < 5000.0 {
        Color::Rgb(255, 165, 0) // orange
    } else {
        Color::Rgb(255, 100, 50) // hot orange-red
    }
}

/// Format token count as human-readable (e.g. 1.2k, 45.3k, 1.1M).
fn format_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

/// Convert a key event to raw bytes for process stdin.
fn key_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
    match code {
        KeyCode::Char(c) => {
            if modifiers.contains(KeyModifiers::CONTROL) {
                // Ctrl+letter = ASCII 1-26
                let ctrl = (c as u8).wrapping_sub(b'a').wrapping_add(1);
                vec![ctrl]
            } else {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                s.as_bytes().to_vec()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        _ => vec![],
    }
}

fn prev_char_boundary(buffer: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let mut next = cursor - 1;
    while next > 0 && !buffer.is_char_boundary(next) {
        next -= 1;
    }
    next
}

fn next_char_boundary(buffer: &str, cursor: usize) -> usize {
    if cursor >= buffer.len() {
        return buffer.len();
    }
    let mut next = cursor + 1;
    while next < buffer.len() && !buffer.is_char_boundary(next) {
        next += 1;
    }
    next.min(buffer.len())
}

fn move_word_left(buffer: &str, cursor: usize) -> usize {
    let mut pos = cursor;
    while pos > 0 {
        let prev = prev_char_boundary(buffer, pos);
        let ch = buffer[prev..pos].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        pos = prev;
    }
    while pos > 0 {
        let prev = prev_char_boundary(buffer, pos);
        let ch = buffer[prev..pos].chars().next().unwrap();
        if ch.is_whitespace() {
            break;
        }
        pos = prev;
    }
    pos
}

fn move_word_right(buffer: &str, cursor: usize) -> usize {
    let mut pos = cursor;
    while pos < buffer.len() {
        let next = next_char_boundary(buffer, pos);
        let ch = buffer[pos..next].chars().next().unwrap();
        if !ch.is_whitespace() {
            break;
        }
        pos = next;
    }
    while pos < buffer.len() {
        let next = next_char_boundary(buffer, pos);
        let ch = buffer[pos..next].chars().next().unwrap();
        pos = next;
        if ch.is_whitespace() {
            break;
        }
    }
    pos
}

fn line_wrap_metrics(line_chars: usize, content_width: usize, prefix_width: usize) -> (usize, usize) {
    let first_width = content_width.saturating_sub(prefix_width).max(1);
    let continuation_width = content_width.max(1);
    if line_chars <= first_width {
        return (1, first_width);
    }
    let remaining = line_chars - first_width;
    let continuation_lines = (remaining + continuation_width - 1) / continuation_width;
    (1 + continuation_lines, first_width)
}

fn wrapped_input_line_count(buffer: &str, content_width: usize, prefix_width: usize) -> usize {
    buffer.split('\n').map(|line| {
        let char_count = line.chars().count();
        line_wrap_metrics(char_count, content_width, prefix_width).0
    }).sum::<usize>().max(1)
}

fn segment_bounds_for_cursor(
    buffer: &str,
    cursor: usize,
    content_width: usize,
    prefix_width: usize,
) -> (usize, usize) {
    let cursor = cursor.min(buffer.len());
    let line_start = buffer[..cursor].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    let line_end = buffer[cursor..].find('\n').map(|idx| cursor + idx).unwrap_or(buffer.len());
    let line = &buffer[line_start..line_end];
    let total_chars = line.chars().count();
    let col_chars = buffer[line_start..cursor].chars().count();
    let (_, first_width) = line_wrap_metrics(total_chars, content_width, prefix_width);
    let continuation_width = content_width.max(1);

    let segment_start_chars = if col_chars <= first_width {
        0
    } else {
        let remainder = col_chars - first_width;
        first_width + ((remainder - 1) / continuation_width) * continuation_width
    };
    let segment_end_chars = if segment_start_chars == 0 {
        first_width.min(total_chars)
    } else {
        (segment_start_chars + continuation_width).min(total_chars)
    };

    let segment_start = if segment_start_chars == 0 {
        line_start
    } else {
        line_start + line.chars().take(segment_start_chars).map(|c| c.len_utf8()).sum::<usize>()
    };
    let segment_end = if segment_end_chars == 0 {
        line_start
    } else {
        line_start + line.chars().take(segment_end_chars).map(|c| c.len_utf8()).sum::<usize>()
    };
    (segment_start, segment_end)
}

fn move_to_segment_start(buffer: &str, cursor: usize, content_width: usize, prefix_width: usize) -> usize {
    let (segment_start, _) = segment_bounds_for_cursor(buffer, cursor, content_width, prefix_width);
    if cursor == segment_start && segment_start > 0 {
        let prev = prev_char_boundary(buffer, segment_start);
        let (prev_segment_start, _) = segment_bounds_for_cursor(buffer, prev, content_width, prefix_width);
        prev_segment_start
    } else {
        segment_start
    }
}

fn move_to_segment_end(buffer: &str, cursor: usize, content_width: usize, prefix_width: usize) -> usize {
    let (_, segment_end) = segment_bounds_for_cursor(buffer, cursor, content_width, prefix_width);
    if cursor == segment_end && segment_end < buffer.len() {
        let next = next_char_boundary(buffer, segment_end);
        let (_, next_segment_end) = segment_bounds_for_cursor(buffer, next, content_width, prefix_width);
        next_segment_end
    } else {
        segment_end
    }
}

/// Calculate (visual_line, visual_col, continuation_segment) from buffer byte position,
/// accounting for prefixed first segments and full-width wrapped continuations.
fn visual_cursor_position(
    buffer: &str,
    cursor: usize,
    content_width: usize,
    prefix_width: usize,
) -> (usize, usize, bool) {
    let cursor = cursor.min(buffer.len());
    let before = &buffer[..cursor];
    let last_newline = before.rfind('\n');
    let mut visual_line = 0usize;

    let preceding = match last_newline {
        Some(nl_pos) => &buffer[..nl_pos],
        None => "",
    };
    if !preceding.is_empty() {
        for hard_line in preceding.split('\n') {
            visual_line += line_wrap_metrics(hard_line.chars().count(), content_width, prefix_width).0;
        }
    }

    let current_line_start = last_newline.map(|idx| idx + 1).unwrap_or(0);
    let current_hard_line = &buffer[current_line_start..cursor];
    let col_in_hard_line = current_hard_line.chars().count();
    let continuation_width = content_width.max(1);
    let (_, first_width) = line_wrap_metrics(current_hard_line.chars().count(), content_width, prefix_width);

    if col_in_hard_line <= first_width {
        (visual_line, col_in_hard_line, false)
    } else {
        let remainder = col_in_hard_line - first_width;
        visual_line += 1 + (remainder / continuation_width);
        (visual_line, remainder % continuation_width, true)
    }
}

impl App {
    /// Poll gamepad events. Collects key codes to process, avoiding borrow conflicts.
    fn start_voice_recording(&mut self) {
        match crate::voice::VoiceCapture::start() {
            Ok(capture) => {
                self.voice_capture = Some(capture);
                self.voice_insert_start = Some(self.input_cursor);
                self.voice_last_transcribe = None;
                let mut state = self.state.lock().unwrap();
                state.recording = true;
            }
            Err(e) => {
                let mut state = self.state.lock().unwrap();
                state.status_message = Some(format!("Voice: {e}"));
            }
        }
    }

    fn stop_voice_recording(&mut self) {
        if let Some(capture) = self.voice_capture.take() {
            let audio = capture.stop();
            let mut state = self.state.lock().unwrap();
            state.recording = false;

            if audio.samples.len() >= 24_000 {
                let _ = self.event_tx.send(AppEvent::VoiceAudio(audio.samples));
            }
            self.voice_last_transcribe = None;
        }
    }

    fn poll_gamepad(&mut self) {
        let Some(gp) = &mut self.gilrs else { return };

        // Collect events into a local vec to avoid borrow issues
        let mut actions: Vec<KeyCode> = Vec::new();
        let mut start_pressed = false;
        let mut start_released = false;
        let mut select_pressed = false;
        let mut select_released = false;
        let mut west_pressed = false;

        while let Some(event) = gp.next_event() {
            match event.event {
                gilrs::EventType::ButtonPressed(gilrs::Button::Start, _) => start_pressed = true,
                gilrs::EventType::ButtonReleased(gilrs::Button::Start, _) => start_released = true,
                gilrs::EventType::ButtonPressed(gilrs::Button::Select, _) => select_pressed = true,
                gilrs::EventType::ButtonReleased(gilrs::Button::Select, _) => select_released = true,
                gilrs::EventType::ButtonPressed(gilrs::Button::DPadUp, _) => actions.push(KeyCode::Up),
                gilrs::EventType::ButtonPressed(gilrs::Button::DPadDown, _) => actions.push(KeyCode::Down),
                gilrs::EventType::ButtonPressed(gilrs::Button::South, _) => actions.push(KeyCode::Enter),
                gilrs::EventType::ButtonPressed(gilrs::Button::East, _) => actions.push(KeyCode::Esc),
                gilrs::EventType::ButtonPressed(gilrs::Button::West, _) => west_pressed = true,
                gilrs::EventType::ButtonPressed(gilrs::Button::LeftTrigger, _) => actions.push(KeyCode::PageUp),
                gilrs::EventType::ButtonPressed(gilrs::Button::RightTrigger, _) => actions.push(KeyCode::PageDown),
                _ => {}
            }
        }

        // Poll left thumbstick for navigation
        {
            let deadzone: f32 = 0.5;
            let repeat_ms: u128 = 200;
            let mut stick_y: f32 = 0.0;

            if let Some(gp_ref) = &self.gilrs {
                for (_id, gamepad) in gp_ref.gamepads() {
                    let y = gamepad.value(gilrs::Axis::LeftStickY);
                    if y.abs() > stick_y.abs() {
                        stick_y = y;
                    }
                }
            }

            // Convert to direction: positive = up, negative = down (may be inverted on some controllers)
            let dir: i8 = if stick_y > deadzone { 1 } else if stick_y < -deadzone { -1 } else { 0 };

            if dir != 0 {
                let should_fire = if dir != self.stick_prev_y {
                    // New direction — fire immediately
                    true
                } else {
                    // Same direction — throttle repeats
                    self.stick_last_action
                        .map(|t| t.elapsed().as_millis() >= repeat_ms)
                        .unwrap_or(true)
                };

                if should_fire {
                    if dir > 0 {
                        actions.push(KeyCode::Up);
                    } else {
                        actions.push(KeyCode::Down);
                    }
                    self.stick_last_action = Some(std::time::Instant::now());
                }
            } else {
                self.stick_last_action = None;
            }
            self.stick_prev_y = dir;
        }

        // Handle Start button hold for couch mode
        if start_pressed {
            self.start_held_since = Some(std::time::Instant::now());
        }
        if start_released {
            self.start_held_since = None;
        }

        if let Some(since) = self.start_held_since {
            if since.elapsed().as_millis() >= 2500 {
                self.start_held_since = None;
                let mut state = self.state.lock().unwrap();
                state.couch_mode = !state.couch_mode;
                state.couch_mode_notify = 30;
                if state.couch_mode {
                    state.push_output("\u{1F3AE} Couch mode on".to_string());
                } else {
                    state.push_output("\u{1F3AE} Couch mode off".to_string());
                }
            }
        }

        // Handle Select button for voice input.
        // - Hold: push-to-talk, release stops recording.
        // - Short tap (<300ms) + release: toggle mode, stays recording until next tap.
        const SHORT_TAP_MS: u128 = 300;

        if select_pressed {
            if self.voice_capture.is_some() && self.voice_toggled {
                // In toggle mode — tap stops recording
                self.stop_voice_recording();
            } else if self.voice_capture.is_none() {
                // Start recording, remember when we pressed
                self.voice_press_time = Some(std::time::Instant::now());
                self.voice_toggled = false;
                self.start_voice_recording();
            }
        }

        if select_released && !select_pressed && self.voice_capture.is_some() && !self.voice_toggled {
            // Released while recording in PTT mode
            let was_short_tap = self.voice_press_time
                .map(|t| t.elapsed().as_millis() < SHORT_TAP_MS)
                .unwrap_or(false);

            if was_short_tap {
                // Short tap — promote to toggle mode, keep recording
                self.voice_toggled = true;
            } else {
                // Long hold — PTT, stop recording
                self.stop_voice_recording();
            }
        }

        // Update VU meter + streaming transcription while recording
        if let Some(capture) = &self.voice_capture {
            let peak = capture.peak();
            let mut state = self.state.lock().unwrap();
            state.voice_meter.update(peak);

            let should_transcribe = self.voice_last_transcribe
                .map(|t| t.elapsed().as_secs() >= 2)
                .unwrap_or_else(|| capture.duration_samples() >= 48_000);
            if should_transcribe {
                let samples = capture.samples_snapshot();
                if samples.len() >= 24_000 {
                    let _ = self.event_tx.send(AppEvent::VoiceAudio(samples));
                    self.voice_last_transcribe = Some(std::time::Instant::now());
                }
            }
        }

        // Handle West (X) button: word-backspace in input, Space in survey
        if west_pressed {
            let state = self.state.lock().unwrap();
            let has_survey = state.pending_survey.is_some() || state.active_menu.is_some();
            drop(state);
            if has_survey {
                actions.push(KeyCode::Char(' '));
            } else if self.input_cursor > 0 {
                // Delete the word before cursor
                let buf = &self.input_buffer[..self.input_cursor];
                // Skip trailing whitespace, then skip the word
                let end = self.input_cursor;
                let trimmed = buf.trim_end();
                let word_start = trimmed.rfind(|c: char| c.is_whitespace())
                    .map(|i| i + 1)
                    .unwrap_or(0);
                // Find the byte position accounting for whitespace we skipped
                let delete_start = if trimmed.len() < buf.len() {
                    // There was trailing whitespace — delete from word_start
                    word_start
                } else {
                    word_start
                };
                self.input_buffer.replace_range(delete_start..end, "");
                self.input_cursor = delete_start;
            }
        }

        // Process collected actions
        for code in actions {
            if self.handle_job_browser_key(code) || self.handle_survey_key(code, KeyModifiers::NONE) || self.handle_menu_key(code) {
                continue;
            }

            match code {
                KeyCode::Enter => {
                    if !self.input_buffer.is_empty() {
                        let line = self.input_buffer.clone();
                        self.history.push(line.clone());
                        self.history_index = None;
                        self.input_buffer.clear();
                        self.input_cursor = 0;
                        let _ = self.event_tx.send(AppEvent::Submit(line));
                    }
                }
                KeyCode::Esc => {
                    let state = self.state.lock().unwrap();
                    if state.agent_active {
                        drop(state);
                        let _ = self.event_tx.send(AppEvent::Interrupt);
                    }
                }
                KeyCode::PageUp => {
                    let mut state = self.state.lock().unwrap();
                    let max_scroll = state.output.len().saturating_sub(1);
                    state.scroll_offset = (state.scroll_offset + 5).min(max_scroll);
                }
                KeyCode::PageDown => {
                    let mut state = self.state.lock().unwrap();
                    state.scroll_offset = state.scroll_offset.saturating_sub(5);
                }
                _ => {}
            }
        }
    }

    /// Handle a key for the job browser. Returns true if consumed.
    /// Handle slash commands that work in the TUI thread without the async loop.
    /// Returns true if handled locally (don't send to main loop).
    fn handle_local_slash(&mut self, line: &str) -> bool {
        match line.trim() {
            "/model" => {
                let mut s = self.state.lock().unwrap();
                let current_name = s.model_name.clone();
                let options: Vec<MenuOption> = crate::models::MODELS.iter().map(|m| {
                    MenuOption {
                        label: m.name.to_string(),
                        description: Some(m.provider.to_string()),
                        enabled: crate::models::is_available(m),
                    }
                }).collect();
                let picker_state = Arc::clone(&self.state);
                s.show_menu(
                    format!("Select model (currently: {current_name})"),
                    options,
                    move |selected| {
                        if let Some(idx) = selected {
                            if let Some(m) = crate::models::MODELS.get(idx) {
                                let mut s = picker_state.lock().unwrap();
                                s.selected_model_id = m.model_id.to_string();
                                s.model_name = m.name.to_string();
                                s.context_window = m.context_window;
                                s.show_model = true;
                                drop(s);
                                crate::config::save_model(m.model_id);
                            }
                        }
                    },
                );
                true
            }
            "/usage" => {
                let mut s = self.state.lock().unwrap();
                s.show_usage = !s.show_usage;
                true
            }
            "/rates" => {
                let mut s = self.state.lock().unwrap();
                s.show_rates = !s.show_rates;
                true
            }
            "/context" => {
                let mut s = self.state.lock().unwrap();
                s.show_context = !s.show_context;
                true
            }
            "/project" => {
                let mut s = self.state.lock().unwrap();
                s.show_project = !s.show_project;
                true
            }
            "/couch" => {
                let mut s = self.state.lock().unwrap();
                s.couch_mode = !s.couch_mode;
                s.couch_mode_notify = 30;
                let mode = if s.couch_mode { "on" } else { "off" };
                s.push_output(format!("\u{1F3AE} Couch mode {mode}"));
                true
            }
            _ => false, // Not a local command — send to main loop
        }
    }

    fn handle_job_browser_key(&mut self, code: KeyCode) -> bool {
        let mut state = self.state.lock().unwrap();

        // Extract browser state to avoid borrow conflicts
        let (mode, cursor) = match &state.job_browser {
            Some(b) => (b.mode.clone(), b.cursor),
            None => return false,
        };

        let job_count = state.jobs.len();
        if job_count == 0 {
            state.job_browser = None;
            return true;
        }

        match &mode {
            JobBrowserMode::List => {
                match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        let new_cursor = if cursor == 0 { job_count - 1 } else { cursor - 1 };
                        state.job_browser.as_mut().unwrap().cursor = new_cursor;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let new_cursor = (cursor + 1) % job_count;
                        state.job_browser.as_mut().unwrap().cursor = new_cursor;
                    }
                    KeyCode::Enter => {
                        // A / Enter: view output
                        if cursor < state.jobs.len() {
                            let job_id = state.jobs[cursor].id;
                            state.job_browser.as_mut().unwrap().mode = JobBrowserMode::ViewOutput(job_id);
                        }
                    }
                    KeyCode::Tab | KeyCode::Char('x') => {
                        // X / Tab: context menu
                        if cursor < state.jobs.len() {
                            let job_id = state.jobs[cursor].id;
                            state.job_browser.as_mut().unwrap().mode = JobBrowserMode::ContextMenu(job_id, 0);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('b') => {
                        // B / Esc: close browser
                        state.job_browser = None;
                    }
                    _ => {}
                }
            }
            JobBrowserMode::ViewOutput(job_id) => {
                match code {
                    KeyCode::Esc | KeyCode::Char('b') => {
                        // B / Esc: back to list
                        state.job_browser.as_mut().unwrap().mode = JobBrowserMode::List;
                    }
                    _ => {}
                }
            }
            JobBrowserMode::ContextMenu(job_id, menu_cursor) => {
                let job_id = *job_id;
                let menu_cursor = *menu_cursor;
                match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        let new_cursor = if menu_cursor == 0 { JOB_CONTEXT_MENU.len() - 1 } else { menu_cursor - 1 };
                        state.job_browser.as_mut().unwrap().mode = JobBrowserMode::ContextMenu(job_id, new_cursor);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        let new_cursor = (menu_cursor + 1) % JOB_CONTEXT_MENU.len();
                        state.job_browser.as_mut().unwrap().mode = JobBrowserMode::ContextMenu(job_id, new_cursor);
                    }
                    KeyCode::Enter => {
                        match menu_cursor {
                            0 => {
                                // Attach (interactive)
                                state.attached_process = Some(job_id);
                                state.job_browser = None;
                                state.push_output(format!("Attached to #{job_id}. Esc or Ctrl-D to detach."));
                            }
                            1 => {
                                // Stop — terminate the process
                                if let Some(handle) = state.process_handles.get(&job_id) {
                                    handle.terminate();
                                }
                                if let Some(job) = state.jobs.iter_mut().find(|j| j.id == job_id) {
                                    job.status = JobStatus::Failed;
                                }
                                state.process_writers.remove(&job_id);
                                state.process_handles.remove(&job_id);
                                state.push_output(format!("Stopped #{job_id}"));
                                state.job_browser.as_mut().unwrap().mode = JobBrowserMode::List;
                            }
                            2 => {
                                // Set Timeout — for now just show a message
                                state.push_output(format!("Set Timeout for #{job_id} (not yet implemented)"));
                                state.job_browser.as_mut().unwrap().mode = JobBrowserMode::List;
                            }
                            _ => {}
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('b') => {
                        // Back to list
                        state.job_browser.as_mut().unwrap().mode = JobBrowserMode::List;
                    }
                    _ => {}
                }
            }
        }
        true
    }

    /// Handle a key for the agent's survey. Returns true if consumed.
    fn handle_survey_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let mut state = self.state.lock().unwrap();
        let survey = match &mut state.pending_survey {
            Some(s) => s,
            None => return false,
        };

        let option_count = survey.options.len();

        // Ctrl+C cancels like Esc
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            let survey = state.pending_survey.take().unwrap();
            state.push_output(format!("  {}", survey.prompt));
            state.push_output("  (cancelled)".to_string());
            state.push_output(String::new());
            let _ = survey.response_tx.send(llm_code_sdk::tools::SurveyResponse { selected: vec![] });
            return true;
        }

        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                survey.cursor = if survey.cursor == 0 { option_count - 1 } else { survey.cursor - 1 };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                survey.cursor = (survey.cursor + 1) % option_count;
            }
            KeyCode::Char(' ') if survey.multi => {
                let c = survey.cursor;
                survey.selected[c] = !survey.selected[c];
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                if idx < option_count {
                    if survey.multi {
                        survey.selected[idx] = !survey.selected[idx];
                        survey.cursor = idx;
                    } else {
                        // Single-select: submit immediately
                        let survey = state.pending_survey.take().unwrap();
                        state.push_output(format!("  {}", survey.prompt));
                        state.push_output(format!("  \u{25b8} {}", survey.options[idx].label));
                        state.push_output(String::new());
                        let _ = survey.response_tx.send(llm_code_sdk::tools::SurveyResponse { selected: vec![idx] });
                        return true;
                    }
                }
            }
            KeyCode::Enter => {
                let selected: Vec<usize> = if survey.multi {
                    survey.selected.iter().enumerate()
                        .filter(|(_, s)| **s)
                        .map(|(i, _)| i)
                        .collect()
                } else {
                    vec![survey.cursor]
                };
                let survey = state.pending_survey.take().unwrap();
                let labels: Vec<&str> = selected.iter()
                    .filter_map(|&i| survey.options.get(i).map(|o| o.label.as_str()))
                    .collect();
                state.push_output(format!("  {}", survey.prompt));
                let answers = labels.iter().map(|l| format!("\u{25b8} {l}")).collect::<Vec<_>>().join("  ");
                state.push_output(format!("  {answers}"));
                state.push_output(String::new());
                let _ = survey.response_tx.send(llm_code_sdk::tools::SurveyResponse { selected });
                return true;
            }
            KeyCode::Esc => {
                let survey = state.pending_survey.take().unwrap();
                state.push_output(format!("  {}", survey.prompt));
                state.push_output("  (cancelled)".to_string());
                state.push_output(String::new());
                let _ = survey.response_tx.send(llm_code_sdk::tools::SurveyResponse { selected: vec![] });
                return true;
            }
            _ => {}
        }
        true
    }

    /// Handle a key for the interactive menu. Returns true if consumed.
    fn handle_menu_key(&mut self, code: KeyCode) -> bool {
        let mut state = self.state.lock().unwrap();
        let menu = match &mut state.active_menu {
            Some(m) => m,
            None => return false,
        };

        let count = menu.options.len();

        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                // Skip disabled options going up
                let mut next = if menu.cursor == 0 { count - 1 } else { menu.cursor - 1 };
                for _ in 0..count {
                    if menu.options[next].enabled { break; }
                    next = if next == 0 { count - 1 } else { next - 1 };
                }
                menu.cursor = next;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let mut next = (menu.cursor + 1) % count;
                for _ in 0..count {
                    if menu.options[next].enabled { break; }
                    next = (next + 1) % count;
                }
                menu.cursor = next;
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                if idx < count && menu.options[idx].enabled {
                    let menu = state.active_menu.take().unwrap();
                    drop(state);
                    (menu.on_select)(Some(idx));
                    return true;
                }
            }
            KeyCode::Enter => {
                let idx = menu.cursor;
                if menu.options[idx].enabled {
                    let menu = state.active_menu.take().unwrap();
                    drop(state);
                    (menu.on_select)(Some(idx));
                    return true;
                }
            }
            KeyCode::Esc => {
                let menu = state.active_menu.take().unwrap();
                drop(state);
                (menu.on_select)(None);
                return true;
            }
            _ => {}
        }
        true
    }

    /// Returns true if we should actually quit, false if this was the first attempt.
    fn try_quit(&mut self) -> bool {
        let now = std::time::Instant::now();
        if let Some(prev) = self.last_quit_attempt {
            if now.duration_since(prev).as_millis() < QUIT_WINDOW_MS {
                let _ = self.event_tx.send(AppEvent::Quit);
                return true;
            }
        }
        self.last_quit_attempt = Some(now);
        let mut state = self.state.lock().unwrap();
        state.status_message = Some("Ctrl-C again to quit".to_string());
        false
    }

    /// Clean up terminal on exit.
    pub fn cleanup() -> io::Result<()> {
        disable_raw_mode()?;
        crossterm::execute!(
            io::stdout(),
            crossterm::event::DisableBracketedPaste,
            crossterm::event::DisableMouseCapture,
            LeaveAlternateScreen,
        )?;
        Ok(())
    }
}
