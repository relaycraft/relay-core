use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};
use relay_core_api::flow::{Flow, Layer};

use super::super::app::{ActiveArea, DetailTab, TuiApp};
use super::super::format::{
    display_method, flow_duration_ms, format_duration_ms, format_host_port, format_size,
    smart_truncate, visible_flow_tags,
};
use super::super::theme::Theme;
use super::body::render_body_for_view;

pub(in crate::ui) fn render_flow_detail(app: &TuiApp, f: &mut Frame, area: Rect) {
    let detail_focused = app.active_area == ActiveArea::FlowDetail;
    let detail_title = "Detail";
    let block = outer_panel_block(detail_title, detail_focused);
    let inner = block.inner(area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)].as_ref())
        .split(inner);

    render_detail_tab_bar(f, chunks[0], app.detail_tab);
    f.render_widget(block, area);

    let filtered_flows = app.get_filtered_flows();
    if let Some(selected) = app.table_state.selected() {
        if let Some(flow) = filtered_flows.get(selected) {
            match app.detail_tab {
                DetailTab::Overview => render_overview(app, f, chunks[1], flow),
                DetailTab::Request => render_request(app, f, chunks[1], flow),
                DetailTab::Response => render_response(app, f, chunks[1], flow),
                DetailTab::Messages => render_messages(app, f, chunks[1], flow),
            }
        } else {
            f.render_widget(
                Paragraph::new("Flow not found")
                    .style(Theme::muted())
                    .block(panel_body_block(0)),
                chunks[1],
            );
        }
    } else {
        f.render_widget(
            Paragraph::new("Select a flow to view details")
                .style(Theme::muted())
                .block(panel_body_block(0)),
            chunks[1],
        );
    }
}

