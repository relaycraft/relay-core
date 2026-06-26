use ratatui::{
    style::Style,
    text::{Line, Span},
};
use serde_json::Value;

use super::super::format::{BodyView, format_size};
use super::super::theme::Theme;

/// Maximum body bytes to render before truncating.
const BODY_RENDER_LIMIT: usize = 4096;
/// Maximum body bytes to attempt full rendering; beyond this show truncated view.
const BODY_FULL_RENDER_LIMIT: usize = 65536;

/// Dispatch body rendering based on BodyView.
pub(super) fn render_body_for_view(body_content: &str, view: BodyView) -> Vec<Line<'static>> {
    if body_content.len() > BODY_FULL_RENDER_LIMIT {
        return vec![Line::from(Span::styled(
            format!(
                "View unavailable — body too large ({} bytes). Only first {} shown.",
                format_size(body_content.len() as u64),
                format_size(BODY_FULL_RENDER_LIMIT as u64)
            ),
            Theme::muted(),
        ))];
    }
    match view {
        BodyView::Auto => render_body_auto(body_content),
        BodyView::Pretty => render_body_pretty(body_content),
        BodyView::Raw => render_body_raw(body_content),
        BodyView::Hex => render_body_hex(body_content.as_bytes()),
    }
}

/// Auto-detect: JSON → pretty, binary → hex, other → raw.
fn render_body_auto(body_content: &str) -> Vec<Line<'static>> {
    if serde_json::from_str::<Value>(body_content).is_ok() {
        render_body_pretty(body_content)
    } else if body_content
        .bytes()
        .any(|b| b < 0x20 && b != b'\n' && b != b'\r' && b != b'\t')
    {
        render_body_hex(body_content.as_bytes())
    } else {
        render_body_raw(body_content)
    }
}

/// JSON pretty-printed with syntax highlighting.
fn render_body_pretty(body_content: &str) -> Vec<Line<'static>> {
    if let Ok(value) = serde_json::from_str::<Value>(body_content)
        && let Ok(formatted) = serde_json::to_string_pretty(&value)
    {
        return render_json_highlighted(&formatted, BODY_RENDER_LIMIT);
    }
    render_body_raw(body_content)
}

/// Raw UTF-8 text, truncated.
fn render_body_raw(body_content: &str) -> Vec<Line<'static>> {
    let truncated = body_content.len() > BODY_RENDER_LIMIT;
    let display = if truncated {
        let boundary = body_content.floor_char_boundary(BODY_RENDER_LIMIT);
        &body_content[..boundary]
    } else {
        body_content
    };
    let mut lines: Vec<Line> = display
        .lines()
        .map(|line| Line::from(Span::styled(line.to_string(), Theme::text())))
        .collect();
    if truncated {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "… showing first {} of {} bytes",
                format_size(BODY_RENDER_LIMIT as u64),
                format_size(body_content.len() as u64)
            ),
            Theme::muted(),
        )));
    }
    lines
}

/// Hex dump: 16 bytes per row with offset + ASCII column.
fn render_body_hex(data: &[u8]) -> Vec<Line<'static>> {
    let limit = BODY_RENDER_LIMIT.min(data.len());
    let mut lines: Vec<Line> = Vec::new();
    for row in 0..limit.div_ceil(16) {
        let offset = row * 16;
        let row_end = (offset + 16).min(limit);
        let hex_part: String = data[offset..row_end]
            .iter()
            .map(|b| format!("{b:02x} "))
            .collect::<Vec<_>>()
            .join("");
        let hex_part = format!("{hex_part:<48}");
        let ascii_part: String = data[offset..row_end]
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        lines.push(Line::from(vec![
            Span::styled(format!("{offset:08x}  "), Theme::label()),
            Span::styled(hex_part, Theme::text()),
            Span::styled(ascii_part, Theme::muted()),
        ]));
    }
    if data.len() > limit {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "… showing first {} of {} bytes",
                format_size(limit as u64),
                format_size(data.len() as u64)
            ),
            Theme::muted(),
        )));
    }
    lines
}

/// JSON syntax highlighting for a pre-formatted (pretty) string.
fn render_json_highlighted(formatted: &str, limit: usize) -> Vec<Line<'static>> {
    let truncated = formatted.len() > limit;
    let display = if truncated {
        let boundary = formatted.floor_char_boundary(limit);
        &formatted[..boundary]
    } else {
        formatted
    };
    let mut lines: Vec<Line> = display
        .lines()
        .map(|line| {
            Line::from(
                highlight_json_line(line)
                    .into_iter()
                    .map(|(text, style)| Span::styled(text, style))
                    .collect::<Vec<Span>>(),
            )
        })
        .collect();
    if truncated {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "… showing first {} of {} bytes",
                format_size(limit as u64),
                format_size(formatted.len() as u64)
            ),
            Theme::muted(),
        )));
    }
    lines
}

/// Simple JSON line highlighter: returns (text, style) pairs.
fn highlight_json_line(line: &str) -> Vec<(String, Style)> {
    let mut parts: Vec<(String, Style)> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let len = chars.len();

    while i < len {
        let c = chars[i];

        // Whitespace
        if c.is_whitespace() {
            let start = i;
            while i < len && chars[i].is_whitespace() {
                i += 1;
            }
            parts.push((chars[start..i].iter().collect(), Theme::text()));
            continue;
        }

        // String (double-quoted)
        if c == '"' {
            let start = i;
            i += 1;
            while i < len {
                if chars[i] == '\\' && i + 1 < len {
                    i += 2;
                } else if chars[i] == '"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            let s: String = chars[start..i].iter().collect();
            // Check if this string is a key (followed by :)
            let is_key = chars.get(i).is_some_and(|&_nc| {
                let rest: String = chars[i..].iter().collect();
                rest.trim_start().starts_with(':')
            });
            parts.push((
                s,
                if is_key {
                    Theme::json_key()
                } else {
                    Theme::json_string()
                },
            ));
            continue;
        }

        // Number
        if c == '-' || c.is_ascii_digit() {
            let start = i;
            if c == '-' {
                i += 1;
            }
            while i < len && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            parts.push((chars[start..i].iter().collect(), Theme::json_number()));
            continue;
        }

        // Boolean / null
        if c == 't' || c == 'f' || c == 'n' {
            let start = i;
            while i < len && chars[i].is_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if word == "true" || word == "false" || word == "null" {
                parts.push((word, Theme::json_bool()));
                continue;
            }
            // Not a keyword, backtrack
            i = start;
        }

        // Structural chars (brackets, braces, commas, colons)
        parts.push((c.to_string(), Theme::muted()));
        i += 1;
    }

    parts
}
