use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap},
};
use relay_core_api::flow::{Flow, Layer};
use relay_core_api::modification::{flow_matches_filter, parse_flow_filter};
use serde_json::Value;
use std::collections::VecDeque;
use url::Url;

use super::format::{
    LAYOUT_NARROW_MAX, TABLE_WIDE_MIN, copy_to_clipboard, display_method, display_path,
    flow_duration_ms, flow_list_title, format_duration_ms, format_size, host_from_url,
    http_flow_to_curl, smart_truncate,
};
use super::theme::Theme;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DetailTab {
    Overview,
    Request,
    Response,
    Messages,
}

impl DetailTab {
    fn next(&self) -> Self {
        match self {
            Self::Overview => Self::Request,
            Self::Request => Self::Response,
            Self::Response => Self::Messages,
            Self::Messages => Self::Overview,
        }
    }

    fn tab_line(&self) -> Line<'static> {
        match self {
            Self::Overview => tab_line_pair("Overview", '1'),
            Self::Request => tab_line_pair("Request", '2'),
            Self::Response => tab_line_pair("Response", '3'),
            Self::Messages => tab_line_pair("Messages", '4'),
        }
    }
}

fn tab_line_pair(label: &'static str, num: char) -> Line<'static> {
    Line::from(vec![
        Span::styled(label, Theme::section()),
        Span::raw(" "),
        Span::styled(num.to_string(), Theme::accent_dim()),
    ])
}

#[derive(PartialEq, Debug)]
pub enum InputMode {
    Normal,
    Filtering,
    Help,
}

#[derive(PartialEq, Debug)]
pub enum ActiveArea {
    FlowList,
    FlowDetail,
}

pub struct TuiApp {
    pub flows: VecDeque<Flow>,
    pub table_state: TableState,
    pub detail_tab: DetailTab,
    pub input_mode: InputMode,
    pub active_area: ActiveArea,
    pub detail_scroll: u16,
    pub filter_input: String,
    pub should_quit: bool,
    pub auto_scroll: bool,
    pub start_time: std::time::Instant,
    pub flow_count_total: u64,
    pub proxy_port: u16,
    /// One-shot message for status bar (e.g. copy confirmation).
    pub toast: Option<String>,
}

impl TuiApp {
    pub fn new(port: u16) -> Self {
        let mut app = Self {
            flows: VecDeque::with_capacity(1000),
            table_state: TableState::default(),
            detail_tab: DetailTab::Overview,
            input_mode: InputMode::Normal,
            active_area: ActiveArea::FlowList,
            detail_scroll: 0,
            filter_input: String::new(),
            should_quit: false,
            auto_scroll: true,
            start_time: std::time::Instant::now(),
            flow_count_total: 0,
            proxy_port: port,
            toast: None,
        };
        app.table_state.select(Some(0));
        app
    }

    pub fn on_flow(&mut self, flow: Flow) {
        if let Some(pos) = self.flows.iter().position(|f| f.id == flow.id) {
            self.flows[pos] = flow;
        } else {
            self.flows.push_front(flow);
            self.flow_count_total = self.flow_count_total.saturating_add(1);
            if self.flows.len() > 1000 {
                self.flows.pop_back();
            }
            if self.auto_scroll {
                self.table_state.select(Some(0));
            }
        }
    }

    fn get_filtered_flows(&self) -> Vec<&Flow> {
        if self.filter_input.is_empty() {
            return self.flows.iter().collect();
        }
        let filter = parse_flow_filter(&self.filter_input);
        self.flows
            .iter()
            .filter(|flow| flow_matches_filter(flow, &filter))
            .collect()
    }

    fn selected_flow(&self) -> Option<&Flow> {
        self.table_state
            .selected()
            .and_then(|i| self.get_filtered_flows().get(i).copied())
    }

    fn copy_curl_selection(&mut self) {
        let Some(flow) = self.selected_flow() else {
            self.toast = Some("No flow selected".into());
            return;
        };
        let Some(curl) = http_flow_to_curl(flow) else {
            self.toast = Some("cURL: not an HTTP/WebSocket flow".into());
            return;
        };
        if copy_to_clipboard(&curl) {
            self.toast = Some("cURL copied to clipboard".into());
        } else {
            self.toast = Some("cURL built (install pbcopy/xclip for clipboard)".into());
        }
    }

    pub fn on_key(&mut self, event: KeyEvent) {
        // Ignore Repeat/Release — e.g. `?` Press opens Help, Repeat would instantly close it.
        if event.kind != KeyEventKind::Press {
            return;
        }
        self.toast = None;
        let key = event.code;
        let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
        match self.input_mode {
            InputMode::Normal => {
                if key == KeyCode::Char('?') {
                    self.input_mode = InputMode::Help;
                    return;
                }
                match self.active_area {
                    ActiveArea::FlowList => match key {
                        KeyCode::Char('q') => self.should_quit = true,
                        KeyCode::Char('d') => {
                            self.delete_selected();
                            self.detail_scroll = 0;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.next();
                            self.auto_scroll = false;
                            self.detail_scroll = 0;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.previous();
                            self.auto_scroll = false;
                            self.detail_scroll = 0;
                        }
                        KeyCode::Home | KeyCode::Char('G') => {
                            // Jump to last (oldest) flow
                            let len = self.get_filtered_flows().len();
                            if len > 0 {
                                self.table_state.select(Some(len - 1));
                            }
                            self.auto_scroll = false;
                            self.detail_scroll = 0;
                        }
                        KeyCode::End | KeyCode::Char('g') => {
                            // Jump to first (newest) flow
                            self.table_state.select(Some(0));
                            self.auto_scroll = true;
                            self.detail_scroll = 0;
                        }
                        KeyCode::Tab => {
                            self.detail_tab = self.detail_tab.next();
                            self.detail_scroll = 0;
                        }
                        KeyCode::Char('1') => self.detail_tab = DetailTab::Overview,
                        KeyCode::Char('2') => self.detail_tab = DetailTab::Request,
                        KeyCode::Char('3') => self.detail_tab = DetailTab::Response,
                        KeyCode::Char('4') => self.detail_tab = DetailTab::Messages,
                        KeyCode::Char('/') => self.input_mode = InputMode::Filtering,
                        KeyCode::Char('?') => self.input_mode = InputMode::Help,
                        KeyCode::Char('y') => self.copy_curl_selection(),
                        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                            self.active_area = ActiveArea::FlowDetail
                        }
                        _ => {}
                    },
                    ActiveArea::FlowDetail => match key {
                        KeyCode::Char('q') => self.should_quit = true,
                        KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                            self.active_area = ActiveArea::FlowList
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.detail_scroll = self.detail_scroll.saturating_add(1)
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.detail_scroll = self.detail_scroll.saturating_sub(1)
                        }
                        KeyCode::PageDown | KeyCode::Char('d') if ctrl => {
                            self.detail_scroll = self.detail_scroll.saturating_add(10)
                        }
                        KeyCode::PageUp | KeyCode::Char('u') if ctrl => {
                            self.detail_scroll = self.detail_scroll.saturating_sub(10)
                        }
                        KeyCode::Home | KeyCode::Char('g') => self.detail_scroll = 0,
                        KeyCode::End | KeyCode::Char('G') => self.detail_scroll = u16::MAX,
                        KeyCode::Tab => {
                            self.detail_tab = self.detail_tab.next();
                            self.detail_scroll = 0;
                        }
                        KeyCode::Char('1') => self.detail_tab = DetailTab::Overview,
                        KeyCode::Char('2') => self.detail_tab = DetailTab::Request,
                        KeyCode::Char('3') => self.detail_tab = DetailTab::Response,
                        KeyCode::Char('4') => self.detail_tab = DetailTab::Messages,
                        KeyCode::Char('y') => self.copy_curl_selection(),
                        _ => {}
                    },
                }
            }
            InputMode::Filtering => match key {
                KeyCode::Enter | KeyCode::Esc => self.input_mode = InputMode::Normal,
                KeyCode::Char('?') => self.input_mode = InputMode::Help,
                KeyCode::Char(c) => self.filter_input.push(c),
                KeyCode::Backspace => {
                    self.filter_input.pop();
                }
                _ => {}
            },
            InputMode::Help => {
                if key == KeyCode::Char('?') || key == KeyCode::Esc {
                    self.input_mode = InputMode::Normal;
                }
            }
        }
    }

    pub fn next(&mut self) {
        let filtered_len = self.get_filtered_flows().len();
        if filtered_len == 0 {
            self.table_state.select(None);
            return;
        }

        let i = match self.table_state.selected() {
            Some(i) => {
                if i >= filtered_len - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let filtered_len = self.get_filtered_flows().len();
        if filtered_len == 0 {
            self.table_state.select(None);
            return;
        }

        let i = match self.table_state.selected() {
            Some(i) => {
                if i == 0 {
                    filtered_len - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn delete_selected(&mut self) {
        let id = self
            .table_state
            .selected()
            .and_then(|i| self.get_filtered_flows().get(i).map(|f| f.id));
        if let Some(id) = id {
            self.flows.retain(|f| f.id != id);
            let new_len = self.get_filtered_flows().len();
            if new_len == 0 {
                self.table_state.select(None);
            } else if self.table_state.selected().unwrap_or(0) >= new_len {
                self.table_state.select(Some(new_len - 1));
            }
        }
    }

    pub fn ui(&mut self, f: &mut Frame) {
        let area = f.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(3)].as_ref())
            .split(area);

        let narrow = area.width < LAYOUT_NARROW_MAX;
        if narrow {
            match self.active_area {
                ActiveArea::FlowList => self.render_flow_list(f, chunks[0]),
                ActiveArea::FlowDetail => self.render_flow_detail(f, chunks[0]),
            }
        } else {
            let (list_width, detail_width) = if area.width < 100 {
                (50, 50)
            } else if area.width < 140 {
                (40, 60)
            } else {
                (35, 65)
            };

            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(list_width),
                    Constraint::Percentage(detail_width),
                ])
                .split(chunks[0]);

            self.render_flow_list(f, main_chunks[0]);
            self.render_flow_detail(f, main_chunks[1]);
        }
        self.render_status_bar(f, chunks[1]);

        if self.input_mode == InputMode::Help {
            self.render_help_overlay(f);
        }
    }

    fn render_help_overlay(&self, f: &mut Frame) {
        let lines = vec![
            Line::from(vec![Span::styled(
                "Keyboard Shortcuts",
                Theme::accent_bold(),
            )]),
            Line::from(""),
            Line::from(vec![
                Span::styled(" j / ↓      ", Theme::label()),
                Span::styled("Move selection down", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" k / ↑      ", Theme::label()),
                Span::styled("Move selection up", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" g / End    ", Theme::label()),
                Span::styled("Jump to newest flow (detail: scroll to top)", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" G / Home   ", Theme::label()),
                Span::styled(
                    "Jump to oldest flow (detail: scroll to bottom)",
                    Theme::text(),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Ctrl+d     ", Theme::label()),
                Span::styled("Scroll detail panel down 10 lines", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" Ctrl+u     ", Theme::label()),
                Span::styled("Scroll detail panel up 10 lines", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" /          ", Theme::label()),
                Span::styled(
                    "Filter (host: path: method: status: err ws, or plain text)",
                    Theme::text(),
                ),
            ]),
            Line::from(vec![
                Span::styled(" Enter / →  ", Theme::label()),
                Span::styled("Focus detail panel", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" Esc / ←    ", Theme::label()),
                Span::styled("Focus flow list", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" Tab        ", Theme::label()),
                Span::styled(
                    "Switch detail tab (Overview→Request→Response→Messages)",
                    Theme::text(),
                ),
            ]),
            Line::from(vec![
                Span::styled(" 1-4        ", Theme::label()),
                Span::styled(
                    "Jump to tab 1=Overview 2=Request 3=Response 4=Messages",
                    Theme::text(),
                ),
            ]),
            Line::from(vec![
                Span::styled(" PgUp / PgDown ", Theme::label()),
                Span::styled("Scroll detail panel", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" ?          ", Theme::label()),
                Span::styled("Toggle this help", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" q          ", Theme::label()),
                Span::styled("Quit", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" d          ", Theme::label()),
                Span::styled("Delete selected flow", Theme::text()),
            ]),
            Line::from(vec![
                Span::styled(" y          ", Theme::label()),
                Span::styled("Copy selected flow as cURL", Theme::text()),
            ]),
        ];

        let help_width = 70;
        let help_height = lines.len() as u16 + 2;
        let area = centered_rect(help_width, help_height, f.area());

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Help (? to close) ")
            .style(Style::default().bg(Theme::BG_ELEVATED));
        let paragraph = Paragraph::new(lines).block(block);
        f.render_widget(Clear, area);
        f.render_widget(paragraph, area);
    }

    fn render_flow_list(&mut self, f: &mut Frame, area: Rect) {
        let filtered_flows = self.get_filtered_flows();
        let table_wide = area.width >= TABLE_WIDE_MIN;
        let filter = &self.filter_input;
        let filtering = !filter.is_empty();
        let path_budget = usize::from(area.width.saturating_sub(if table_wide { 52 } else { 28 }));

        let rows: Vec<Row> = filtered_flows
            .iter()
            .map(|flow| flow_table_row(flow, table_wide, path_budget, filter, filtering))
            .collect();

        let border_style = Theme::border(self.active_area == ActiveArea::FlowList);
        let title = flow_list_title(filter, filtered_flows.len(), self.flows.len());

        let (header, widths): (Vec<&str>, Vec<Constraint>) = if table_wide {
            (
                vec!["Method", "Code", "Dur", "Size", "Host", "Path"],
                vec![
                    Constraint::Length(6),
                    Constraint::Length(4),
                    Constraint::Length(7),
                    Constraint::Length(9),
                    Constraint::Length(18),
                    Constraint::Min(10),
                ],
            )
        } else {
            (
                vec!["Method", "Code", "Dur", "URL"],
                vec![
                    Constraint::Length(6),
                    Constraint::Length(4),
                    Constraint::Length(7),
                    Constraint::Min(10),
                ],
            )
        };

        let header_row = Row::new(header)
            .style(Theme::table_header())
            .bottom_margin(1);

        let table = Table::new(rows, widths)
            .header(header_row)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(title, Theme::section()))
                    .border_style(border_style),
            )
            .row_highlight_style(Theme::row_highlight())
            .highlight_symbol("▌ ");

        f.render_stateful_widget(table, area, &mut self.table_state);
    }

    fn render_flow_detail(&self, f: &mut Frame, area: Rect) {
        let border_style = Theme::border(self.active_area == ActiveArea::FlowDetail);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Detail ", Theme::section()))
            .border_style(border_style);
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)].as_ref())
            .split(inner);

        let titles = vec![
            DetailTab::Overview.tab_line(),
            DetailTab::Request.tab_line(),
            DetailTab::Response.tab_line(),
            DetailTab::Messages.tab_line(),
        ];

        let tabs = Tabs::new(titles)
            .highlight_style(Theme::accent())
            .select(self.detail_tab as usize);
        f.render_widget(block, area);
        f.render_widget(tabs, chunks[0]);

        let content_block = Block::default();
        let filtered_flows = self.get_filtered_flows();
        if let Some(selected) = self.table_state.selected() {
            if let Some(flow) = filtered_flows.get(selected) {
                match self.detail_tab {
                    DetailTab::Overview => self.render_overview(f, chunks[1], flow),
                    DetailTab::Request => self.render_request(f, chunks[1], flow),
                    DetailTab::Response => self.render_response(f, chunks[1], flow),
                    DetailTab::Messages => self.render_messages(f, chunks[1], flow),
                }
            } else {
                f.render_widget(
                    Paragraph::new("Flow not found").block(content_block),
                    chunks[1],
                );
            }
        } else {
            f.render_widget(
                Paragraph::new("Select a flow to view details").block(content_block),
                chunks[1],
            );
        }
    }

    fn render_overview(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let border_style = Theme::border(self.active_area == ActiveArea::FlowDetail);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Overview")
            .border_style(border_style);

        let text = match &flow.layer {
            Layer::Http(h) => {
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled("ID:       ", Theme::label()),
                        Span::styled(flow.id.to_string(), Theme::value()),
                    ]),
                    Line::from(vec![
                        Span::styled("URL:      ", Theme::label()),
                        Span::styled(h.request.url.to_string(), Theme::value()),
                    ]),
                    Line::from(vec![
                        Span::styled("Method:   ", Theme::label()),
                        Span::styled(&h.request.method, Theme::value()),
                    ]),
                    Line::from(vec![
                        Span::styled("Version:  ", Theme::label()),
                        Span::styled(&h.request.version, Theme::value()),
                    ]),
                ];
                if let Some(resp) = &h.response {
                    lines.push(Line::from(vec![
                        Span::styled("Status:   ", Theme::label()),
                        Span::styled(resp.status.to_string(), Theme::value()),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Reason:   ", Theme::label()),
                        Span::styled(&resp.status_text, Theme::value()),
                    ]));
                }
                if let Some(err) = &h.error {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        Span::styled("Error:    ", Theme::error_bold()),
                        Span::styled(err, Theme::error()),
                    ]));
                }
                lines
            }
            Layer::WebSocket(w) => vec![
                Line::from(vec![
                    Span::styled("ID:       ", Theme::label()),
                    Span::styled(flow.id.to_string(), Theme::value()),
                ]),
                Line::from(vec![
                    Span::styled("URL:      ", Theme::label()),
                    Span::styled(w.handshake_request.url.to_string(), Theme::value()),
                ]),
                Line::from(vec![
                    Span::styled("Type:     ", Theme::label()),
                    Span::styled("WebSocket", Theme::value()),
                ]),
            ],
            _ => vec![Line::from("Unknown Layer")],
        };

        let p = Paragraph::new(text)
            .block(block)
            .wrap(Wrap { trim: true })
            .scroll((self.detail_scroll, 0));
        f.render_widget(p, area);
    }

    fn render_request(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let border_style = Theme::border(self.active_area == ActiveArea::FlowDetail);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Request")
            .border_style(border_style);

        match &flow.layer {
            Layer::Http(h) => {
                let mut text = vec![Line::from(Span::styled("Headers:", Theme::section()))];
                for header in &h.request.headers {
                    text.push(Line::from(vec![
                        Span::styled(format!("  {}: ", header.0), Theme::header_key()),
                        Span::styled(&header.1, Theme::value()),
                    ]));
                }

                text.push(Line::from(""));
                text.push(Line::from(Span::styled("Body:", Theme::section())));

                if let Some(body) = &h.request.body {
                    text.push(Line::from(format!(
                        "  Size: {} bytes, Encoding: {}",
                        body.size, body.encoding
                    )));
                    // Basic body preview with JSON formatting
                    if body.size > 0 {
                        text.push(Line::from(""));

                        let content =
                            if let Ok(json_val) = serde_json::from_str::<Value>(&body.content) {
                                if let Ok(pretty) = serde_json::to_string_pretty(&json_val) {
                                    pretty
                                } else {
                                    body.content.clone()
                                }
                            } else {
                                body.content.clone()
                            };

                        for line in content.lines() {
                            text.push(Line::from(format!("  {}", line)));
                        }
                    }
                } else {
                    text.push(Line::from("  (No Body)"));
                }

                let p = Paragraph::new(text)
                    .block(block)
                    .wrap(Wrap { trim: true })
                    .scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            }
            Layer::WebSocket(w) => {
                let mut text = vec![Line::from(Span::styled(
                    "Handshake Request Headers:",
                    Theme::section(),
                ))];
                for header in &w.handshake_request.headers {
                    text.push(Line::from(vec![
                        Span::styled(format!("  {}: ", header.0), Theme::header_key()),
                        Span::styled(&header.1, Theme::value()),
                    ]));
                }
                let p = Paragraph::new(text)
                    .block(block)
                    .wrap(Wrap { trim: true })
                    .scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            }
            _ => f.render_widget(Paragraph::new("N/A").block(block), area),
        }
    }

    fn render_response(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let border_style = Theme::border(self.active_area == ActiveArea::FlowDetail);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Response")
            .border_style(border_style);

        match &flow.layer {
            Layer::Http(h) => {
                if let Some(resp) = &h.response {
                    let mut text = vec![Line::from(Span::styled("Headers:", Theme::section()))];
                    for header in &resp.headers {
                        text.push(Line::from(vec![
                            Span::styled(format!("  {}: ", header.0), Theme::header_key()),
                            Span::styled(&header.1, Theme::value()),
                        ]));
                    }

                    text.push(Line::from(""));
                    text.push(Line::from(Span::styled("Body:", Theme::section())));

                    if let Some(body) = &resp.body {
                        text.push(Line::from(format!(
                            "  Size: {} bytes, Encoding: {}",
                            body.size, body.encoding
                        )));
                        if body.size > 0 {
                            text.push(Line::from(""));

                            let content = if let Ok(json_val) =
                                serde_json::from_str::<Value>(&body.content)
                            {
                                if let Ok(pretty) = serde_json::to_string_pretty(&json_val) {
                                    pretty
                                } else {
                                    body.content.clone()
                                }
                            } else {
                                body.content.clone()
                            };

                            for line in content.lines() {
                                text.push(Line::from(format!("  {}", line)));
                            }
                        }
                    } else {
                        text.push(Line::from("  (No Body)"));
                    }

                    let p = Paragraph::new(text)
                        .block(block)
                        .wrap(Wrap { trim: true })
                        .scroll((self.detail_scroll, 0));
                    f.render_widget(p, area);
                } else {
                    f.render_widget(Paragraph::new("Waiting for response...").block(block), area);
                }
            }
            Layer::WebSocket(w) => {
                let mut text = vec![Line::from(Span::styled(
                    "Handshake Response Headers:",
                    Theme::section(),
                ))];
                for header in &w.handshake_response.headers {
                    text.push(Line::from(vec![
                        Span::styled(format!("  {}: ", header.0), Theme::header_key()),
                        Span::styled(&header.1, Theme::value()),
                    ]));
                }
                let p = Paragraph::new(text)
                    .block(block)
                    .wrap(Wrap { trim: true })
                    .scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            }
            _ => f.render_widget(Paragraph::new("N/A").block(block), area),
        }
    }

    fn render_messages(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let border_style = Theme::border(self.active_area == ActiveArea::FlowDetail);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Messages")
            .border_style(border_style);

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
                    .scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            }
            _ => f.render_widget(Paragraph::new("Not a WebSocket flow").block(block), area),
        }
    }

    fn render_status_bar(&self, f: &mut Frame, area: Rect) {
        let flow_count = self.flows.len();
        let filtered_count = self.get_filtered_flows().len();
        let count_str = if filtered_count != flow_count {
            format!("{}/{}", filtered_count, flow_count)
        } else {
            flow_count.to_string()
        };
        let elapsed = self.start_time.elapsed();
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

        let bar_text = match self.input_mode {
            InputMode::Normal => {
                let active = match self.active_area {
                    ActiveArea::FlowList => "LIST",
                    ActiveArea::FlowDetail => "DETAIL",
                };
                let mut spans = vec![
                    Span::styled("Flows: ", Theme::label()),
                    Span::styled(format!("{} ", count_str), Theme::stat_ok()),
                    Span::styled("| ", Theme::muted()),
                    Span::styled("Total: ", Theme::label()),
                    Span::styled(format!("{} ", self.flow_count_total), Theme::text()),
                    Span::styled("| ", Theme::muted()),
                    Span::styled("Up: ", Theme::label()),
                    Span::styled(format!("{} ", uptime), Theme::uptime()),
                    Span::styled("| ", Theme::muted()),
                    Span::styled("Port: ", Theme::label()),
                    Span::styled(format!("{} ", self.proxy_port), Theme::stat_info()),
                    Span::styled("| ", Theme::muted()),
                    Span::styled(format!("[{}] ", active), Theme::accent_bold()),
                    Span::styled("| ", Theme::muted()),
                    Span::styled("q", Theme::hotkey()),
                    Span::styled(" quit | ", Theme::muted()),
                    Span::styled("/", Theme::hotkey()),
                    Span::styled(" filter | ", Theme::muted()),
                    Span::styled("?", Theme::hotkey()),
                    Span::styled(" help | ", Theme::muted()),
                    Span::styled("y", Theme::hotkey()),
                    Span::styled(" cURL", Theme::muted()),
                ];
                if let Some(ref msg) = self.toast {
                    spans.push(Span::styled(" | ", Theme::muted()));
                    spans.push(Span::styled(msg.as_str(), Theme::accent()));
                }
                spans
            }
            InputMode::Filtering => {
                vec![
                    Span::styled("Filter: ", Theme::label()),
                    Span::styled(self.filter_input.as_str(), Theme::accent()),
                    Span::styled(" | ", Theme::muted()),
                    Span::styled("Enter", Theme::hotkey()),
                    Span::styled(" apply | ", Theme::muted()),
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
        let paragraph = Paragraph::new(text).block(Block::default().borders(Borders::ALL));
        f.render_widget(paragraph, area);
    }
}

fn flow_table_row(
    flow: &Flow,
    table_wide: bool,
    path_budget: usize,
    filter: &str,
    filtering: bool,
) -> Row<'static> {
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
    let method_color = Theme::method(method_label.trim_end_matches('+'));
    let status_color = Theme::status(&status);
    let dur_ms = flow_duration_ms(flow);
    let dur_label = format_duration_ms(dur_ms);
    let dur_style = dur_ms
        .map(|ms| Style::default().fg(Theme::duration_color(ms)))
        .unwrap_or(Theme::muted());

    let method_cell = Cell::from(Span::styled(
        method_label,
        Style::default().fg(method_color),
    ));
    let status_cell = Cell::from(Span::styled(
        status.clone(),
        Style::default().fg(status_color),
    ));
    let dur_cell = Cell::from(Span::styled(dur_label, dur_style));

    if table_wide {
        let host = host_from_url(&url);
        let path = display_path(&url, path_budget, has_query);
        Row::new(vec![
            method_cell,
            status_cell,
            dur_cell,
            Cell::from(size_str),
            styled_text_cell(&host, filter, filtering, Theme::muted()),
            styled_text_cell(&path, filter, filtering, Theme::text()),
        ])
    } else {
        let url_text = smart_truncate(url.as_str(), path_budget);
        Row::new(vec![
            method_cell,
            status_cell,
            dur_cell,
            styled_text_cell(&url_text, filter, filtering, Theme::text()),
        ])
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

#[cfg(test)]
mod tests {
    use super::*;
    use relay_core_api::flow::{
        Flow, HttpLayer, HttpRequest, Layer, NetworkInfo, TransportProtocol,
    };
    use std::collections::HashMap;
    use url::Url;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_repeat(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Repeat)
    }

    fn make_http_flow(id: &str, url_str: &str, method: &str) -> Flow {
        Flow {
            id: uuid::Uuid::parse_str(id).unwrap(),
            start_time: chrono::Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".into(),
                client_port: 12345,
                server_ip: "93.184.216.34".into(),
                server_port: 443,
                protocol: TransportProtocol::TCP,
                tls: true,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: method.into(),
                    url: Url::parse(url_str).unwrap(),
                    version: "HTTP/1.1".into(),
                    headers: vec![],
                    cookies: vec![],
                    query: vec![],
                    body: None,
                },
                response: None,
                error: None,
            }),
            tags: vec![],
            meta: HashMap::new(),
        }
    }

    #[test]
    fn test_new_app_has_selection() {
        let app = TuiApp::new(8080);
        assert_eq!(app.table_state.selected(), Some(0));
        assert_eq!(app.detail_tab, DetailTab::Overview);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.active_area, ActiveArea::FlowList);
        assert!(app.auto_scroll);
    }

    #[test]
    fn test_next_previous_wraps_around() {
        let mut app = TuiApp::new(8080);
        for i in 0..5 {
            app.flows.push_back(make_http_flow(
                &format!("00000000-0000-0000-0000-00000000000{i}"),
                &format!("http://example.com/{i}"),
                "GET",
            ));
        }

        assert_eq!(app.table_state.selected(), Some(0));
        for _ in 0..5 {
            app.next();
        }
        assert_eq!(app.table_state.selected(), Some(0));

        app.previous();
        assert_eq!(app.table_state.selected(), Some(4));
        app.previous();
        assert_eq!(app.table_state.selected(), Some(3));
    }

    #[test]
    fn test_filtering_filters_by_url_and_method() {
        let mut app = TuiApp::new(8080);
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000001",
            "http://api.example.com/users",
            "GET",
        ));
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000002",
            "http://example.com/admin",
            "POST",
        ));
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000003",
            "http://api.example.com/items",
            "GET",
        ));

        assert_eq!(app.get_filtered_flows().len(), 3);

        app.filter_input = "api".into();
        assert_eq!(app.get_filtered_flows().len(), 2);

        app.filter_input = "admin".into();
        assert_eq!(app.get_filtered_flows().len(), 1);

        app.filter_input = "POST".into();
        assert_eq!(app.get_filtered_flows().len(), 1);

        app.filter_input = "nonexistent".into();
        assert_eq!(app.get_filtered_flows().len(), 0);

        app.filter_input = "host:api.example".into();
        assert_eq!(app.get_filtered_flows().len(), 2);

        app.filter_input = "method:POST".into();
        assert_eq!(app.get_filtered_flows().len(), 1);
    }

    #[test]
    fn test_filtering_plain_text_case_insensitive() {
        let mut app = TuiApp::new(8080);
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000001",
            "http://API.Example.com/x",
            "GET",
        ));
        app.filter_input = "api.example".into();
        assert_eq!(app.get_filtered_flows().len(), 1);
    }

    #[test]
    fn test_on_flow_updates_existing_and_adds_new() {
        let mut app = TuiApp::new(8080);
        let id = "00000000-0000-0000-0000-000000000001";

        let flow1 = make_http_flow(id, "http://example.com/original", "GET");
        app.on_flow(flow1.clone());
        assert_eq!(app.flows.len(), 1);
        assert_eq!(app.flow_count_total, 1);

        let mut flow2 = flow1.clone();
        if let Layer::Http(ref mut h) = flow2.layer {
            h.request.method = "POST".into();
        }
        app.on_flow(flow2);
        assert_eq!(app.flows.len(), 1);
        assert_eq!(app.flow_count_total, 1);
    }

    #[test]
    fn test_on_flow_caps_at_1000() {
        let mut app = TuiApp::new(8080);
        for i in 0..1100 {
            app.on_flow(make_http_flow(
                &format!("00000000-0000-0000-0000-{i:012x}"),
                &format!("http://example.com/{i}"),
                "GET",
            ));
        }
        assert_eq!(app.flows.len(), 1000);
        assert_eq!(app.flow_count_total, 1100);
    }

    #[test]
    fn test_key_quit() {
        let mut app = TuiApp::new(8080);
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_detail_tab_cycle() {
        let mut app = TuiApp::new(8080);
        assert_eq!(app.detail_tab, DetailTab::Overview);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.detail_tab, DetailTab::Request);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.detail_tab, DetailTab::Response);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.detail_tab, DetailTab::Messages);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.detail_tab, DetailTab::Overview);
    }

    #[test]
    fn test_help_stays_open_on_key_repeat() {
        let mut app = TuiApp::new(8080);
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Help);
        app.on_key(key_repeat(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Help);
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn test_filter_mode_toggle() {
        let mut app = TuiApp::new(8080);
        assert_eq!(app.input_mode, InputMode::Normal);
        app.on_key(key(KeyCode::Char('/')));
        assert_eq!(app.input_mode, InputMode::Filtering);
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('p')));
        app.on_key(key(KeyCode::Char('i')));
        assert_eq!(app.filter_input, "api");
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn test_copy_curl_sets_toast() {
        let mut app = TuiApp::new(8080);
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000001",
            "http://api.example.com/v1",
            "GET",
        ));
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.toast.is_some());
        assert!(
            app.toast
                .as_deref()
                .unwrap()
                .to_lowercase()
                .contains("curl")
        );
    }

    #[test]
    fn test_narrow_layout_switches_pane_on_enter_esc() {
        let mut app = TuiApp::new(8080);
        assert_eq!(app.active_area, ActiveArea::FlowList);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.active_area, ActiveArea::FlowDetail);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.active_area, ActiveArea::FlowList);
    }

    #[test]
    fn test_keyboard_navigation_moves_selection() {
        let mut app = TuiApp::new(8080);
        for i in 0..10 {
            app.flows.push_back(make_http_flow(
                &format!("00000000-0000-0000-0000-00000000000{i}"),
                &format!("http://example.com/{i}"),
                "GET",
            ));
        }
        app.table_state.select(Some(0));

        app.on_key(key(KeyCode::Down));
        assert_eq!(app.table_state.selected(), Some(1));

        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.table_state.selected(), Some(2));

        app.on_key(key(KeyCode::Up));
        assert_eq!(app.table_state.selected(), Some(1));

        app.on_key(key(KeyCode::Char('k')));
        assert_eq!(app.table_state.selected(), Some(0));
    }
}