fn render_overview(app: &TuiApp, f: &mut Frame, area: Rect, flow: &Flow) {
    let block = panel_body_block(app.detail_scroll);

    let mut lines: Vec<Line> = Vec::new();

    if let Some(summary) = flow_summary_line(flow) {
        lines.push(summary);
    }

    lines.push(panel_kv_line("ID:", flow.id.to_string(), Theme::muted()));

    match &flow.layer {
        Layer::Http(h) => {
            if let Some(query) = h.request.url.query() {
                lines.push(panel_kv_line(
                    "Query:",
                    smart_truncate(query, 160),
                    Theme::muted(),
                ));
            }
            let size = h
                .response
                .as_ref()
                .and_then(|r| r.body.as_ref())
                .map(|b| b.size)
                .unwrap_or(0);
            if size > 0 {
                lines.push(panel_kv_line("Size:", format_size(size), Theme::value()));
            }
            if let Some(err) = &h.error {
                lines.push(panel_kv_line("Error:", err.as_str(), Theme::error()));
            }
        }
        Layer::WebSocket(w) => {
            lines.push(panel_kv_line(
                "Messages:",
                w.messages.len().to_string(),
                Theme::value(),
            ));
            if let Some(query) = w.handshake_request.url.query() {
                lines.push(panel_kv_line(
                    "Query:",
                    smart_truncate(query, 160),
                    Theme::muted(),
                ));
            }
        }
        _ => {
            lines.push(Line::from("Unknown Layer"));
        }
    }

    push_panel_section(&mut lines, "Network");
    let net = &flow.network;
    lines.push(panel_kv_line_indented(
        PANEL_SECTION_INDENT,
        "Client:",
        format!("{}:{}", net.client_ip, net.client_port),
        Theme::value(),
    ));
    lines.push(panel_kv_line_indented(
        PANEL_SECTION_INDENT,
        "Server:",
        format!("{}:{}", net.server_ip, net.server_port),
        Theme::value(),
    ));
    if net.tls {
        let tls_val = net.tls_version.as_deref().unwrap_or("on").to_string();
        lines.push(panel_kv_line_indented(
            PANEL_SECTION_INDENT,
            "TLS:",
            tls_val,
            Theme::stat_ok(),
        ));
    }
    if let Some(ref sni) = net.sni {
        lines.push(panel_kv_line_indented(
            PANEL_SECTION_INDENT,
            "SNI:",
            sni.clone(),
            Theme::value(),
        ));
    }

    if !flow.tags.is_empty() {
        let visible = visible_flow_tags(&flow.tags);
        if !visible.is_empty() {
            push_panel_section(&mut lines, "Tags");
            lines.push(Line::from(vec![
                Span::raw(PANEL_SECTION_INDENT),
                Span::styled(visible.join("  "), Theme::accent_dim()),
            ]));
        }
    }

    let timing_present = match &flow.layer {
        Layer::Http(h) => h
            .response
            .as_ref()
            .is_some_and(|r| r.timing.time_to_first_byte.is_some()),
        Layer::WebSocket(w) => w.handshake_response.timing.time_to_first_byte.is_some(),
        _ => false,
    };

    if timing_present || flow.end_time.is_some() {
        push_panel_section(&mut lines, "Timing");

        let (ttfb, ttlb, connect, ssl) = match &flow.layer {
            Layer::Http(h) => {
                if let Some(ref resp) = h.response {
                    (
                        resp.timing.time_to_first_byte,
                        resp.timing.time_to_last_byte,
                        resp.timing.connect_time_ms,
                        resp.timing.ssl_time_ms,
                    )
                } else {
                    (None, None, None, None)
                }
            }
            Layer::WebSocket(w) => {
                let t = &w.handshake_response.timing;
                (
                    t.time_to_first_byte,
                    t.time_to_last_byte,
                    t.connect_time_ms,
                    t.ssl_time_ms,
                )
            }
            _ => (None, None, None, None),
        };

        let total_ms = flow
            .end_time
            .map(|e| (e - flow.start_time).num_milliseconds() as u64)
            .or(ttlb);

        lines.push(panel_kv_line_indented(
            PANEL_SECTION_INDENT,
            "Total:",
            format_timing_ms(total_ms),
            timing_value_style(total_ms),
        ));
        lines.push(panel_kv_line_indented(
            PANEL_SECTION_INDENT,
            "TTFB:",
            format_timing_ms(ttfb),
            timing_value_style(ttfb),
        ));
        lines.push(panel_kv_line_indented(
            PANEL_SECTION_INDENT,
            "TTLB:",
            format_timing_ms(ttlb),
            timing_value_style(ttlb),
        ));

        if connect.is_some() || ssl.is_some() {
            lines.push(panel_kv_line_indented(
                PANEL_SECTION_INDENT,
                "Connect:",
                format_timing_ms(connect),
                timing_value_style(connect),
            ));
            lines.push(panel_kv_line_indented(
                PANEL_SECTION_INDENT,
                "SSL:",
                format_timing_ms(ssl),
                timing_value_style(ssl),
            ));
        }
    }

    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: true })
        .scroll((app.detail_scroll, 0));
    f.render_widget(p, area);
}

fn render_request(app: &TuiApp, f: &mut Frame, area: Rect, flow: &Flow) {
    let block = panel_body_block(app.detail_scroll);

    match &flow.layer {
        Layer::Http(h) => {
            let mut text = vec![Line::from(Span::styled("Headers", Theme::subsection()))];
            for header in &h.request.headers {
                text.push(Line::from(vec![
                    Span::styled(format!("{}: ", header.0), Theme::header_key()),
                    Span::styled(&header.1, Theme::value()),
                ]));
            }

            text.push(Line::from(""));
            text.push(Line::from(Span::styled("Body", Theme::subsection())));

            if let Some(body) = &h.request.body {
                text.push(Line::from(format!(
                    "Size: {} bytes, Encoding: {}",
                    body.size, body.encoding
                )));
                if body.size > 0 {
                    text.push(Line::from(""));
                    for line in render_body_for_view(&body.content, app.body_view) {
                        text.push(line);
                    }
                }
            } else {
                text.push(Line::from("(No Body)"));
            }

            let p = Paragraph::new(text)
                .block(block)
                .wrap(Wrap { trim: true })
                .scroll((app.detail_scroll, 0));
            f.render_widget(p, area);
        }
        Layer::WebSocket(w) => {
            let mut text = vec![Line::from(Span::styled(
                "Handshake request headers",
                Theme::subsection(),
            ))];
            for header in &w.handshake_request.headers {
                text.push(Line::from(vec![
                    Span::styled(format!("{}: ", header.0), Theme::header_key()),
                    Span::styled(&header.1, Theme::value()),
                ]));
            }
            let p = Paragraph::new(text)
                .block(block)
                .wrap(Wrap { trim: true })
                .scroll((app.detail_scroll, 0));
            f.render_widget(p, area);
        }
        _ => f.render_widget(Paragraph::new("N/A").block(block), area),
    }
}

