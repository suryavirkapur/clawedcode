use ratatui::{
    prelude::*,
    text::{Line, Span},
};

const ACCENT: Color = Color::Rgb(224, 122, 95);
const CODE_BG: Color = Color::Rgb(40, 40, 40);
const CODE_FG: Color = Color::Rgb(180, 180, 180);
const TABLE_BORDER: Color = Color::Rgb(100, 100, 100);
const HEADER_FG: Color = Color::Rgb(255, 255, 255);
const BOLD_FG: Color = Color::Rgb(240, 240, 240);
const ITALIC_FG: Color = Color::Rgb(200, 200, 200);
const BLOCKQUOTE_FG: Color = Color::Rgb(150, 150, 150);

/// Parse markdown text into styled ratatui Lines.
pub fn parse_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;
    let mut _code_block_lang = String::new();
    let mut code_block_lines = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim_end();

        // Code block handling
        if line.starts_with("```") {
            if in_code_block {
                // End code block
                lines.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().fg(CODE_BG),
                )));
                for code_line in &code_block_lines {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", code_line),
                        Style::default().fg(CODE_FG),
                    )));
                }
                lines.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().fg(CODE_BG),
                )));
                in_code_block = false;
                code_block_lines.clear();
            } else {
                // Start code block
                in_code_block = true;
                _code_block_lang = line.trim_start_matches('`').trim().to_string();
            }
            continue;
        }

        if in_code_block {
            code_block_lines.push(line.to_string());
            continue;
        }

        // Empty line
        if line.is_empty() {
            lines.push(Line::from(""));
            continue;
        }

        // Horizontal rule
        if line.trim() == "---" || line.trim() == "***" || line.trim() == "___" {
            lines.push(Line::from(Span::styled(
                "─".repeat(40),
                Style::default().fg(TABLE_BORDER),
            )));
            continue;
        }

        // Header
        if let Some(header_line) = parse_header(line) {
            lines.push(header_line);
            continue;
        }

        // Blockquote
        if line.starts_with("> ") {
            let content = &line[2..];
            let spans = parse_inline(content);
            let mut styled = vec![Span::styled("> ", Style::default().fg(BLOCKQUOTE_FG))];
            styled.extend(spans);
            lines.push(Line::from(styled));
            continue;
        }

        // Table row
        if is_table_row(line) {
            // Skip separator rows like |---|---|
            if line.contains("|---") {
                continue;
            }
            let table_line = parse_table_row(line);
            lines.push(table_line);
            continue;
        }

        // List item
        if line.starts_with("- ") || line.starts_with("* ") {
            let content = &line[2..];
            let spans = parse_inline(content);
            let mut styled = vec![Span::styled("  • ", Style::default().fg(ACCENT))];
            styled.extend(spans);
            lines.push(Line::from(styled));
            continue;
        }

        // Numbered list
        if let Some(rest) = line.strip_prefix(|c: char| c.is_ascii_digit()) {
            if let Some(content) = rest.strip_prefix(". ") {
                let spans = parse_inline(content);
                let mut styled = vec![Span::styled(
                    format!("  {}. ", &line[..line.len() - content.len() - 2]),
                    Style::default().fg(ACCENT),
                )];
                styled.extend(spans);
                lines.push(Line::from(styled));
                continue;
            }
        }

        // Regular paragraph with inline formatting
        lines.push(Line::from(parse_inline(line)));
    }

    // Handle unclosed code block
    if in_code_block && !code_block_lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "─".repeat(40),
            Style::default().fg(CODE_BG),
        )));
        for code_line in &code_block_lines {
            lines.push(Line::from(Span::styled(
                format!("  {}", code_line),
                Style::default().fg(CODE_FG),
            )));
        }
        lines.push(Line::from(Span::styled(
            "─".repeat(40),
            Style::default().fg(CODE_BG),
        )));
    }

    lines
}

fn parse_header(line: &str) -> Option<Line<'static>> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|&c| c == '#').count();
    if level == 0 || level > 6 || !trimmed.chars().nth(level).map(|c| c == ' ').unwrap_or(false) {
        return None;
    }
    let text = trimmed[level..].trim();
    let style = match level {
        1 => Style::default()
            .fg(HEADER_FG)
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::UNDERLINED),
        2 => Style::default()
            .fg(HEADER_FG)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(BOLD_FG)
            .add_modifier(Modifier::BOLD),
    };
    Some(Line::from(Span::styled(text.to_string(), style)))
}

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|')
}

fn parse_table_row(line: &str) -> Line<'static> {
    let trimmed = line.trim();
    let cells: Vec<&str> = trimmed
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(|s| s.trim())
        .collect();

    let mut spans = Vec::new();
    spans.push(Span::styled("| ", Style::default().fg(TABLE_BORDER)));

    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" | ", Style::default().fg(TABLE_BORDER)));
        }
        let cell_spans = parse_inline(cell);
        spans.extend(cell_spans);
    }

    spans.push(Span::styled(" |", Style::default().fg(TABLE_BORDER)));
    Line::from(spans)
}

