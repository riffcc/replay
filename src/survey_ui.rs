//! Survey UI — Codex-style interactive selection widget.
//!
//! Input methods:
//! - Arrow keys / j/k + Enter (single select)
//! - Arrow keys / j/k + Space (toggle) + Enter (confirm multi-select)
//! - Number keys for quick selection
//! - Gamepad: D-Pad/LStick + A/Cross, B/Circle, Start

use std::io::{self, Write};

use crossterm::event::{Event, KeyCode, KeyEventKind};
use gilrs::{Button, EventType, Gilrs};
use llm_code_sdk::tools::{SurveyOption, SurveyRequest, SurveyResponse};

const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

enum Action {
    Up,
    Down,
    Select,
    Toggle,
    Confirm,
    Cancel,
    Quick(usize),
    None,
}

/// Run an interactive survey. Blocks until the user makes a selection.
pub fn run_survey(request: &SurveyRequest) -> SurveyResponse {
    let mut cursor: usize = 0;
    let mut selected: Vec<bool> = vec![false; request.options.len()];
    let option_count = request.options.len();
    let mut gilrs = Gilrs::new().ok();

    // Print the prompt
    println!();
    println!("  {CYAN}{}{RESET}", request.prompt);
    println!();

    crossterm::terminal::enable_raw_mode().ok();

    let lines = draw_options(&request.options, cursor, &selected, request.multi);
    draw_hint_bar(request.multi);

    // Total lines drawn = options + blank line + hint bar
    let total_lines = lines + 2;

    loop {
        let action = poll_input(&mut gilrs, request.multi);

        match action {
            Action::Up => {
                cursor = if cursor == 0 { option_count - 1 } else { cursor - 1 };
                redraw(total_lines, &request.options, cursor, &selected, request.multi);
            }
            Action::Down => {
                cursor = (cursor + 1) % option_count;
                redraw(total_lines, &request.options, cursor, &selected, request.multi);
            }
            Action::Quick(idx) => {
                if idx < option_count {
                    if request.multi {
                        selected[idx] = !selected[idx];
                        cursor = idx;
                        redraw(total_lines, &request.options, cursor, &selected, request.multi);
                    } else {
                        crossterm::terminal::disable_raw_mode().ok();
                        clear_lines(total_lines);
                        draw_final(&request.options, &[idx]);
                        return SurveyResponse { selected: vec![idx] };
                    }
                }
            }
            Action::Toggle => {
                if request.multi {
                    selected[cursor] = !selected[cursor];
                    redraw(total_lines, &request.options, cursor, &selected, request.multi);
                }
            }
            Action::Select if !request.multi => {
                crossterm::terminal::disable_raw_mode().ok();
                clear_lines(total_lines);
                draw_final(&request.options, &[cursor]);
                return SurveyResponse { selected: vec![cursor] };
            }
            Action::Select | Action::Confirm => {
                crossterm::terminal::disable_raw_mode().ok();
                let result: Vec<usize> = if request.multi {
                    selected.iter().enumerate().filter(|(_, s)| **s).map(|(i, _)| i).collect()
                } else {
                    vec![cursor]
                };
                clear_lines(total_lines);
                draw_final(&request.options, &result);
                return SurveyResponse { selected: result };
            }
            Action::Cancel => {
                crossterm::terminal::disable_raw_mode().ok();
                clear_lines(total_lines);
                println!("  {DIM}(cancelled){RESET}");
                return SurveyResponse { selected: vec![] };
            }
            Action::None => {}
        }
    }
}

