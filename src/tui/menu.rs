//! Reusable menu system for Ratatui.
//!
//! A menu is a list of labeled choices. One is always selected.
//! Navigate with Up/Down, confirm with Select, cancel with Back.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};

use super::input::InputEvent;

/// A single menu item.
pub struct MenuItem {
    pub label: String,
}

impl MenuItem {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into() }
    }
}

/// A navigable menu.
pub struct Menu {
    items: Vec<MenuItem>,
    selected: usize,
}

/// What the caller should do after handling input.
pub enum MenuAction {
    /// User confirmed the selected item. Returns its index.
    Confirmed(usize),
    /// Menu consumed the input (navigation). Keep looping.
    Consumed,
    /// Menu didn't care about this input. Let the caller handle it.
    Ignored,
}

impl Menu {
    pub fn new(items: Vec<MenuItem>) -> Self {
        Self { items, selected: 0 }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Handle an input event. Returns what the caller should do.
    pub fn handle(&mut self, event: InputEvent) -> MenuAction {
        match event {
            InputEvent::Up => {
                if self.selected == 0 {
                    self.selected = self.items.len().saturating_sub(1);
                } else {
                    self.selected -= 1;
                }
                MenuAction::Consumed
            }
            InputEvent::Down => {
                if self.selected >= self.items.len().saturating_sub(1) {
                    self.selected = 0;
                } else {
                    self.selected += 1;
                }
                MenuAction::Consumed
            }
            InputEvent::Select => MenuAction::Confirmed(self.selected),
            _ => MenuAction::Ignored,
        }
    }

    /// Render the menu into the given area.
    /// Each item gets its own line with a blank line between items.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        for (i, item) in self.items.iter().enumerate() {
            let marker = if i == self.selected { "●" } else { "○" };
            let style = if i == self.selected {
                Style::default().fg(Color::White).bold()
            } else {
                Style::default().fg(Color::DarkGray)
            };

            lines.push(Line::from(Span::styled(
                format!("{marker} {}", item.label),
                style,
            )));

            // Blank line between items (not after the last one)
            if i < self.items.len() - 1 {
                lines.push(Line::from(""));
            }
        }

        let widget = Paragraph::new(lines).block(Block::default());
        frame.render_widget(widget, area);
    }
}