fn render_response(app: &TuiApp, f: &mut Frame, area: Rect, flow: &Flow) {
    let block = panel_body_block(app.detail_scroll);

    match &flow.layer {
        Layer::Http(h) => {
            if let Some(resp) = &h.response {
                let mut text = vec![Line::from(Span::styled("Headers", Theme::subsection()))];
                for header in &resp.headers {
                    text.push(Line::from(vec![
                        Span::styled(format!("{}: ", header.0), Theme::header_key()),
                        Span::styled(&header.1, Theme::value()),
                    ]));
                }

                text.push(Line::from(""));
                text.push(Line::from(Span::styled("Body", Theme::subsection())));

                if let Some(body) = &resp.body {
                    text.push(Line::from(format!(
                        "Size: {} bytes, Encoding: {}",
                        body.size, body.encoding
                    )));
                    if body.size > 0 {
                        text.push(Line::from(""));
                        for line in render_body_for_view(&body.content, app.body_view) {
                            text.push(line);
                        }
                    }
                } else {
                    text.push(Line::from("(No Body)"));
                }

                let p = Paragraph::new(text)
                    .block(block)
                    .wrap(Wrap { trim: true })
                    .scroll((app.detail_scroll, 0));
                f.render_widget(p, area);
            } else {
                f.render_widget(Paragraph::new("Waiting for response...").block(block), area);
            }
        }
        Layer::WebSocket(w) => {
            let mut text = vec![Line::from(Span::styled(
                "Handshake response headers",
                Theme::subsection(),
            ))];
            for header in &w.handshake_response.headers {
                text.push(Line::from(vec![
                    Span::styled(format!("{}: ", header.0), Theme::header_key()),
                    Span::styled(&header.1, Theme::value()),
                ]));
            }
            let p = Paragraph::new(text)
                .block(block)
                .wrap(Wrap { trim: true })
                .scroll((app.detail_scroll, 0));
            f.render_widget(p, area);
        }
        _ => f.render_widget(Paragraph::new("N/A").block(block), area),
    }
}

fn render_messages(app: &TuiApp, f: &mut Frame, area: Rect, flow: &Flow) {
    let block = panel_body_block(app.detail_scroll);

    match &flow.layer {
        Layer::WebSocket(w) => {
            let lines: Vec<Line> = w
                .messages
                .iter()
                .map(|msg| {
                    let direction = match msg.direction {
                        relay_core_api::flow::Direction::ClientToServer => "->",
                        relay_core_api::flow::Direction::ServerToClient => "<-",
                    };
                    let dir_style = match msg.direction {
                        relay_core_api::flow::Direction::ClientToServer => Theme::ws_outbound(),
                        relay_core_api::flow::Direction::ServerToClient => Theme::ws_inbound(),
                    };

                    let content = if msg.content.size > 50 {
                        format!(
                            "{}... ({} bytes)",
                            &msg.content.content[..50],
                            msg.content.size
                        )
                    } else {
                        msg.content.content.clone()
                    };

                    Line::from(vec![
                        Span::styled(format!("{} ", direction), dir_style),
                        Span::styled(format!("[{}] ", msg.opcode), Theme::ws_opcode()),
                        Span::styled(content, Theme::text()),
                    ])
                })
                .collect();

            let p = Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: true })
                .scroll((app.detail_scroll, 0));
            f.render_widget(p, area);
        }
        _ => f.render_widget(Paragraph::new("Not a WebSocket flow").block(block), area),
    }
}

fn outer_panel_block(title: &str, focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Theme::border(focused))
        .title(Span::styled(format!(" {title} "), Theme::panel_title()))
        .style(Theme::surface())
}

/// Left inset for panel body text (tabs, tables, key-value lines) under border titles.
fn panel_body_padding() -> Padding {
    Padding {
        left: 1,
        right: 0,
        top: 0,
        bottom: 0,
    }
}

