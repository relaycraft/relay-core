use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Padding, Paragraph},
};
use std::time::Instant;

use super::super::app::{ActiveArea, InputMode, TuiApp};
use super::super::theme::Theme;

pub(in crate::ui) fn render_status_bar(app: &TuiApp, f: &mut Frame, area: Rect) {
    let flow_count = app.flows.len();
    let filtered_count = app.get_filtered_flows().len();
    let count_str = if filtered_count != flow_count {
        format!("{}/{}", filtered_count, flow_count)
    } else {
        flow_count.to_string()
    };

    // req/s: count timestamps in the last 1 second
    let now = Instant::now();
    let req_per_sec = app
        .req_timestamps
        .iter()
        .rev()
        .take_while(|t| now.duration_since(**t).as_secs() < 1)
        .count();

    let elapsed = app.start_time.elapsed();
    let uptime = if elapsed.as_secs() < 60 {
        format!("{}s", elapsed.as_secs())
    } else if elapsed.as_secs() < 3600 {
        format!("{}m{}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
    } else {
        format!(
            "{}h{}m",
            elapsed.as_secs() / 3600,
            (elapsed.as_secs() % 3600) / 60
        )
    };

    let bar_text = match app.input_mode {
        InputMode::Normal => {
            let rec = if app.paused {
                Span::styled("[⏸ PAUSED] ", Theme::error_bold())
            } else {
                Span::styled("[●REC] ", Theme::stat_ok())
            };

            let pending = if app.pending_count > 0 {
                Span::styled(format!("↓{} new ", app.pending_count), Theme::accent_bold())
            } else {
                Span::raw("")
            };

            let focus = match app.active_area {
                ActiveArea::FlowList => Span::styled("· LIST · ", Theme::muted()),
                ActiveArea::FlowDetail => Span::styled("· DETAIL · ", Theme::muted()),
            };

            // Left section
            let left = vec![
                rec,
                pending,
                focus,
                Span::styled("Flows: ", Theme::label()),
                Span::styled(format!("{} ", count_str), Theme::stat_ok()),
                Span::styled(format!("{}req/s ", req_per_sec), Theme::stat_info()),
                Span::styled("| ", Theme::muted()),
                Span::styled("Total: ", Theme::label()),
                Span::styled(format!("{}", app.flow_count_total), Theme::text()),
            ];

            // Middle section
            let middle = vec![
                Span::styled("proxy ", Theme::label()),
                Span::styled(format!(":{}", app.proxy_port), Theme::uptime()),
                Span::styled("  up ", Theme::muted()),
                Span::styled(uptime, Theme::uptime()),
            ];

            // Right section — priority-based hints that truncate on narrow terminals.
            let hints = app.status_hints();
            let left_width = Text::from(Line::from(left.clone())).width() as u16;
            let middle_width = Text::from(Line::from(middle.clone())).width() as u16;
            let right_budget = area.width.saturating_sub(left_width + middle_width);

            let mut right_spans: Vec<Span> = Vec::new();
            let mut used = 0u16;
            for (min_width, hint) in &hints {
                let hint_w = (hint.len() as u16).max(*min_width) + 1;
                if used + hint_w <= right_budget || right_spans.is_empty() {
                    if !right_spans.is_empty() {
                        right_spans.push(Span::raw(" "));
                    }
                    right_spans.extend(status_hint_spans(hint));
                    used += hint_w;
                }
            }

            let spacer1 = if left_width + middle_width + used < area.width {
                (area.width - left_width - middle_width - used) / 2
            } else {
                1u16
            };
            let spacer2 = area
                .width
                .saturating_sub(left_width + spacer1 + middle_width + used);

            let mut main_spans: Vec<Span> = Vec::new();
            main_spans.extend(left);
            main_spans.push(Span::raw(" ".repeat(spacer1 as usize)));
            main_spans.extend(middle);
            main_spans.push(Span::raw(" ".repeat(spacer2 as usize)));
            main_spans.extend(right_spans);

            let bar_block = status_bar_block();
            let bar_area = bar_block.inner(area);
            f.render_widget(bar_block, area);

            let toast_alive = app.toast_at.is_some_and(|at| at.elapsed().as_secs() < 5);
            if !toast_alive {
                f.render_widget(Paragraph::new(Line::from(main_spans)), bar_area);
            } else if let Some(ref msg) = app.toast {
                let status_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Length(1)])
                    .split(bar_area);
                f.render_widget(Paragraph::new(Line::from(main_spans)), status_chunks[0]);
                let toast_line = Line::from(vec![
                    Span::styled("▸ ", Theme::row_marker()),
                    Span::styled(msg.as_str(), Theme::accent_dim()),
                ]);
                f.render_widget(Paragraph::new(toast_line), status_chunks[1]);
            } else {
                f.render_widget(Paragraph::new(Line::from(main_spans)), bar_area);
            }
            return;
        }
        InputMode::Filtering => {
            let mut spans = vec![
                Span::styled("Filter: ", Theme::label()),
                Span::styled(app.filter_input.as_str(), Theme::accent()),
            ];
            if app.filter_input.is_empty() {
                spans.push(Span::styled(
                    "  host:api.example method:POST status:>=400 err",
                    Theme::muted(),
                ));
            }
            spans.extend([
                Span::styled(" | ", Theme::muted()),
                Span::styled("Enter", Theme::hotkey()),
                Span::styled(" apply | ", Theme::muted()),
                Span::styled("Esc", Theme::hotkey()),
                Span::styled(" cancel", Theme::muted()),
            ]);
            spans
        }
        InputMode::Command => {
            let colon = if app.command_input.is_empty() {
                Span::styled(":", Theme::accent())
            } else {
                Span::styled(":", Theme::muted())
            };
            vec![
                colon,
                Span::styled(app.command_input.as_str(), Theme::accent()),
                Span::styled(" | ", Theme::muted()),
                Span::styled("Enter", Theme::hotkey()),
                Span::styled(" execute | ", Theme::muted()),
                Span::styled("Esc", Theme::hotkey()),
                Span::styled(" cancel", Theme::muted()),
            ]
        }
        InputMode::Marking => {
            vec![
                Span::styled("mark — ", Theme::accent_bold()),
                Span::styled("A-Z to toggle", Theme::muted()),
                Span::styled(" | ", Theme::muted()),
                Span::styled("Esc", Theme::hotkey()),
                Span::styled(" cancel", Theme::muted()),
            ]
        }
        InputMode::Help => {
            vec![
                Span::styled("HELP", Theme::accent_bold()),
                Span::styled(" — press ", Theme::muted()),
                Span::styled("? or Esc", Theme::hotkey()),
                Span::styled(" to close", Theme::muted()),
            ]
        }
    };

    let text = Text::from(Line::from(bar_text));
    let paragraph = Paragraph::new(text).block(status_bar_block());
    f.render_widget(paragraph, area);
}

fn status_bar_block() -> Block<'static> {
    Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Theme::status_bar_border())
        .style(Theme::surface())
        .padding(Padding {
            left: 1,
            right: 0,
            top: 0,
            bottom: 0,
        })
}

/// Render `[key]label` hints with accent keys and muted labels (status bar).
fn status_hint_spans(hint: &str) -> Vec<Span<'_>> {
    if let Some(rest) = hint.strip_prefix('[')
        && let Some((key, tail)) = rest.split_once(']')
    {
        return vec![
            Span::styled("[", Theme::muted()),
            Span::styled(key, Theme::hotkey()),
            Span::styled("]", Theme::muted()),
            Span::styled(tail, Theme::muted()),
        ];
    }
    if hint.starts_with('↓') {
        vec![Span::styled(hint, Theme::accent_bold())]
    } else if hint.starts_with("marks:") {
        vec![Span::styled(hint, Theme::accent_dim())]
    } else {
        vec![Span::styled(hint, Theme::muted())]
    }
}