/// Parse inline markdown: bold, italic, inline code, links.
fn parse_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        // Find the next special pattern
        let mut found = None;

        // Inline code: `...`
        if let Some(start) = remaining.find('`') {
            if let Some(end) = remaining[start + 1..].find('`') {
                found = Some((start, start + 1 + end + 1, InlineStyle::Code));
            }
        }

        // Bold: **...**
        if let Some(start) = remaining.find("**") {
            if let Some(end) = remaining[start + 2..].find("**") {
                let end_pos = start + 2 + end;
                if found.map(|(s, _, _)| start < s).unwrap_or(true) {
                    found = Some((start, end_pos + 2, InlineStyle::Bold));
                }
            }
        }

        // Italic: *...* (but not **)
        if let Some(start) = remaining.find('*') {
            // Skip if it's part of **
            if remaining.get(start..start + 2) != Some("**") {
                if let Some(end) = remaining[start + 1..].find('*') {
                    let end_pos = start + 1 + end;
                    if found.map(|(s, _, _)| start < s).unwrap_or(true) {
                        found = Some((start, end_pos + 1, InlineStyle::Italic));
                    }
                }
            }
        }

        // Italic: _..._
        if let Some(start) = remaining.find('_') {
            if let Some(end) = remaining[start + 1..].find('_') {
                let end_pos = start + 1 + end;
                if found.map(|(s, _, _)| start < s).unwrap_or(true) {
                    found = Some((start, end_pos + 1, InlineStyle::Italic));
                }
            }
        }

        match found {
            Some((start, end, style)) => {
                if start > 0 {
                    spans.push(Span::raw(remaining[..start].to_string()));
                }
                let content = match style {
                    InlineStyle::Bold => &remaining[start + 2..end - 2],
                    InlineStyle::Italic => {
                        if remaining.get(start..start + 1) == Some("*") {
                            &remaining[start + 1..end - 1]
                        } else {
                            &remaining[start + 1..end - 1]
                        }
                    }
                    InlineStyle::Code => &remaining[start + 1..end - 1],
                };
                spans.push(style_span(content, style));
                remaining = &remaining[end..];
            }
            None => {
                spans.push(Span::raw(remaining.to_string()));
                break;
            }
        }
    }

    spans
}

#[derive(Clone, Copy)]
enum InlineStyle {
    Bold,
    Italic,
    Code,
}

fn style_span(text: &str, style: InlineStyle) -> Span<'static> {
    match style {
        InlineStyle::Bold => Span::styled(
            text.to_string(),
            Style::default()
                .fg(BOLD_FG)
                .add_modifier(Modifier::BOLD),
        ),
        InlineStyle::Italic => Span::styled(
            text.to_string(),
            Style::default()
                .fg(ITALIC_FG)
                .add_modifier(Modifier::ITALIC),
        ),
        InlineStyle::Code => Span::styled(
            text.to_string(),
            Style::default().fg(CODE_FG).bg(CODE_BG),
        ),
    }
}

/// Convert a plain text block (like tool results) into wrapped lines.
pub fn format_plain_text(text: &str, prefix: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        if raw_line.trim().is_empty() {
            lines.push(Line::from(""));
        } else {
            lines.push(Line::from(vec![
                Span::styled(prefix.to_string(), Style::default().fg(MUTED)),
                Span::raw(raw_line.to_string()),
            ]));
        }
    }
    lines
}

const MUTED: Color = Color::Rgb(150, 150, 150);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bold_text() {
        let lines = parse_markdown("This is **bold** text");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert!(spans.iter().any(|s| s.content == "bold" && s.style.add_modifier == Modifier::BOLD));
    }

    #[test]
    fn parse_italic_text() {
        let lines = parse_markdown("This is *italic* text");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert!(spans.iter().any(|s| s.content == "italic" && s.style.add_modifier == Modifier::ITALIC));
    }

    #[test]
    fn parse_inline_code() {
        let lines = parse_markdown("Use `cargo build` to compile");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert!(spans.iter().any(|s| s.content == "cargo build"));
    }

    #[test]
    fn parse_header() {
        let lines = parse_markdown("# Hello\n## World");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].spans.iter().any(|s| s.content == "Hello"));
        assert!(lines[1].spans.iter().any(|s| s.content == "World"));
    }

    #[test]
    fn parse_code_block() {
        let text = "```rust\nfn main() {}\n```";
        let lines = parse_markdown(text);
        // Should have border + code line + border = 3 lines
        assert!(lines.len() >= 2);
    }

    #[test]
    fn parse_table() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |";
        let lines = parse_markdown(text);
        // Should skip separator, keep header and data rows
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn parse_list() {
        let lines = parse_markdown("- first\n- second");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].spans.iter().any(|s| s.content.contains('•')));
    }
}
