//! TUI — the face of Replay.

pub mod input;
pub mod menu;
pub mod screen;

use anyhow::{Context, Result};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;

use input::{InputEvent, InputManager};
use screen::{Screen, Transition};

/// Launch the TUI. This is the main entry point for Replay.
pub fn run() -> Result<()> {
    enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;

    let mut input = InputManager::new()?;

    let result = event_loop(&mut terminal, &mut input);

    disable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)?;

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    input: &mut InputManager,
) -> Result<()> {
    let mut screen = Screen::initial();

    loop {
        terminal.draw(|frame| screen.render(frame))?;

        if let Some(event) = input.poll()? {
            if event == InputEvent::Quit {
                return Ok(());
            }

            match screen.handle(event) {
                Transition::Stay => {}
                Transition::Goto(next) => screen = next,
                Transition::Quit => return Ok(()),
            }
        }
    }
}
