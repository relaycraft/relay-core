use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap},
};
use relay_core_api::flow::{Flow, Layer};
use relay_core_api::modification::{flow_matches_filter, parse_flow_filter};
use serde_json::Value;
use std::collections::VecDeque;

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

    fn title(&self) -> &str {
        match self {
            Self::Overview => "Overview (1)",
            Self::Request => "Request (2)",
            Self::Response => "Response (3)",
            Self::Messages => "Messages (4)",
        }
    }
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

    pub fn on_key(&mut self, event: KeyEvent) {
        // Ignore Repeat/Release — e.g. `?` Press opens Help, Repeat would instantly close it.
        if event.kind != KeyEventKind::Press {
            return;
        }
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
        self.render_status_bar(f, chunks[1]);

        if self.input_mode == InputMode::Help {
            self.render_help_overlay(f);
        }
    }

    fn render_help_overlay(&self, f: &mut Frame) {
        let lines = vec![
            Line::from(vec![Span::styled(
                "Keyboard Shortcuts",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Yellow),
            )]),
            Line::from(""),
            Line::from(vec![
                Span::styled(" j / ↓      ", Style::default().fg(Color::Cyan)),
                Span::raw("Move selection down"),
            ]),
            Line::from(vec![
                Span::styled(" k / ↑      ", Style::default().fg(Color::Cyan)),
                Span::raw("Move selection up"),
            ]),
            Line::from(vec![
                Span::styled(" g / End    ", Style::default().fg(Color::Cyan)),
                Span::raw("Jump to newest flow (detail: scroll to top)"),
            ]),
            Line::from(vec![
                Span::styled(" G / Home   ", Style::default().fg(Color::Cyan)),
                Span::raw("Jump to oldest flow (detail: scroll to bottom)"),
            ]),
            Line::from(vec![
                Span::styled(" Ctrl+d     ", Style::default().fg(Color::Cyan)),
                Span::raw("Scroll detail panel down 10 lines"),
            ]),
            Line::from(vec![
                Span::styled(" Ctrl+u     ", Style::default().fg(Color::Cyan)),
                Span::raw("Scroll detail panel up 10 lines"),
            ]),
            Line::from(vec![
                Span::styled(" /          ", Style::default().fg(Color::Cyan)),
                Span::raw("Filter (host: path: method: status: err ws, or plain text)"),
            ]),
            Line::from(vec![
                Span::styled(" Enter / →  ", Style::default().fg(Color::Cyan)),
                Span::raw("Focus detail panel"),
            ]),
            Line::from(vec![
                Span::styled(" Esc / ←    ", Style::default().fg(Color::Cyan)),
                Span::raw("Focus flow list"),
            ]),
            Line::from(vec![
                Span::styled(" Tab        ", Style::default().fg(Color::Cyan)),
                Span::raw("Switch detail tab (Overview→Request→Response→Messages)"),
            ]),
            Line::from(vec![
                Span::styled(" 1-4        ", Style::default().fg(Color::Cyan)),
                Span::raw("Jump to tab 1=Overview 2=Request 3=Response 4=Messages"),
            ]),
            Line::from(vec![
                Span::styled(" PgUp / PgDown ", Style::default().fg(Color::Cyan)),
                Span::raw("Scroll detail panel"),
            ]),
            Line::from(vec![
                Span::styled(" ?          ", Style::default().fg(Color::Cyan)),
                Span::raw("Toggle this help"),
            ]),
            Line::from(vec![
                Span::styled(" q          ", Style::default().fg(Color::Cyan)),
                Span::raw("Quit"),
            ]),
            Line::from(vec![
                Span::styled(" d          ", Style::default().fg(Color::Cyan)),
                Span::raw("Delete selected flow"),
            ]),
        ];

        let help_width = 70;
        let help_height = lines.len() as u16 + 2;
        let area = centered_rect(help_width, help_height, f.area());

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Help (? to close) ")
            .style(Style::default().bg(Color::Rgb(20, 20, 30)));
        let paragraph = Paragraph::new(lines).block(block);
        f.render_widget(Clear, area);
        f.render_widget(paragraph, area);
    }

    fn render_flow_list(&mut self, f: &mut Frame, area: Rect) {
        let filtered_flows = self.get_filtered_flows();
        let wide = area.width >= 120;
        let filter = &self.filter_input;
        let filtering = !filter.is_empty();

        let rows: Vec<Row> = filtered_flows
            .iter()
            .map(|flow| {
                let (method, url, status, size_str) = match &flow.layer {
                    Layer::Http(h) => {
                        let size = h
                            .response
                            .as_ref()
                            .and_then(|r| r.body.as_ref())
                            .map(|b| b.size)
                            .unwrap_or(0);
                        (
                            h.request.method.clone(),
                            h.request.url.to_string(),
                            if let Some(resp) = &h.response {
                                resp.status.to_string()
                            } else {
                                "---".to_string()
                            },
                            if size > 0 {
                                format_size(size)
                            } else {
                                String::new()
                            },
                        )
                    }
                    Layer::WebSocket(w) => (
                        "WS".to_string(),
                        w.handshake_request.url.to_string(),
                        w.handshake_response.status.to_string(),
                        String::new(),
                    ),
                    _ => (
                        "UNKNOWN".to_string(),
                        "Unknown".to_string(),
                        "---".to_string(),
                        String::new(),
                    ),
                };

                let method_color = match method.as_str() {
                    "GET" => Color::Blue,
                    "POST" => Color::Green,
                    "PUT" => Color::Yellow,
                    "DELETE" => Color::Red,
                    "PATCH" => Color::Cyan,
                    "HEAD" => Color::Magenta,
                    "OPTIONS" => Color::White,
                    "WS" => Color::Magenta,
                    _ => Color::Gray,
                };

                let status_color = if status.starts_with('2') {
                    Color::Green
                } else if status.starts_with('3') {
                    Color::Yellow
                } else if status.starts_with('4') || status.starts_with('5') {
                    Color::Red
                } else {
                    Color::Gray
                };

                let url_cell = if filtering {
                    let lower_url = url.to_lowercase();
                    let lower_filter = filter.to_lowercase();
                    let mut spans = Vec::new();
                    let mut last = 0;
                    for (idx, _) in lower_url.match_indices(&lower_filter) {
                        if idx > last {
                            spans.push(Span::raw(url[last..idx].to_string()));
                        }
                        spans.push(Span::styled(
                            url[idx..idx + filter.len()].to_string(),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ));
                        last = idx + filter.len();
                    }
                    if last < url.len() {
                        spans.push(Span::raw(url[last..].to_string()));
                    }
                    Cell::from(Line::from(spans))
                } else {
                    Cell::from(url)
                };

                if wide {
                    Row::new(vec![
                        Cell::from(Span::styled(method, Style::default().fg(method_color))),
                        Cell::from(Span::styled(status, Style::default().fg(status_color))),
                        Cell::from(size_str),
                        url_cell,
                    ])
                } else {
                    Row::new(vec![
                        Cell::from(Span::styled(method, Style::default().fg(method_color))),
                        Cell::from(Span::styled(status, Style::default().fg(status_color))),
                        url_cell,
                    ])
                }
            })
            .collect();

        let border_style = if self.active_area == ActiveArea::FlowList {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };

        let (header, widths): (Vec<&str>, Vec<Constraint>) = if wide {
            (
                vec!["Method", "Code", "Size", "URL"],
                vec![
                    Constraint::Length(8),
                    Constraint::Length(5),
                    Constraint::Length(9),
                    Constraint::Min(10),
                ],
            )
        } else {
            (
                vec!["Method", "Code", "URL"],
                vec![
                    Constraint::Length(8),
                    Constraint::Length(5),
                    Constraint::Min(10),
                ],
            )
        };

        let header_row = Row::new(header)
            .style(Style::default().add_modifier(Modifier::BOLD))
            .bottom_margin(1);

        let table = Table::new(rows, widths)
            .header(header_row)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Flows")
                    .border_style(border_style),
            )
            .row_highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("► ");

        f.render_stateful_widget(table, area, &mut self.table_state);
    }

    fn render_flow_detail(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
            .split(area);

        // Render Tabs
        let titles = vec![
            DetailTab::Overview.title(),
            DetailTab::Request.title(),
            DetailTab::Response.title(),
            DetailTab::Messages.title(),
        ];

        let tabs = Tabs::new(titles)
            .block(Block::default().borders(Borders::ALL).title("Detail"))
            .highlight_style(Style::default().fg(Color::Yellow))
            .select(self.detail_tab as usize);
        f.render_widget(tabs, chunks[0]);

        // Render Content
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
                    Paragraph::new("Flow not found").block(Block::default().borders(Borders::ALL)),
                    chunks[1],
                );
            }
        } else {
            f.render_widget(
                Paragraph::new("Select a flow to view details")
                    .block(Block::default().borders(Borders::ALL)),
                chunks[1],
            );
        }
    }

    fn render_overview(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let border_style = if self.active_area == ActiveArea::FlowDetail {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Overview")
            .border_style(border_style);

        let text = match &flow.layer {
            Layer::Http(h) => {
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled("ID:       ", Style::default().fg(Color::Cyan)),
                        Span::raw(flow.id.to_string()),
                    ]),
                    Line::from(vec![
                        Span::styled("URL:      ", Style::default().fg(Color::Cyan)),
                        Span::raw(h.request.url.to_string()),
                    ]),
                    Line::from(vec![
                        Span::styled("Method:   ", Style::default().fg(Color::Cyan)),
                        Span::raw(&h.request.method),
                    ]),
                    Line::from(vec![
                        Span::styled("Version:  ", Style::default().fg(Color::Cyan)),
                        Span::raw(&h.request.version),
                    ]),
                ];
                if let Some(resp) = &h.response {
                    lines.push(Line::from(vec![
                        Span::styled("Status:   ", Style::default().fg(Color::Cyan)),
                        Span::raw(resp.status.to_string()),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Reason:   ", Style::default().fg(Color::Cyan)),
                        Span::raw(&resp.status_text),
                    ]));
                }
                if let Some(err) = &h.error {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        Span::styled(
                            "Error:    ",
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(err, Style::default().fg(Color::Red)),
                    ]));
                }
                lines
            }
            Layer::WebSocket(w) => vec![
                Line::from(vec![
                    Span::styled("ID:       ", Style::default().fg(Color::Cyan)),
                    Span::raw(flow.id.to_string()),
                ]),
                Line::from(vec![
                    Span::styled("URL:      ", Style::default().fg(Color::Cyan)),
                    Span::raw(w.handshake_request.url.to_string()),
                ]),
                Line::from(vec![
                    Span::styled("Type:     ", Style::default().fg(Color::Cyan)),
                    Span::raw("WebSocket"),
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
        let border_style = if self.active_area == ActiveArea::FlowDetail {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Request")
            .border_style(border_style);

        match &flow.layer {
            Layer::Http(h) => {
                let mut text = vec![Line::from(Span::styled(
                    "Headers:",
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(Color::Blue),
                ))];
                for header in &h.request.headers {
                    text.push(Line::from(vec![
                        Span::styled(
                            format!("  {}: ", header.0),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(&header.1),
                    ]));
                }

                text.push(Line::from(""));
                text.push(Line::from(Span::styled(
                    "Body:",
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(Color::Blue),
                )));

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
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(Color::Blue),
                ))];
                for header in &w.handshake_request.headers {
                    text.push(Line::from(vec![
                        Span::styled(
                            format!("  {}: ", header.0),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(&header.1),
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
        let border_style = if self.active_area == ActiveArea::FlowDetail {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Response")
            .border_style(border_style);

        match &flow.layer {
            Layer::Http(h) => {
                if let Some(resp) = &h.response {
                    let mut text = vec![Line::from(Span::styled(
                        "Headers:",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .fg(Color::Blue),
                    ))];
                    for header in &resp.headers {
                        text.push(Line::from(vec![
                            Span::styled(
                                format!("  {}: ", header.0),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::raw(&header.1),
                        ]));
                    }

                    text.push(Line::from(""));
                    text.push(Line::from(Span::styled(
                        "Body:",
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .fg(Color::Blue),
                    )));

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
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(Color::Blue),
                ))];
                for header in &w.handshake_response.headers {
                    text.push(Line::from(vec![
                        Span::styled(
                            format!("  {}: ", header.0),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::raw(&header.1),
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
        let border_style = if self.active_area == ActiveArea::FlowDetail {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
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
                        let color = match msg.direction {
                            relay_core_api::flow::Direction::ClientToServer => Color::Green,
                            relay_core_api::flow::Direction::ServerToClient => Color::Blue,
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
                            Span::styled(format!("{} ", direction), Style::default().fg(color)),
                            Span::styled(
                                format!("[{}] ", msg.opcode),
                                Style::default().fg(Color::Yellow),
                            ),
                            Span::raw(content),
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
                vec![
                    Span::styled(
                        format!("Flows: {} ", count_str),
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw("| "),
                    Span::styled(
                        format!("Total: {} ", self.flow_count_total),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw("| "),
                    Span::styled(
                        format!("Up: {} ", uptime),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw("| "),
                    Span::styled(
                        format!("Port: {} ", self.proxy_port),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw("| "),
                    Span::styled(
                        format!("[{}] ", active),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("| "),
                    Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" quit | "),
                    Span::styled("/", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" filter | "),
                    Span::styled("?", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" help"),
                ]
            }
            InputMode::Filtering => {
                vec![
                    Span::raw("Filter: "),
                    Span::styled(
                        self.filter_input.as_str(),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(" | "),
                    Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" apply | "),
                    Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" cancel"),
                ]
            }
            InputMode::Help => {
                vec![
                    Span::styled(
                        "HELP",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" — press "),
                    Span::styled("? or Esc", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" to close"),
                ]
            }
        };

        let text = Text::from(Line::from(bar_text));
        let paragraph = Paragraph::new(text).block(Block::default().borders(Borders::ALL));
        f.render_widget(paragraph, area);
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    }
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
