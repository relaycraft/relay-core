use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, HighlightSpacing, Padding, Paragraph, Row, Table},
};
use relay_core_api::flow::{Flow, Layer};
use url::Url;

use super::super::app::{ActiveArea, TuiApp};
use super::super::format::{
    ColumnWidth, LayoutProfile, display_method, display_path, empty_flow_list_message,
    flow_duration_ms, flow_list_title, format_duration_ms, format_host_port, format_size,
    path_budget_for, plan_columns, smart_truncate, tags_list_suffix,
};
use super::super::theme::Theme;

pub(in crate::ui) fn render_flow_list(app: &mut TuiApp, f: &mut Frame, area: Rect) {
    // ratatui needs mutable access to `TableState` for stateful table rendering.
    // Keep other flow-list rendering logic read-only despite this wider borrow.
    let filtered_flows = app.get_filtered_flows();
    let show_mark = !app.marks.is_empty();
    let profile = LayoutProfile::for_flow_list_width(area.width);
    let columns = plan_columns(profile, show_mark);
    let filter = &app.filter_input;
    let filtering = !filter.is_empty();
    let path_budget = path_budget_for(profile, area.width, show_mark);

    let list_focused = app.active_area == ActiveArea::FlowList;
    let title = flow_list_title(filter, filtered_flows.len(), app.flows.len());

    let widths: Vec<Constraint> = columns
        .iter()
        .map(|c| match c.width {
            ColumnWidth::Fixed(w) => Constraint::Length(w),
            ColumnWidth::Rest => Constraint::Min(10),
        })
        .collect();

    if filtered_flows.is_empty() {
        let block = outer_panel_block(&title, list_focused).padding(panel_body_padding());
        f.render_widget(
            Paragraph::new(empty_flow_list_message(filtering))
                .style(Theme::muted())
                .block(block),
            area,
        );
        return;
    }

    let selected = app.table_state.selected();
    let rows: Vec<Row> = filtered_flows
        .iter()
        .enumerate()
        .map(|(i, flow)| {
            let mark = app.marks.get(&flow.id).copied();
            flow_table_row(
                flow,
                profile,
                path_budget,
                filter,
                selected == Some(i),
                show_mark,
                mark,
            )
        })
        .collect();

    let header: Vec<&str> = columns.iter().map(|c| c.header).collect();
    let header_row = Row::new(header).style(Theme::table_header());

    let table = Table::new(rows, widths)
        .header(header_row)
        .block(outer_panel_block(&title, list_focused))
        .row_highlight_style(Theme::row_highlight())
        .highlight_spacing(HighlightSpacing::Never);

    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn flow_table_row(
    flow: &Flow,
    profile: LayoutProfile,
    path_budget: usize,
    filter: &str,
    selected: bool,
    show_mark: bool,
    mark: Option<char>,
) -> Row<'static> {
    let filtering = !filter.is_empty();
    let (method, url, status, size_str, has_body, has_query) = match &flow.layer {
        Layer::Http(h) => {
            let size = h
                .response
                .as_ref()
                .and_then(|r| r.body.as_ref())
                .map(|b| b.size)
                .unwrap_or(0);
            (
                h.request.method.clone(),
                h.request.url.clone(),
                if let Some(resp) = &h.response {
                    resp.status.to_string()
                } else {
                    "---".to_string()
                },
                format_size(size),
                h.request.body.is_some(),
                !h.request.query.is_empty() || h.request.url.query().is_some(),
            )
        }
        Layer::WebSocket(w) => (
            "WS".to_string(),
            w.handshake_request.url.clone(),
            w.handshake_response.status.to_string(),
            String::new(),
            false,
            w.handshake_request.url.query().is_some(),
        ),
        _ => (
            "???".to_string(),
            Url::parse("http://unknown/").unwrap(),
            "---".to_string(),
            String::new(),
            false,
            false,
        ),
    };

    let method_label = display_method(&method, has_body);
    let dur_ms = flow_duration_ms(flow);
    let dur_label = format_duration_ms(dur_ms);

    let marker_cell = if selected {
        Cell::from(Span::styled("▸", Theme::row_marker()))
    } else {
        Cell::from(Span::raw(""))
    };
    let mark_cell = Cell::from(Span::styled(
        mark.map(|c| c.to_string()).unwrap_or_default(),
        match mark {
            Some(c) => Style::default().fg(Theme::host_color(&c.to_string())),
            None => Style::default(),
        },
    ));
    let method_key = method_label.trim_end_matches('+');
    let method_cell = Cell::from(Span::styled(
        method_label.clone(),
        Theme::method_text(method_key),
    ));
    let (status_text, status_style) = if status == "---" {
        ("…".to_string(), Theme::pending_status())
    } else {
        (format!("{:>3}", status), Theme::status_badge(&status))
    };
    let status_cell = Cell::from(Span::styled(status_text, status_style));
    let dur_cell = Cell::from(Span::styled(
        format!("{:>6}", dur_label),
        Theme::duration_style(dur_ms),
    ));

    let tags_str = tags_list_suffix(&flow.tags);

    let mut prefix = vec![marker_cell];
    if show_mark {
        prefix.push(mark_cell);
    }

    match profile {
        LayoutProfile::TooNarrow | LayoutProfile::SinglePane => {
            let url_text = smart_truncate(url.as_str(), path_budget);
            let mut cells = prefix;
            cells.push(Cell::from(Span::styled(url_text, Theme::text())));
            Row::new(cells)
        }
        LayoutProfile::TwoPaneCompact | LayoutProfile::TwoPaneStandard => {
            let url_text = smart_truncate(url.as_str(), path_budget);
            let url_with_tags = if tags_str.is_empty() {
                url_text
            } else {
                format!("{}{}", url_text, tags_str)
            };
            let mut cells = prefix;
            cells.extend([
                method_cell,
                status_cell,
                dur_cell,
                styled_text_cell(&url_with_tags, filter, filtering, Theme::text()),
            ]);
            Row::new(cells)
        }
        LayoutProfile::TwoPaneWide | LayoutProfile::TwoPaneExtraWide => {
            let host = format_host_port(&url);
            let host_color = Theme::host_color(&host);
            let path = display_path(&url, path_budget, has_query);
            let path_with_tags = if tags_str.is_empty() {
                path
            } else {
                format!("{}{}", path, tags_str)
            };
            let mut cells = prefix;
            cells.extend([
                method_cell,
                status_cell,
                dur_cell,
                Cell::from(Span::styled(format!("{:>7}", size_str), Theme::text())),
                Cell::from(Span::styled(host, Style::default().fg(host_color))),
                styled_text_cell(&path_with_tags, filter, filtering, Theme::text()),
            ]);
            Row::new(cells)
        }
    }
}

