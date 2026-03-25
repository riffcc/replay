//! KITT-style throbber — animated thinking indicator.
//!
//! 4 unicode block characters that warble back and forth,
//! changing color based on activity state.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Activity state affects color.
const STATE_IDLE: u8 = 0;     // waiting for first token
const STATE_THINKING: u8 = 1; // model is generating
const STATE_TOOL: u8 = 2;     // tool call in progress

/// The KITT blocks cycle through these brightness levels.
const BLOCKS: [char; 4] = ['▁', '▃', '▅', '▇'];

/// Colors per state.
fn state_color(state: u8, intensity: u8) -> &'static str {
    match (state, intensity) {
        (STATE_IDLE, 3) => "\x1b[38;5;33m",    // bright blue
        (STATE_IDLE, 2) => "\x1b[38;5;27m",    // medium blue
        (STATE_IDLE, 1) => "\x1b[38;5;21m",    // dim blue
        (STATE_IDLE, _) => "\x1b[38;5;17m",    // very dim blue

        (STATE_THINKING, 3) => "\x1b[38;5;214m", // bright amber
        (STATE_THINKING, 2) => "\x1b[38;5;208m", // medium amber
        (STATE_THINKING, 1) => "\x1b[38;5;166m", // dim amber
        (STATE_THINKING, _) => "\x1b[38;5;94m",  // very dim amber

        (STATE_TOOL, 3) => "\x1b[38;5;48m",    // bright cyan
        (STATE_TOOL, 2) => "\x1b[38;5;42m",    // medium cyan
        (STATE_TOOL, 1) => "\x1b[38;5;36m",    // dim cyan
        (STATE_TOOL, _) => "\x1b[38;5;30m",    // very dim cyan

        _ => "\x1b[38;5;240m",
    }
}

const RESET: &str = "\x1b[0m";

/// Handle to a running throbber animation.
pub struct Throbber {
    running: Arc<AtomicBool>,
    state: Arc<AtomicU8>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Throbber {
    /// Start the throbber animation.
    pub fn start() -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let state = Arc::new(AtomicU8::new(STATE_IDLE));

        let r = Arc::clone(&running);
        let s = Arc::clone(&state);

        let handle = std::thread::spawn(move || {
            let mut position: i32 = 0;
            let mut direction: i32 = 1;
            let mut frame: u64 = 0;

            while r.load(Ordering::Relaxed) {
                let current_state = s.load(Ordering::Relaxed);

                // Calculate intensity for each of 4 positions based on "scanner" position
                let mut out = io::stdout();
                write!(out, "\r").ok();

                for i in 0..4i32 {
                    // Distance from the scanner head
                    let dist = (i - position).unsigned_abs();
                    let intensity = match dist {
                        0 => 3,
                        1 => 2,
                        2 => 1,
                        _ => 0,
                    };

                    let color = state_color(current_state, intensity as u8);
                    let block = BLOCKS[intensity as usize];
                    write!(out, "{color}{block}{RESET}").ok();
                }

                write!(out, " ").ok(); // trailing space to clear any leftover chars
                out.flush().ok();

                // Move scanner
                position += direction;
                if position >= 3 {
                    direction = -1;
                } else if position <= 0 {
                    direction = 1;
                }

                frame += 1;

                // Speed varies by state
                let delay = match current_state {
                    STATE_THINKING => 80,
                    STATE_TOOL => 120,
                    _ => 150,
                };

                std::thread::sleep(Duration::from_millis(delay));
            }

            // Clear the throbber
            let mut out = io::stdout();
            write!(out, "\r\x1b[2K").ok();
            out.flush().ok();
        });

        Self {
            running,
            state,
            handle: Some(handle),
        }
    }

    /// Set the activity state (changes color and speed).
    pub fn set_thinking(&self) {
        self.state.store(STATE_THINKING, Ordering::Relaxed);
    }

    pub fn set_tool(&self) {
        self.state.store(STATE_TOOL, Ordering::Relaxed);
    }

    pub fn set_idle(&self) {
        self.state.store(STATE_IDLE, Ordering::Relaxed);
    }

    /// Stop the animation and clear it.
    pub fn stop(mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Throbber {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Don't join in drop — might deadlock
    }
}
