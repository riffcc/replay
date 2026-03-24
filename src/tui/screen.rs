//! Screen system — each screen owns its menu and rendering.

use ratatui::prelude::*;

use super::input::InputEvent;
use super::menu::{Menu, MenuAction, MenuItem};

/// Which screen we're on.
pub enum Screen {
    /// First launch, no project configured.
    Welcome(Menu),
    /// Project exists, can continue.
    Home(Menu),
    /// Inside a session, no issues yet.
    SessionEmpty(Menu),
}

/// What the app should do after a screen handles input.
pub enum Transition {
    /// Stay on this screen.
    Stay,
    /// Switch to a different screen.
    Goto(Screen),
    /// Quit the application.
    Quit,
}

impl Screen {
    /// Create the welcome screen (no project configured).
    pub fn welcome() -> Self {
        Screen::Welcome(Menu::new(vec![
            MenuItem::new("Start new session"),
            MenuItem::new("Settings"),
            MenuItem::new("Quit"),
        ]))
    }

    /// Create the home screen (project exists).
    pub fn home() -> Self {
        Screen::Home(Menu::new(vec![
            MenuItem::new("Continue project"),
            MenuItem::new("Start new session"),
            MenuItem::new("Settings"),
            MenuItem::new("Quit"),
        ]))
    }

    /// Create the empty session screen.
    pub fn session_empty() -> Self {
        Screen::SessionEmpty(Menu::new(vec![
            MenuItem::new("Find next issues to work on"),
        ]))
    }

    /// Handle input. Returns what the app should do next.
    pub fn handle(&mut self, event: InputEvent) -> Transition {
        match self {
            Screen::Welcome(menu) => match menu.handle(event) {
                MenuAction::Confirmed(0) => Transition::Goto(Screen::session_empty()),
                MenuAction::Confirmed(2) => Transition::Quit,
                _ => Transition::Stay,
            },
            Screen::Home(menu) => match menu.handle(event) {
                MenuAction::Confirmed(0) => Transition::Goto(Screen::session_empty()),
                MenuAction::Confirmed(1) => Transition::Goto(Screen::session_empty()),
                MenuAction::Confirmed(3) => Transition::Quit,
                _ => Transition::Stay,
            },
            Screen::SessionEmpty(menu) => {
                if event == InputEvent::Back {
                    return Transition::Goto(Screen::home());
                }
                match menu.handle(event) {
                    MenuAction::Confirmed(0) => {
                        // This is where we'll generate suggestions
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

        // Left third of the screen
        let h_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(33),
                Constraint::Percentage(67),
            ])
            .split(area);

        let left = h_layout[0];

        // Vertically center the content
        let menu_height = match self {
            Screen::Welcome(m) => menu_render_height(3),
            Screen::Home(m) => menu_render_height(4),
            Screen::SessionEmpty(m) => menu_render_height(1),
        };
        // title + blank line + menu
        let content_height = 1 + 1 + menu_height;
        let v_pad = left.height.saturating_sub(content_height) / 2;

        let v_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(v_pad),
                Constraint::Length(1),  // title
                Constraint::Length(1),  // blank
                Constraint::Length(menu_height),
                Constraint::Min(0),
            ])
            .split(left);

        let title = match self {
            Screen::Welcome(_) => "Welcome to Replay.",
            Screen::Home(_) => "Welcome back.",
            Screen::SessionEmpty(_) => "Session",
        };

        // Pad from left edge
        let title_area = pad_left(v_layout[1], 7);
        let menu_area = pad_left(v_layout[3], 7);

        let title_widget = ratatui::widgets::Paragraph::new(title)
            .style(Style::default().fg(Color::White).bold());
        frame.render_widget(title_widget, title_area);

        match self {
            Screen::Welcome(menu) => menu.render(frame, menu_area),
            Screen::Home(menu) => menu.render(frame, menu_area),
            Screen::SessionEmpty(menu) => menu.render(frame, menu_area),
        }
    }
}

/// How many rows a menu with n items needs (items + blank lines between).
fn menu_render_height(item_count: u16) -> u16 {
    if item_count == 0 { 0 } else { item_count * 2 - 1 }
}

/// Shrink a rect by padding from the left.
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