fn styled_text_cell(text: &str, filter: &str, filtering: bool, base: Style) -> Cell<'static> {
    if filtering && !filter.is_empty() {
        Cell::from(Line::from(highlight_filter_spans(text, filter, base)))
    } else {
        Cell::from(Span::styled(text.to_string(), base))
    }
}

fn highlight_filter_spans(text: &str, filter: &str, base: Style) -> Vec<Span<'static>> {
    let lower_text = text.to_lowercase();
    let lower_filter = filter.to_lowercase();
    let mut spans = Vec::new();
    let mut last = 0;
    for (idx, _) in lower_text.match_indices(&lower_filter) {
        if idx > last {
            spans.push(Span::styled(text[last..idx].to_string(), base));
        }
        spans.push(Span::styled(
            text[idx..idx + filter.len()].to_string(),
            Theme::filter_hit(),
        ));
        last = idx + filter.len();
    }
    if last < text.len() {
        spans.push(Span::styled(text[last..].to_string(), base));
    }
    if spans.is_empty() {
        spans.push(Span::styled(text.to_string(), base));
    }
    spans
}

fn outer_panel_block(title: &str, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Theme::border(focused))
        .title(Span::styled(format!(" {title} "), Theme::panel_title()))
        .style(Theme::surface())
}

fn panel_body_padding() -> Padding {
    Padding {
        left: 1,
        right: 0,
        top: 0,
        bottom: 0,
    }
}
