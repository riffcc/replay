//! Reusable menu system for Ratatui.
//!
//! A menu is a list of labeled choices. One is always highlighted (cursor).
//! Items can be individually checked (toggled) with Select.

use std::collections::HashSet;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};

use super::input::InputEvent;

/// A single menu item.
pub struct MenuItem {
    pub label: String,
    /// If set, this marker is shown instead of the default ●/○.
    pub marker: Option<String>,
    /// Whether this item supports being checked/toggled.
    pub checkable: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            marker: None,
            checkable: false,
        }
    }

    pub fn with_marker(label: impl Into<String>, marker: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            marker: Some(marker.into()),
            checkable: true,
        }
    }
}

/// A navigable menu with optional multi-select.
pub struct Menu {
    items: Vec<MenuItem>,
    cursor: usize,
    checked: HashSet<usize>,
}

/// What the caller should do after handling input.
pub enum MenuAction {
    /// User toggled an item. Returns its index and new checked state.
    Toggled(usize, bool),
    /// User confirmed on a non-checkable item.
    Confirmed(usize),
    /// Menu consumed the input (navigation). Keep looping.
    Consumed,
    /// Menu didn't care about this input. Let the caller handle it.
    Ignored,
}

impl Menu {
    pub fn new(items: Vec<MenuItem>) -> Self {
        Self {
            items,
            cursor: 0,
            checked: HashSet::new(),
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    pub fn is_checked(&self, index: usize) -> bool {
        self.checked.contains(&index)
    }

    pub fn checked_indices(&self) -> &HashSet<usize> {
        &self.checked
    }

    /// Handle an input event.
    pub fn handle(&mut self, event: InputEvent) -> MenuAction {
        match event {
            InputEvent::Up => {
                if self.cursor == 0 {
                    self.cursor = self.items.len().saturating_sub(1);
                } else {
                    self.cursor -= 1;
                }
                MenuAction::Consumed
            }
            InputEvent::Down => {
                if self.cursor >= self.items.len().saturating_sub(1) {
                    self.cursor = 0;
                } else {
                    self.cursor += 1;
                }
                MenuAction::Consumed
            }
            InputEvent::Select => {
                if self.cursor < self.items.len() && self.items[self.cursor].checkable {
                    let was_checked = self.checked.contains(&self.cursor);
                    if was_checked {
                        self.checked.remove(&self.cursor);
                    } else {
                        self.checked.insert(self.cursor);
                    }
                    MenuAction::Toggled(self.cursor, !was_checked)
                } else {
                    MenuAction::Confirmed(self.cursor)
                }
            }
            _ => MenuAction::Ignored,
        }
    }

    /// Render the menu into the given area.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        for (i, item) in self.items.iter().enumerate() {
            let at_cursor = i == self.cursor;
            let checked = self.checked.contains(&i);

            let marker = match &item.marker {
                Some(m) => m.as_str(),
                None => if at_cursor { "●" } else { "○" },
            };

            // Show check state: ▸ prefix for checked items
            let check = if checked { "▸ " } else { "  " };

            let style = if at_cursor {
                Style::default().fg(Color::Yellow).bold()
            } else if checked {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };

            lines.push(Line::from(Span::styled(
                format!("{check}{marker} {}", item.label),
                style,
            )));

            if i < self.items.len() - 1 {
                lines.push(Line::from(""));
            }
        }

        let widget = Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(Block::default());
        frame.render_widget(widget, area);
    }
}
