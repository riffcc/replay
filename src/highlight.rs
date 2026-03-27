//! Syntax highlighting — maps file extensions to syntect highlighting,
//! returns ratatui Spans with appropriate colors.

use std::path::Path;

use ratatui::prelude::*;
use syntect::easy::HighlightLines;
use syntect::highlighting::{ThemeSet, Style as SynStyle};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Shared highlighting state (lazy-loaded syntax definitions + theme).
pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl Highlighter {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    /// Highlight a line of code, returning ratatui Spans.
    /// Falls back to plain text if the extension isn't recognized.
    pub fn highlight_line<'a>(
        &self,
        line: &str,
        extension: &str,
        base_style: Option<Style>,
    ) -> Vec<Span<'a>> {
        let syntax = self.syntax_set
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let theme = &self.theme_set.themes["base16-ocean.dark"];
        let mut h = HighlightLines::new(syntax, theme);

        let regions = match h.highlight_line(line, &self.syntax_set) {
            Ok(r) => r,
            Err(_) => return vec![Span::raw(line.to_string())],
        };

        regions.iter().map(|(style, text)| {
            let fg = syn_to_ratatui_color(style.foreground);
            let mut ratatui_style = Style::default().fg(fg);

            // If there's a base style (e.g. for diff lines), blend it
            if let Some(base) = base_style {
                if let Some(bg) = base.bg {
                    ratatui_style = ratatui_style.bg(bg);
                }
            }

            Span::styled(text.to_string(), ratatui_style)
        }).collect()
    }

    /// Highlight a line for a diff context (applies base fg from syntax, with
    /// the diff background color).
    pub fn highlight_diff_line<'a>(
        &self,
        line: &str,
        extension: &str,
        bg: Color,
    ) -> Vec<Span<'a>> {
        let syntax = self.syntax_set
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let theme = &self.theme_set.themes["base16-ocean.dark"];
        let mut h = HighlightLines::new(syntax, theme);

        let regions = match h.highlight_line(line, &self.syntax_set) {
            Ok(r) => r,
            Err(_) => return vec![Span::styled(line.to_string(), Style::default().bg(bg))],
        };

        regions.iter().map(|(style, text)| {
            let fg = syn_to_ratatui_color(style.foreground);
            Span::styled(text.to_string(), Style::default().fg(fg).bg(bg))
        }).collect()
    }
}

/// Extract file extension from a path string.
pub fn extension_from_path(path: &str) -> &str {
    Path::new(path.trim())
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("txt")
}

fn syn_to_ratatui_color(c: syntect::highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}