/// Panel body — optional scroll hint for detail tabs.
fn panel_body_block(scroll: u16) -> Block<'static> {
    let mut block = Block::default()
        .style(Theme::surface())
        .padding(panel_body_padding());
    if scroll > 0 {
        block = block.title(Span::styled(format!(" ↓{scroll} "), Theme::muted()));
    }
    block
}

fn render_detail_tab_bar(f: &mut Frame, area: Rect, selected: DetailTab) {
    let mut spans: Vec<Span> = Vec::new();
    for (i, &tab) in DetailTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        let style = if tab == selected {
            Theme::tab_active()
        } else {
            Theme::tab_inactive()
        };
        spans.push(Span::styled(tab.label(), style));
    }
    let block = Block::default().padding(panel_body_padding());
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

/// Fixed label column so key-value rows align across panels.
pub(crate) const PANEL_KV_LABEL_WIDTH: usize = 12;
const PANEL_SECTION_INDENT: &str = "  ";

pub(crate) fn panel_kv_label_column(label: &str) -> String {
    format!("{label:<PANEL_KV_LABEL_WIDTH$}")
}

fn panel_kv_line(label: &str, value: impl AsRef<str>, value_style: Style) -> Line<'static> {
    panel_kv_line_indented("", label, value, value_style)
}

fn panel_kv_line_indented(
    indent: &str,
    label: &str,
    value: impl AsRef<str>,
    value_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::raw(indent.to_string()),
        Span::styled(panel_kv_label_column(label), Theme::label()),
        Span::styled(value.as_ref().to_string(), value_style),
    ])
}

fn panel_section_gap() -> Line<'static> {
    Line::from("")
}

fn push_panel_section(lines: &mut Vec<Line>, title: &'static str) {
    lines.push(panel_section_gap());
    lines.push(subsection_line(title));
}

fn format_timing_ms(ms: Option<u64>) -> String {
    match ms {
        Some(v) => format!("{v}ms"),
        None => "—".to_string(),
    }
}

fn timing_value_style(ms: Option<u64>) -> Style {
    if ms.is_some() {
        Theme::text()
    } else {
        Theme::muted()
    }
}

fn subsection_line(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(format!("· {title} ·"), Theme::subsection()))
}

fn flow_summary_line(flow: &Flow) -> Option<Line<'static>> {
    match &flow.layer {
        Layer::Http(h) => {
            let method_label = display_method(&h.request.method, h.request.body.is_some());
            let method_key = method_label.trim_end_matches('+');
            let status = h
                .response
                .as_ref()
                .map(|r| r.status.to_string())
                .unwrap_or_else(|| "…".to_string());
            let dur = format_duration_ms(flow_duration_ms(flow));
            let host_str = format_host_port(&h.request.url);
            let path = h.request.url.path();
            let path = if path.is_empty() { "/" } else { path };
            let path_show = smart_truncate(path, 40);
            Some(Line::from(vec![
                Span::styled(method_label.clone(), Theme::method_badge(method_key)),
                Span::raw("  "),
                Span::styled(format!("{status:>3}"), Theme::status_badge(&status)),
                Span::styled(
                    format!("  {dur}  "),
                    Theme::duration_style(flow_duration_ms(flow)),
                ),
                Span::styled(format!("{host_str}{path_show}"), Theme::value()),
            ]))
        }
        Layer::WebSocket(w) => {
            let status = w.handshake_response.status.to_string();
            let dur = format_duration_ms(flow_duration_ms(flow));
            let host_str = format_host_port(&w.handshake_request.url);
            let path = w.handshake_request.url.path();
            let path = if path.is_empty() { "/" } else { path };
            let path_show = smart_truncate(path, 40);
            let msg_count = w.messages.len();
            Some(Line::from(vec![
                Span::styled("WS", Theme::method_badge("WS")),
                Span::raw("  "),
                Span::styled(format!("{status:>3}"), Theme::status_badge(&status)),
                Span::styled(
                    format!("  {dur}  "),
                    Theme::duration_style(flow_duration_ms(flow)),
                ),
                Span::styled(format!("{host_str}{path_show}"), Theme::value()),
                Span::styled(format!("  ({msg_count} msgs)"), Theme::muted()),
            ]))
        }
        _ => None,
    }
}
