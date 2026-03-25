//! ANSI escape code to ratatui Span converter.
//!
//! Parses text with embedded ANSI color codes and produces
//! styled ratatui Spans.

use ratatui::prelude::*;

/// Parse a string with ANSI escape codes into styled ratatui Lines.
pub fn parse_to_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for raw_line in text.split('\n') {
        let spans = parse_spans(raw_line);
        lines.push(Line::from(spans));
    }

    lines
}

/// Parse a single line with ANSI codes into spans.
fn parse_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current_style = Style::default();
    let mut buf = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Flush current buffer
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), current_style));
            }

            // Parse escape sequence
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                let mut seq = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphabetic() {
                        chars.next(); // consume the final letter
                        break;
                    }
                    seq.push(chars.next().unwrap());
                }

                // Parse SGR (Select Graphic Rendition) codes
                current_style = apply_sgr(&seq, current_style);
            }
        } else {
            buf.push(ch);
        }
    }

    // Flush remaining
    if !buf.is_empty() {
        spans.push(Span::styled(buf, current_style));
    }

    if spans.is_empty() {
        spans.push(Span::raw(""));
    }

    spans
}

/// Apply an SGR sequence to a style.
fn apply_sgr(seq: &str, base: Style) -> Style {
    if seq.is_empty() || seq == "0" {
        return Style::default(); // reset
    }

    let mut style = base;

    for code in seq.split(';') {
        match code {
            "0" => style = Style::default(),
            "1" => style = style.bold(),
            "2" => style = style.dim(),
            "3" => style = style.italic(),
            "4" => style = style.underlined(),
            // Foreground colors
            "30" => style = style.fg(Color::Black),
            "31" => style = style.fg(Color::Red),
            "32" => style = style.fg(Color::Green),
            "33" => style = style.fg(Color::Yellow),
            "34" => style = style.fg(Color::Blue),
            "35" => style = style.fg(Color::Magenta),
            "36" => style = style.fg(Color::Cyan),
            "37" => style = style.fg(Color::White),
            "90" => style = style.fg(Color::DarkGray),
            "91" => style = style.fg(Color::LightRed),
            "92" => style = style.fg(Color::LightGreen),
            "93" => style = style.fg(Color::LightYellow),
            "94" => style = style.fg(Color::LightBlue),
            "95" => style = style.fg(Color::LightMagenta),
            "96" => style = style.fg(Color::LightCyan),
            "97" => style = style.fg(Color::White),
            // 256-color: 38;5;N
            "38" => {
                // Will be handled with next codes via split
            }
            "5" => {
                // Part of 38;5;N — but we already split, so this won't work
                // Handled below as a special case
            }
            _ => {
                // Try 256-color: the seq might be "38;5;123"
                if let Some(n) = code.parse::<u8>().ok() {
                    // This might be the color index after "38;5;"
                    if seq.starts_with("38;5;") {
                        style = style.fg(Color::Indexed(n));
                    }
                }
            }
        }
    }

    // Handle 38;5;N as a complete sequence
    if seq.starts_with("38;5;") {
        if let Some(n) = seq.strip_prefix("38;5;").and_then(|s| s.parse::<u8>().ok()) {
            style = Style::default().fg(Color::Indexed(n));
        }
    }

    style
}