fn poll_input(gilrs: &mut Option<Gilrs>, multi: bool) -> Action {
    // Gamepad
    if let Some(gp) = gilrs.as_mut() {
        while let Some(event) = gp.next_event() {
            if let EventType::ButtonPressed(button, _) = event.event {
                return match button {
                    Button::DPadUp => Action::Up,
                    Button::DPadDown => Action::Down,
                    Button::South => if multi { Action::Toggle } else { Action::Select },
                    Button::East => Action::Cancel,
                    Button::Start => Action::Confirm,
                    _ => Action::None,
                };
            }
        }
        for (_id, gamepad) in gp.gamepads() {
            let y = gamepad.value(gilrs::Axis::LeftStickY);
            if y > 0.5 { return Action::Up; }
            if y < -0.5 { return Action::Down; }
        }
    }

    // Keyboard
    if crossterm::event::poll(std::time::Duration::from_millis(30)).unwrap_or(false) {
        if let Ok(Event::Key(key)) = crossterm::event::read() {
            if key.kind != KeyEventKind::Press { return Action::None; }
            return match key.code {
                KeyCode::Up | KeyCode::Char('k') => Action::Up,
                KeyCode::Down | KeyCode::Char('j') => Action::Down,
                KeyCode::Enter => Action::Select,
                KeyCode::Char(' ') => Action::Toggle,
                KeyCode::Esc => Action::Cancel,
                KeyCode::Char(c @ '1'..='9') => Action::Quick((c as usize) - ('1' as usize)),
                _ => Action::None,
            };
        }
    }
    Action::None
}

/// Draw options in Codex style. Returns number of lines drawn.
fn draw_options(options: &[SurveyOption], cursor: usize, selected: &[bool], multi: bool) -> usize {
    let mut out = io::stdout();

    // Find max label width for alignment
    let max_label = options.iter().map(|o| o.label.len()).max().unwrap_or(0);

    for (i, opt) in options.iter().enumerate() {
        let at_cursor = i == cursor;
        let is_selected = selected[i];
        let num = i + 1;

        // Cursor indicator
        let arrow = if at_cursor { "\u{203a}" } else { " " };

        // Checkbox / radio
        let check = if multi {
            if is_selected { format!("[{GREEN}x{RESET}]") } else { "[ ]".to_string() }
        } else {
            String::new()
        };

        // Number + label
        let label_style = if at_cursor { CYAN } else { RESET };
        let desc_style = if at_cursor { CYAN } else { DIM };

        // Pad label for description alignment
        let padding = max_label.saturating_sub(opt.label.len());
        let pad = " ".repeat(padding);

        let desc = opt.description.as_deref().unwrap_or("");

        if multi {
            write!(out, "  {arrow} {check} {label_style}{num}. {}{RESET}{pad}  {desc_style}{desc}{RESET}\r\n",
                opt.label).ok();
        } else {
            write!(out, "  {arrow} {label_style}{num}. {}{RESET}{pad}  {desc_style}{desc}{RESET}\r\n",
                opt.label).ok();
        }
    }
    out.flush().ok();
    options.len()
}

/// Draw the hint bar.
fn draw_hint_bar(multi: bool) {
    let mut out = io::stdout();
    write!(out, "\r\n").ok();
    if multi {
        write!(out, "  {DIM}space{RESET} to select | {BOLD}enter{RESET} to submit | {DIM}esc to cancel{RESET}\r\n").ok();
    } else {
        write!(out, "  {BOLD}enter{RESET} to submit | {DIM}esc to cancel{RESET}\r\n").ok();
    }
    out.flush().ok();
}

/// Clear N lines and redraw.
fn redraw(total_lines: usize, options: &[SurveyOption], cursor: usize, selected: &[bool], multi: bool) {
    clear_lines(total_lines);
    draw_options(options, cursor, selected, multi);
    draw_hint_bar(multi);
}

/// Clear lines by moving cursor up and erasing.
fn clear_lines(count: usize) {
    let mut out = io::stdout();
    for _ in 0..count {
        write!(out, "\x1b[A\x1b[2K").ok();
    }
    out.flush().ok();
}

/// Draw the final compact selection summary.
fn draw_final(options: &[SurveyOption], selected: &[usize]) {
    let labels: Vec<&str> = selected
        .iter()
        .filter_map(|&i| options.get(i).map(|o| o.label.as_str()))
        .collect();
    println!("  {GREEN}\u{203a}{RESET} {}", labels.join(", "));
}
