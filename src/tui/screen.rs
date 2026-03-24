//! Screen system — each screen owns its menu and rendering.

use std::path::PathBuf;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::input::InputEvent;
use super::menu::{Menu, MenuAction, MenuItem};
use crate::beads;

/// Which screen we're on.
pub enum Screen {
    /// Main menu — beads not yet loaded.
    Welcome(Menu),
    /// Active session — viewing issues.
    Session(SessionState),
}

pub struct SessionState {
    pub target: PathBuf,
    pub issues: Vec<beads::Issue>,
    pub menu: Menu,
    pub status: String,
}

/// What the app should do after a screen handles input.
pub enum Transition {
    Stay,
    Goto(Screen),
    Quit,
}

impl Screen {
    /// Detect state and build the right initial screen.
    pub fn initial() -> Self {
        let cwd = std::env::current_dir().expect("failed to get cwd");
        if beads::is_initialised(&cwd) {
            Screen::start_session(cwd)
        } else {
            Screen::Welcome(Menu::new(vec![
                MenuItem::new("Initialise project"),
                MenuItem::new("Settings"),
                MenuItem::new("Quit"),
            ]))
        }
    }

    /// Load (or reload) a session from a target directory.
    fn start_session(target: PathBuf) -> Self {
        if !beads::is_initialised(&target) {
            if let Err(e) = beads::init(&target) {
                return Screen::Welcome(Menu::new(vec![
                    MenuItem::new(format!("Error: {e}")),
                    MenuItem::new("Quit"),
                ]));
            }
        }

        let (issues, status) = match beads::list_all(&target) {
            Ok(issues) => {
                let count = issues.len();
                (issues, format!("{count} issues"))
            }
            Err(e) => (Vec::new(), format!("error: {e}")),
        };

        let menu_items = if issues.is_empty() {
            vec![MenuItem::new("Find next issues to work on")]
        } else {
            let mut items: Vec<MenuItem> = issues
                .iter()
                .map(|issue| {
                    MenuItem::with_marker(
                        &issue.title,
                        status_icon(&issue.status),
                    )
                })
                .collect();
            items.push(MenuItem::new("Find next issues to work on"));
            items
        };

        Screen::Session(SessionState {
            target,
            issues,
            menu: Menu::new(menu_items),
            status,
        })
    }

    /// Handle input.
    pub fn handle(&mut self, event: InputEvent) -> Transition {
        match self {
            Screen::Welcome(menu) => match menu.handle(event) {
                MenuAction::Confirmed(0) => {
                    let cwd = std::env::current_dir().expect("failed to get cwd");
                    Transition::Goto(Screen::start_session(cwd))
                }
                MenuAction::Confirmed(i) if is_quit_index(i, menu) => Transition::Quit,
                _ => Transition::Stay,
            },
            Screen::Session(state) => {
                if event == InputEvent::Back {
                    return Transition::Goto(Screen::initial());
                }
                match state.menu.handle(event) {
                    MenuAction::Confirmed(_) => {
                        // TODO: solve selected issue or generate suggestions
                        Transition::Stay
                    }
                    _ => Transition::Stay,
                }
            }
        }
    }

    /// Render the screen.
    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();

        // Reserve bottom row for legend
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);

        let legend = Paragraph::new(
            "◌ Not started │ ◐ In progress │ ❄ Deferred │ ● Blocked │ ◍ Tests written │ ✪ Passing tests │ ⦿ Accepted │ ✔ Completed"
        ).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(legend, pad_left(outer[1], 3));

        let h_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(33),
                Constraint::Percentage(67),
            ])
            .split(outer[0]);

        let left = h_layout[0];

        let (title, menu, subtitle) = match self {
            Screen::Welcome(menu) => ("Welcome to Replay.", menu, None),
            Screen::Session(state) => ("Session", &state.menu, Some(state.status.as_str())),
        };

        let subtitle_height: u16 = if subtitle.is_some() { 2 } else { 0 };
        // title + subtitle + gap + generous menu estimate
        let menu_item_count = menu.item_count() as u16;
        let menu_height = menu_render_height(menu_item_count);
        let content_height = 1 + subtitle_height + 1 + menu_height;
        let v_pad = left.height.saturating_sub(content_height) / 2;

        let v_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(v_pad),
                Constraint::Length(1),               // title
                Constraint::Length(subtitle_height),  // subtitle
                Constraint::Length(1),               // gap
                Constraint::Min(0),                  // menu gets all remaining space
            ])
            .split(left);

        let pad = 7;
        let title_area = pad_left(v_layout[1], pad);
        let subtitle_area = pad_left(v_layout[2], pad);
        let menu_area = pad_left(v_layout[4], pad);  // index 4 = Min(0)

        let title_widget = Paragraph::new(title)
            .style(Style::default().fg(Color::White).bold());
        frame.render_widget(title_widget, title_area);

        if let Some(sub) = subtitle {
            let sub_widget = Paragraph::new(sub)
                .style(Style::default().fg(Color::Gray));
            frame.render_widget(sub_widget, subtitle_area);
        }

        menu.render(frame, menu_area);
    }
}

fn status_icon(status: &str) -> &'static str {
    match status {
        "open" => "◌",
        "in_progress" => "◐",
        "deferred" => "❄",
        "blocked" => "●",
        "tests_written" => "◍",
        "passing_tests" => "✪",
        "accepted" => "⦿",
        "closed" => "✔",
        _ => "?",
    }
}

fn is_quit_index(i: usize, menu: &Menu) -> bool {
    i == menu.item_count() - 1
}

fn menu_render_height(item_count: u16) -> u16 {
    if item_count == 0 { 0 } else { item_count * 2 - 1 }
}

fn pad_left(area: Rect, pad: u16) -> Rect {
    if pad >= area.width {
        return area;
    }
    Rect {
        x: area.x + pad,
        width: area.width - pad,
        ..area
    }
}
