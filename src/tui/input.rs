//! Unified input handling for keyboard and gamepad.
//!
//! This module abstracts over both keyboard (crossterm) and gamepad (gilrs) events
//! into a unified input type, enabling couch-mode operation with a controller.

use anyhow::Result;
use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEventKind};
use gilrs::Gilrs;

/// Unified input events combining keyboard and gamepad input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputEvent {
    /// Quit the application.
    Quit,
    /// Navigate up.
    Up,
    /// Navigate down.
    Down,
    /// Navigate left.
    Left,
    /// Navigate right.
    Right,
    /// Select/confirm action.
    Select,
    /// Back/cancel action.
    Back,
    /// Open menu.
    Menu,
    /// No input event occurred.
    None,
}

/// Input handler that manages both keyboard and gamepad input.
pub struct InputManager {
    gilrs: Option<Gilrs>,
}

impl InputManager {
    /// Create a new InputManager, initializing gamepad support if available.
    pub fn new() -> Result<Self> {
        let gilrs = Gilrs::new().ok();

        Ok(Self { gilrs })
    }

    /// Poll for input events without blocking.
    /// Returns the most relevant input event, or None if no input is available.
    pub fn poll(&mut self) -> Result<Option<InputEvent>> {
        // First check for gamepad events (non-blocking)
        if let Some(gp_event) = self.poll_gamepad()? {
            return Ok(Some(gp_event));
        }

        // Then check for keyboard events (non-blocking)
        if let Some(kb_event) = self.poll_keyboard()? {
            return Ok(Some(kb_event));
        }

        Ok(None)
    }

    /// Poll for gamepad events without blocking.
    fn poll_gamepad(&mut self) -> Result<Option<InputEvent>> {
        let Some(gilrs) = &mut self.gilrs else {
            return Ok(None);
        };

        // Process any pending gamepad events
        while let Some(event) = gilrs.next_event() {
            // The gilrs Event struct has an `event` field containing the EventType
            let event_type = event.event;
            let input = match event_type {
                // D-pad navigation
                gilrs::EventType::ButtonPressed(
                    button @ (gilrs::Button::DPadUp
                    | gilrs::Button::DPadDown
                    | gilrs::Button::DPadLeft
                    | gilrs::Button::DPadRight),
                    _,
                ) => {
                    match button {
                        gilrs::Button::DPadUp => InputEvent::Up,
                        gilrs::Button::DPadDown => InputEvent::Down,
                        gilrs::Button::DPadLeft => InputEvent::Left,
                        gilrs::Button::DPadRight => InputEvent::Right,
                        _ => unreachable!(),
                    }
                }
                // A/Cross button for select
                gilrs::EventType::ButtonPressed(gilrs::Button::South, _) => InputEvent::Select,
                // B/Circle button for back
                gilrs::EventType::ButtonPressed(gilrs::Button::East, _) => InputEvent::Back,
                // Start button for menu
                gilrs::EventType::ButtonPressed(gilrs::Button::Start, _) => InputEvent::Menu,
                _ => continue,
            };

            return Ok(Some(input));
        }

        Ok(None)
    }

    /// Poll for keyboard events without blocking.
    fn poll_keyboard(&mut self) -> Result<Option<InputEvent>> {
        // Poll using crossterm's poll function for non-blocking behavior
        if crossterm::event::poll(std::time::Duration::from_secs(0))? {
            if let Ok(CrosstermEvent::Key(key)) = crossterm::event::read() {
                if key.kind == KeyEventKind::Press {
                    return Ok(Some(key_to_input(key.code)));
                }
            }
        }

        Ok(None)
    }
}

impl Default for InputManager {
    fn default() -> Self {
        Self::new().expect("failed to initialize input manager")
    }
}

/// Convert crossterm KeyCode to InputEvent.
fn key_to_input(code: KeyCode) -> InputEvent {
    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') => InputEvent::Quit,
        KeyCode::Up | KeyCode::Char('k') => InputEvent::Up,
        KeyCode::Down | KeyCode::Char('j') => InputEvent::Down,
        KeyCode::Left | KeyCode::Char('h') => InputEvent::Left,
        KeyCode::Right | KeyCode::Char('l') => InputEvent::Right,
        KeyCode::Enter | KeyCode::Char(' ') => InputEvent::Select,
        KeyCode::Esc | KeyCode::Backspace => InputEvent::Back,
        KeyCode::Char('m') => InputEvent::Menu,
        _ => InputEvent::None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_to_input() {
        // Quit
        assert_eq!(key_to_input(KeyCode::Char('q')), InputEvent::Quit);
        assert_eq!(key_to_input(KeyCode::Char('Q')), InputEvent::Quit);

        // Navigation
        assert_eq!(key_to_input(KeyCode::Up), InputEvent::Up);
        assert_eq!(key_to_input(KeyCode::Down), InputEvent::Down);
        assert_eq!(key_to_input(KeyCode::Left), InputEvent::Left);
        assert_eq!(key_to_input(KeyCode::Right), InputEvent::Right);

        // Vim-style navigation
        assert_eq!(key_to_input(KeyCode::Char('k')), InputEvent::Up);
        assert_eq!(key_to_input(KeyCode::Char('j')), InputEvent::Down);
        assert_eq!(key_to_input(KeyCode::Char('h')), InputEvent::Left);
        assert_eq!(key_to_input(KeyCode::Char('l')), InputEvent::Right);

        // Select
        assert_eq!(key_to_input(KeyCode::Enter), InputEvent::Select);
        assert_eq!(key_to_input(KeyCode::Char(' ')), InputEvent::Select);

        // Back
        assert_eq!(key_to_input(KeyCode::Esc), InputEvent::Back);
        assert_eq!(key_to_input(KeyCode::Backspace), InputEvent::Back);

        // Menu
        assert_eq!(key_to_input(KeyCode::Char('m')), InputEvent::Menu);

        // Unknown
        assert_eq!(key_to_input(KeyCode::Delete), InputEvent::None);
    }

    #[test]
    fn test_input_event_equality() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let event1 = InputEvent::Up;
        let event2 = InputEvent::Up;
        let event3 = InputEvent::Down;

        assert_eq!(event1, event2);
        assert_ne!(event1, event3);

        // Test hashing
        let mut hasher1 = DefaultHasher::new();
        let mut hasher2 = DefaultHasher::new();
        let mut hasher3 = DefaultHasher::new();

        event1.hash(&mut hasher1);
        event2.hash(&mut hasher2);
        event3.hash(&mut hasher3);

        assert_eq!(hasher1.finish(), hasher2.finish());
        assert_ne!(hasher1.finish(), hasher3.finish());
    }

    #[test]
    fn test_input_manager_creation() {
        let manager = InputManager::new();
        assert!(manager.is_ok());
    }

    #[test]
    fn test_input_event_debug() {
        let event = InputEvent::Quit;
        let debug_str = format!("{:?}", event);
        assert_eq!(debug_str, "Quit");

        let event = InputEvent::Up;
        let debug_str = format!("{:?}", event);
        assert_eq!(debug_str, "Up");
    }

    #[test]
    fn test_input_event_copy() {
        let event = InputEvent::Select;
        let _copied = event; // Should compile - InputEvent is Copy
        let _cloned = event; // Should compile - InputEvent is Clone
    }
}
