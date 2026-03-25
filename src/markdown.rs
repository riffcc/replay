//! Markdown to ratatui — converts markdown text into styled Lines.
//!
//! Uses pulldown-cmark to parse, maps events to ratatui Spans with styles.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::prelude::*;

/// Parse markdown text into styled ratatui Lines.
pub fn render(markdown: &str) -> Vec<Line<'static>> {
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let parser = Parser::new_ext(markdown, options);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];
    let mut in_code_block = false;
    let mut code_block_lines: Vec<String> = Vec::new();
    let mut list_depth: usize = 0;
    let mut ordered_index: Option<u64> = None;

    for event in parser {
        match event {
            Event::Start(tag) => {
                match tag {
                    Tag::Heading { level, .. } => {
                        flush_line(&mut lines, &mut current_spans);
                        // Blank line before heading (unless we're at the start)
                        if !lines.is_empty() {
                            lines.push(Line::raw(""));
                        }
                        let style = heading_style(level);
                        style_stack.push(style);
                    }
                    Tag::Paragraph => {
                        // Start of paragraph
                    }
                    Tag::Emphasis => {
                        let current = *style_stack.last().unwrap_or(&Style::default());
                        style_stack.push(current.italic());
                    }
                    Tag::Strong => {
                        let current = *style_stack.last().unwrap_or(&Style::default());
                        style_stack.push(current.bold());
                    }
                    Tag::Strikethrough => {
                        let current = *style_stack.last().unwrap_or(&Style::default());
                        style_stack.push(current.fg(Color::DarkGray));
                    }
                    Tag::CodeBlock(_) => {
                        flush_line(&mut lines, &mut current_spans);
                        in_code_block = true;
                        code_block_lines.clear();
                    }
                    Tag::List(start) => {
                        list_depth += 1;
                        ordered_index = start;
                    }
                    Tag::Item => {
                        flush_line(&mut lines, &mut current_spans);
                        let indent = "  ".repeat(list_depth.saturating_sub(1));
                        let bullet = if let Some(ref mut idx) = ordered_index {
                            let s = format!("{indent}{idx}. ");
                            *idx += 1;
                            s
                        } else {
                            format!("{indent}• ")
                        };
                        current_spans.push(Span::styled(bullet, Style::default().fg(Color::DarkGray)));
                    }
                    Tag::BlockQuote(_) => {
                        let current = *style_stack.last().unwrap_or(&Style::default());
                        style_stack.push(current.fg(Color::DarkGray).italic());
                    }
                    Tag::Link { dest_url, .. } => {
                        let current = *style_stack.last().unwrap_or(&Style::default());
                        style_stack.push(current.fg(Color::Cyan).underlined());
                        // Store URL for later — we'll append it after the link text
                        let _ = dest_url; // TODO: show URL
                    }
                    Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {
                        // Tables: render cells separated by │
                    }
                    _ => {}
                }
            }
            Event::End(tag_end) => {
                match tag_end {
                    TagEnd::Heading(_) => {
                        style_stack.pop();
                        flush_line(&mut lines, &mut current_spans);
                        lines.push(Line::raw(""));
                    }
                    TagEnd::Paragraph => {
                        flush_line(&mut lines, &mut current_spans);
                        lines.push(Line::raw(""));
                    }
                    TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough
                    | TagEnd::BlockQuote(_) | TagEnd::Link => {
                        style_stack.pop();
                    }
                    TagEnd::CodeBlock => {
                        in_code_block = false;
                        // Render code block with background
                        for code_line in &code_block_lines {
                            lines.push(Line::from(Span::styled(
                                format!("  {code_line}"),
                                Style::default().fg(Color::Green),
                            )));
                        }
                        lines.push(Line::raw(""));
                        code_block_lines.clear();
                    }
                    TagEnd::List(_) => {
                        list_depth = list_depth.saturating_sub(1);
                        if list_depth == 0 {
                            ordered_index = None;
                        }
                    }
                    TagEnd::Item => {
                        flush_line(&mut lines, &mut current_spans);
                    }
                    TagEnd::TableHead | TagEnd::TableRow => {
                        flush_line(&mut lines, &mut current_spans);
                    }
                    TagEnd::TableCell => {
                        current_spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                    }
                    _ => {}
                }
            }
            Event::Text(text) => {
                if in_code_block {
                    // Collect code block lines
                    for line in text.split('\n') {
                        code_block_lines.push(line.to_string());
                    }
                } else {
                    let style = *style_stack.last().unwrap_or(&Style::default());
                    // Split on newlines within text
                    let parts: Vec<&str> = text.split('\n').collect();
                    for (i, part) in parts.iter().enumerate() {
                        if i > 0 {
                            flush_line(&mut lines, &mut current_spans);
                        }
                        if !part.is_empty() {
                            current_spans.push(Span::styled(part.to_string(), style));
                        }
                    }
                }
            }
            Event::Code(code) => {
                current_spans.push(Span::styled(
                    code.to_string(),
                    Style::default().fg(Color::Yellow),
                ));
            }
            Event::SoftBreak => {
                current_spans.push(Span::raw(" "));
            }
            Event::HardBreak => {
                flush_line(&mut lines, &mut current_spans);
            }
            Event::Rule => {
                flush_line(&mut lines, &mut current_spans);
                lines.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::raw(""));
            }
            _ => {}
        }
    }

    // Flush any remaining spans
    flush_line(&mut lines, &mut current_spans);

    // Trim trailing empty lines
    while lines.last().map(|l| l.spans.is_empty() || (l.spans.len() == 1 && l.spans[0].content.is_empty())).unwrap_or(false) {
        lines.pop();
    }

    lines
}

fn flush_line(lines: &mut Vec<Line<'static>>, spans: &mut Vec<Span<'static>>) {
    if !spans.is_empty() {
        lines.push(Line::from(std::mem::take(spans)));
    }
}

fn heading_style(level: HeadingLevel) -> Style {
    match level {
        HeadingLevel::H1 => Style::default().fg(Color::White).bold(),
        HeadingLevel::H2 => Style::default().fg(Color::Cyan).bold(),
        HeadingLevel::H3 => Style::default().fg(Color::Yellow).bold(),
        _ => Style::default().fg(Color::White).bold(),
    }
}

/// Convert markdown to pre-wrapped plain-ish text lines for the output buffer.
/// Returns strings with embedded ANSI-free content (ratatui handles styling).
/// This is used when we need to push markdown into AppState as raw strings.
pub fn to_plain_lines(markdown: &str) -> Vec<String> {
    let styled_lines = render(markdown);
    styled_lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}
