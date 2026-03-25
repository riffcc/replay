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

/// A line in the output log. Stored pre-wrapped to terminal width.
#[derive(Clone)]
pub struct OutputLine {
    pub content: String,
}

/// Wrap a string to a given width, returning individual lines.
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
        // Simple char-based wrapping (ANSI-unaware for now)
        let chars: Vec<char> = raw_line.chars().collect();
        for chunk in chars.chunks(width.max(1)) {
            lines.push(chunk.iter().collect());
        }
    }
    lines
}

/// State shared between the TUI and the agent.
pub struct AppState {
    /// Raw output strings (not wrapped).
    raw_output: Vec<String>,
    /// Pre-wrapped output lines for display.
    pub output: Vec<OutputLine>,
    /// Current terminal width for wrapping.
    pub term_width: usize,
    /// Scroll offset from bottom (0 = at bottom).
    pub scroll_offset: usize,
    /// Whether the agent is currently working.
    pub agent_active: bool,
    /// Transient status message shown in the input border.
    pub status_message: Option<String>,
    /// Throbber frame for animation.
    pub throbber_frame: u8,
    /// Throbber state: 0=idle, 1=thinking, 2=tool
    pub throbber_state: u8,
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
            status_message: None,
            throbber_frame: 0,
            throbber_state: 0,
        }
    }

    pub fn push_output(&mut self, content: String) {
        self.raw_output.push(content.clone());
        for line in wrap_text(&content, self.term_width) {
            self.output.push(OutputLine { content: line });
        }
        self.scroll_offset = 0;
    }

    /// Re-wrap all output for a new terminal width.
    pub fn rewrap(&mut self, new_width: usize) {
        self.term_width = new_width;
        self.output.clear();
        for raw in &self.raw_output {
            for line in wrap_text(raw, new_width) {
                self.output.push(OutputLine { content: line });
            }
        }
        self.scroll_offset = 0;
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
}

/// The TUI application.
pub struct App {
    state: Arc<Mutex<AppState>>,
    input_buffer: String,
    input_cursor: usize,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    last_enter: Option<std::time::Instant>,
    last_quit_attempt: Option<std::time::Instant>,
}

const DOUBLE_ENTER_MS: u64 = 300;

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
        }
    }

    /// Run the TUI event loop. Blocks until quit.
    pub fn run(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

        loop {
            // Tick throbber
            {
                let mut state = self.state.lock().unwrap();
                if state.agent_active {
                    state.throbber_frame = (state.throbber_frame + 1) % 8;
                }
            }

            terminal.draw(|frame| self.render(frame))?;

            // Poll with short timeout for animation
            if event::poll(Duration::from_millis(80))? {
                let ev = event::read()?;

                if let Event::Resize(w, _) = ev {
                    let mut state = self.state.lock().unwrap();
                    state.rewrap(w as usize);
                    continue;
                }

                let key = match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => key,
                    _ => continue,
                };


                    match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                            // Shift-enter: insert newline
                            self.input_buffer.insert(self.input_cursor, '\n');
                            self.input_cursor += 1;
                        }
                        KeyCode::Enter => {
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
                                    self.input_buffer.clear();
                                    self.input_cursor = 0;
                                    let _ = self.event_tx.send(AppEvent::Submit(line));
                                }
                            }
                        }
                        KeyCode::Char(c) => {
                            self.input_buffer.insert(self.input_cursor, c);
                            self.input_cursor += 1;
                            // Clear status on any typing
                            self.last_quit_attempt = None;
                            let mut s = self.state.lock().unwrap();
                            s.status_message = None;
                        }
                        KeyCode::Backspace => {
                            if self.input_cursor > 0 {
                                self.input_cursor -= 1;
                                self.input_buffer.remove(self.input_cursor);
                            }
                        }
                        KeyCode::Left => {
                            if self.input_cursor > 0 {
                                self.input_cursor -= 1;
                            }
                        }
                        KeyCode::Right => {
                            if self.input_cursor < self.input_buffer.len() {
                                self.input_cursor += 1;
                            }
                        }
                        KeyCode::Home => {
                            self.input_cursor = 0;
                        }
                        KeyCode::End => {
                            self.input_cursor = self.input_buffer.len();
                        }
                        KeyCode::Up => {
                            let mut state = self.state.lock().unwrap();
                            let max_scroll = state.output.len().saturating_sub(1);
                            state.scroll_offset = (state.scroll_offset + 1).min(max_scroll);
                        }
                        KeyCode::Down => {
                            let mut state = self.state.lock().unwrap();
                            state.scroll_offset = state.scroll_offset.saturating_sub(1);
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

        // Input area height: 1 line per input line + 2 for borders
        let input_line_count = self.input_buffer.matches('\n').count() + 1;
        let input_height = (input_line_count as u16 + 2).max(3);

        // Layout: output takes all space, input at bottom
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(input_height),
            ])
            .split(frame.area());

        // ── Output pane ──
        let output_area = chunks[0];
        let visible_height = output_area.height as usize;
        let total_lines = state.output.len();

        let all_lines: Vec<Line> = state.output
            .iter()
            .map(|line| Line::raw(&line.content))
            .collect();

        // Scroll position: 0 = at bottom (most recent), so convert to top-based offset
        let max_scroll = total_lines.saturating_sub(visible_height);
        let clamped_offset = state.scroll_offset.min(max_scroll);
        let scroll_top = max_scroll.saturating_sub(clamped_offset);

        let output_widget = Paragraph::new(all_lines)
            .scroll((scroll_top as u16, 0));
        frame.render_widget(output_widget, output_area);

        // ── Input pane ──
        let input_area = chunks[1];

        // Throbber in the top border
        let border_title = if state.agent_active {
            let frame_idx = state.throbber_frame as usize % THROBBER_FRAMES.len();
            let chars = &THROBBER_FRAMES[frame_idx];
            Span::styled(
                format!(" {} working ", chars.join("")),
                Style::default().fg(Color::Yellow),
            )
        } else if let Some(msg) = &state.status_message {
            Span::styled(
                format!(" {msg} "),
                Style::default().fg(Color::DarkGray),
            )
        } else {
            Span::raw("")
        };

        let input_block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_type(ratatui::widgets::BorderType::Plain)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(border_title);

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
            .block(input_block);
        frame.render_widget(input_text, input_area);

        // Cursor position — account for › prefix and newlines
        let (cursor_line, cursor_col) = cursor_position(&self.input_buffer, self.input_cursor);
        let prefix_width: u16 = if cursor_line == 0 { 2 } else { 2 }; // "› " or "  "
        let cursor_x = input_area.x + prefix_width + cursor_col as u16;
        let cursor_y = input_area.y + 1 + cursor_line as u16; // +1 for top border
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
    }

}

const QUIT_WINDOW_MS: u128 = 2000;

/// Calculate (line, col) from buffer position.
fn cursor_position(buffer: &str, cursor: usize) -> (usize, usize) {
    let before = &buffer[..cursor.min(buffer.len())];
    let line = before.matches('\n').count();
    let col = before.rfind('\n').map(|p| cursor - p - 1).unwrap_or(cursor);
    (line, col)
}

impl App {
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
        crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;
        Ok(())
    }
}
