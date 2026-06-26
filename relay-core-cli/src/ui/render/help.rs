use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use super::super::app::{ApiMode, TuiApp};
use super::super::theme::Theme;

pub(in crate::ui) fn render_help_overlay(app: &TuiApp, f: &mut Frame) {
    let mut lines: Vec<Line> = vec![
        Line::from(vec![Span::styled("RelayCore TUI", Theme::accent_bold())]),
        Line::from(""),
    ];

    match app.api_mode {
        ApiMode::Connected => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("HTTP API: ", Theme::label()),
                Span::styled("on", Theme::stat_ok()),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "REST + SSE on localhost (/api/v1/flows, rules, events) for ",
                    Theme::muted(),
                ),
                Span::styled("relay flows", Theme::accent_dim()),
                Span::styled(", MCP, and other clients. ", Theme::muted()),
                Span::styled("This TUI is unchanged.", Theme::muted()),
            ]));
        }
        ApiMode::Offline => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("HTTP API: ", Theme::label()),
                Span::styled("off", Theme::muted()),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "--api-port PORT starts REST + SSE on localhost for ",
                    Theme::muted(),
                ),
                Span::styled("relay flows", Theme::accent_dim()),
                Span::styled(", MCP, and integrations. ", Theme::muted()),
                Span::styled("Does not change this TUI.", Theme::muted()),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("Example: ", Theme::label()),
                Span::styled("relay run --ui --api-port 8082", Theme::accent_dim()),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(help_section("Flow List"));
    lines.extend([
        help_binding("j  ↓", "Move selection down"),
        help_binding("k  ↑", "Move selection up"),
        help_binding("g  Home", "Jump to newest (top)"),
        help_binding("G  End", "Jump to oldest (bottom)"),
        help_binding("Tab", "Focus detail panel (from list)"),
        help_binding("/", "Filter (host: path: method: status: err ws)"),
        help_binding("m", "Toggle mark on selected flow (A-Z; same key removes)"),
        help_binding("'", "Jump to next mark"),
        help_binding("R", "Replay selected request"),
        help_binding("Enter  →", "Focus detail panel"),
    ]);

    lines.push(Line::from(""));
    lines.push(help_section("Detail Tabs"));
    lines.extend([
        help_binding("Esc  ←", "Back to flow list"),
        help_binding("Tab", "Cycle Overview → Request → Response → Messages"),
        help_binding("1 / 2 / 3 / 4", "Jump to tab (not a four-panel layout)"),
        help_binding("PgUp  PgDown", "Scroll content"),
        help_binding("Ctrl+u  Ctrl+d", "Scroll up / down 10 lines"),
        help_binding("v", "Cycle body view: Auto → Pretty → Raw → Hex"),
    ]);

    lines.push(Line::from(""));
    lines.push(help_section("Actions"));
    lines.extend([
        help_binding(":", "Command palette (:q :clear :filter :theme :view)"),
        help_binding("y", "Copy selected flow as cURL"),
        help_binding("d", "Delete selected flow"),
        help_binding("p", "Pause / resume list (proxy keeps running)"),
        help_binding("c", "Clear flow list"),
    ]);
    lines.push(Line::from(""));
    lines.push(help_section("General"));
    lines.extend([
        help_binding("?", "Toggle this help"),
        help_binding("q", "Quit"),
    ]);

    let help_width = 72;
    let help_height = lines.len() as u16 + 2;
    let area = centered_rect(help_width, help_height, f.area());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Theme::border(true))
        .title(Span::styled(
            " Help (? or Esc to close) ",
            Theme::panel_title(),
        ))
        .style(Style::default().bg(Theme::bg_elevated()));
    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
}

fn help_section(title: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("· {title} ·"), Theme::subsection()),
    ])
}

fn help_binding(keys: &'static str, description: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{keys:<16}"), Theme::hotkey()),
        Span::styled(description, Theme::muted()),
    ])
}

fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((r.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Length((r.height.saturating_sub(height)) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((r.width.saturating_sub(width)) / 2),
            Constraint::Length(width),
            Constraint::Length((r.width.saturating_sub(width)) / 2),
        ])
        .split(vertical[1])[1]
}
