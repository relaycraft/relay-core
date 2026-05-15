use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap, Tabs, Table, Row, TableState, Cell, Clear},
    Frame,
};
use crossterm::event::KeyCode;
use relay_core_api::flow::{Flow, Layer};
use std::collections::VecDeque;
use serde_json::Value;

#[derive(Clone, Copy, PartialEq)]
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

#[derive(PartialEq)]
pub enum InputMode {
    Normal,
    Filtering,
    Help,
}

#[derive(PartialEq)]
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
}

impl TuiApp {
    pub fn new() -> Self {
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
        };
        app.table_state.select(Some(0));
        app
    }

    pub fn on_flow(&mut self, flow: Flow) {
        // Update existing flow or add new one
        if let Some(pos) = self.flows.iter().position(|f| f.id == flow.id) {
            self.flows[pos] = flow;
        } else {
            self.flows.push_front(flow);
            if self.flows.len() > 1000 {
                self.flows.pop_back();
            }
            
            // Auto-scroll logic: if we are at the top (index 0) and auto_scroll is enabled
            // Actually, we prepend flows, so index 0 is always the newest.
            // If the user has selected index 0, we want to keep them on the newest flow?
            // Or if they haven't moved selection, keep it at 0.
            if self.auto_scroll {
                self.table_state.select(Some(0));
            }
        }
    }

    fn get_filtered_flows(&self) -> Vec<&Flow> {
        if self.filter_input.is_empty() {
            self.flows.iter().collect()
        } else {
            self.flows.iter()
                .filter(|flow| {
                    match &flow.layer {
                        Layer::Http(h) => {
                            h.request.url.to_string().contains(&self.filter_input) ||
                            h.request.method.contains(&self.filter_input)
                        }
                        Layer::WebSocket(w) => {
                            w.handshake_request.url.to_string().contains(&self.filter_input)
                        }
                        _ => false,
                    }
                })
                .collect()
        }
    }

    pub fn on_key(&mut self, key: KeyCode) {
        match self.input_mode {
            InputMode::Normal => {
                if key == KeyCode::Char('?') {
                    self.input_mode = InputMode::Help;
                    return;
                }
                match self.active_area {
                    ActiveArea::FlowList => match key {
                        KeyCode::Char('q') => self.should_quit = true,
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.next();
                            self.auto_scroll = false; // User moved manually
                            self.detail_scroll = 0;
                        },
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.previous();
                            self.auto_scroll = false; // User moved manually
                            self.detail_scroll = 0;
                        },
                        KeyCode::Char('g') => { // Go to top (newest)
                            self.table_state.select(Some(0));
                            self.auto_scroll = true;
                            self.detail_scroll = 0;
                        },
                        KeyCode::Tab => {
                            self.detail_tab = self.detail_tab.next();
                            self.detail_scroll = 0;
                        },
                        KeyCode::Char('1') => self.detail_tab = DetailTab::Overview,
                        KeyCode::Char('2') => self.detail_tab = DetailTab::Request,
                        KeyCode::Char('3') => self.detail_tab = DetailTab::Response,
                        KeyCode::Char('4') => self.detail_tab = DetailTab::Messages,
                        KeyCode::Char('/') => self.input_mode = InputMode::Filtering,
                        KeyCode::Char('?') => self.input_mode = InputMode::Help,
                        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.active_area = ActiveArea::FlowDetail,
                        _ => {}
                    },
                    ActiveArea::FlowDetail => match key {
                        KeyCode::Char('q') => self.should_quit = true,
                        KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => self.active_area = ActiveArea::FlowList,
                        KeyCode::Down | KeyCode::Char('j') => self.detail_scroll = self.detail_scroll.saturating_add(1),
                        KeyCode::Up | KeyCode::Char('k') => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                        KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(10),
                        KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(10),
                        KeyCode::Tab => {
                            self.detail_tab = self.detail_tab.next();
                            self.detail_scroll = 0;
                        },
                        KeyCode::Char('1') => self.detail_tab = DetailTab::Overview,
                        KeyCode::Char('2') => self.detail_tab = DetailTab::Request,
                        KeyCode::Char('3') => self.detail_tab = DetailTab::Response,
                        KeyCode::Char('4') => self.detail_tab = DetailTab::Messages,
                        _ => {}
                    }
                }
            },
            InputMode::Filtering => match key {
                KeyCode::Enter | KeyCode::Esc => self.input_mode = InputMode::Normal,
                KeyCode::Char('?') => self.input_mode = InputMode::Help,
                KeyCode::Char(c) => self.filter_input.push(c),
                KeyCode::Backspace => { self.filter_input.pop(); },
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

    pub fn ui(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(3), // Filter/Status bar
            ].as_ref())
            .split(f.area());

        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
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
            Line::from(vec![Span::styled("Keyboard Shortcuts", Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow))]),
            Line::from(""),
            Line::from(vec![Span::styled(" j / ↓      ", Style::default().fg(Color::Cyan)), Span::raw("Navigate flow list down")]),
            Line::from(vec![Span::styled(" k / ↑      ", Style::default().fg(Color::Cyan)), Span::raw("Navigate flow list up")]),
            Line::from(vec![Span::styled(" g          ", Style::default().fg(Color::Cyan)), Span::raw("Jump to newest flow")]),
            Line::from(vec![Span::styled(" /          ", Style::default().fg(Color::Cyan)), Span::raw("Filter flows by host/URL/method")]),
            Line::from(vec![Span::styled(" Enter / l  ", Style::default().fg(Color::Cyan)), Span::raw("Focus detail panel")]),
            Line::from(vec![Span::styled(" Esc / h    ", Style::default().fg(Color::Cyan)), Span::raw("Focus flow list")]),
            Line::from(vec![Span::styled(" Tab        ", Style::default().fg(Color::Cyan)), Span::raw("Switch detail tab (Overview→Request→Response→Messages)")]),
            Line::from(vec![Span::styled(" 1-4        ", Style::default().fg(Color::Cyan)), Span::raw("Jump to tab 1=Overview 2=Request 3=Response 4=Messages")]),
            Line::from(vec![Span::styled(" PgUp / PgDown ", Style::default().fg(Color::Cyan)), Span::raw("Scroll detail panel")]),
            Line::from(vec![Span::styled(" ?          ", Style::default().fg(Color::Cyan)), Span::raw("Toggle this help")]),
            Line::from(vec![Span::styled(" q          ", Style::default().fg(Color::Cyan)), Span::raw("Quit")]),
        ];

        let help_width = 65;
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
        
        let rows: Vec<Row> = filtered_flows
            .iter()
            .map(|flow| {
                let (method, url, status) = match &flow.layer {
                    Layer::Http(h) => (
                        h.request.method.clone(),
                        h.request.url.to_string(),
                        if let Some(resp) = &h.response {
                            resp.status.to_string()
                        } else {
                            "---".to_string()
                        },
                    ),
                    Layer::WebSocket(w) => (
                        "WS".to_string(),
                        w.handshake_request.url.to_string(),
                        w.handshake_response.status.to_string(),
                    ),
                    _ => ("UNKNOWN".to_string(), "Unknown".to_string(), "---".to_string()),
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

                Row::new(vec![
                    Cell::from(Span::styled(method, Style::default().fg(method_color))),
                    Cell::from(Span::styled(status, Style::default().fg(status_color))),
                    Cell::from(url),
                ])
            })
            .collect();

        let border_style = if self.active_area == ActiveArea::FlowList {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };

        let table = Table::new(rows, [
                Constraint::Length(8),  // Method
                Constraint::Length(5),  // Status
                Constraint::Min(10),    // URL
            ])
            .header(
                Row::new(vec!["Method", "Code", "URL"])
                    .style(Style::default().add_modifier(Modifier::BOLD))
                    .bottom_margin(1)
            )
            .block(Block::default().borders(Borders::ALL).title("Flows").border_style(border_style))
            .row_highlight_style(Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray))
            .highlight_symbol("> ");

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
                f.render_widget(Paragraph::new("Flow not found").block(Block::default().borders(Borders::ALL)), chunks[1]);
            }
        } else {
            f.render_widget(Paragraph::new("Select a flow to view details").block(Block::default().borders(Borders::ALL)), chunks[1]);
        }
    }

    fn render_overview(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let border_style = if self.active_area == ActiveArea::FlowDetail {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let block = Block::default().borders(Borders::ALL).title("Overview").border_style(border_style);
        
        let text = match &flow.layer {
            Layer::Http(h) => {
                let mut lines = vec![
                    Line::from(vec![Span::styled("ID:       ", Style::default().fg(Color::Cyan)), Span::raw(flow.id.to_string())]),
                    Line::from(vec![Span::styled("URL:      ", Style::default().fg(Color::Cyan)), Span::raw(h.request.url.to_string())]),
                    Line::from(vec![Span::styled("Method:   ", Style::default().fg(Color::Cyan)), Span::raw(&h.request.method)]),
                    Line::from(vec![Span::styled("Version:  ", Style::default().fg(Color::Cyan)), Span::raw(&h.request.version)]),
                ];
                if let Some(resp) = &h.response {
                    lines.push(Line::from(vec![Span::styled("Status:   ", Style::default().fg(Color::Cyan)), Span::raw(resp.status.to_string())]));
                    lines.push(Line::from(vec![Span::styled("Reason:   ", Style::default().fg(Color::Cyan)), Span::raw(&resp.status_text)]));
                }
                if let Some(err) = &h.error {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![
                        Span::styled("Error:    ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                        Span::styled(err, Style::default().fg(Color::Red)),
                    ]));
                }
                lines
            },
            Layer::WebSocket(w) => vec![
                Line::from(vec![Span::styled("ID:       ", Style::default().fg(Color::Cyan)), Span::raw(flow.id.to_string())]),
                Line::from(vec![Span::styled("URL:      ", Style::default().fg(Color::Cyan)), Span::raw(w.handshake_request.url.to_string())]),
                Line::from(vec![Span::styled("Type:     ", Style::default().fg(Color::Cyan)), Span::raw("WebSocket")]),
            ],
            _ => vec![Line::from("Unknown Layer")],
        };

        let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true }).scroll((self.detail_scroll, 0));
        f.render_widget(p, area);
    }

    fn render_request(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let border_style = if self.active_area == ActiveArea::FlowDetail {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let block = Block::default().borders(Borders::ALL).title("Request").border_style(border_style);
        
        match &flow.layer {
            Layer::Http(h) => {
                let mut text = vec![
                    Line::from(Span::styled("Headers:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue))),
                ];
                for header in &h.request.headers {
                    text.push(Line::from(vec![
                        Span::styled(format!("  {}: ", header.0), Style::default().fg(Color::DarkGray)),
                        Span::raw(&header.1)
                    ]));
                }
                
                text.push(Line::from(""));
                text.push(Line::from(Span::styled("Body:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue))));
                
                if let Some(body) = &h.request.body {
                    text.push(Line::from(format!("  Size: {} bytes, Encoding: {}", body.size, body.encoding)));
                    // Basic body preview with JSON formatting
                    if body.size > 0 {
                        text.push(Line::from(""));
                        
                        let content = if let Ok(json_val) = serde_json::from_str::<Value>(&body.content) {
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

                let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true }).scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            },
            Layer::WebSocket(w) => {
                let mut text = vec![
                    Line::from(Span::styled("Handshake Request Headers:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue))),
                ];
                for header in &w.handshake_request.headers {
                    text.push(Line::from(vec![
                        Span::styled(format!("  {}: ", header.0), Style::default().fg(Color::DarkGray)),
                        Span::raw(&header.1)
                    ]));
                }
                let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true }).scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            },
            _ => f.render_widget(Paragraph::new("N/A").block(block), area),
        }
    }

    fn render_response(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let border_style = if self.active_area == ActiveArea::FlowDetail {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let block = Block::default().borders(Borders::ALL).title("Response").border_style(border_style);
        
        match &flow.layer {
            Layer::Http(h) => {
                if let Some(resp) = &h.response {
                    let mut text = vec![
                        Line::from(Span::styled("Headers:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue))),
                    ];
                    for header in &resp.headers {
                        text.push(Line::from(vec![
                            Span::styled(format!("  {}: ", header.0), Style::default().fg(Color::DarkGray)),
                            Span::raw(&header.1)
                        ]));
                    }

                    text.push(Line::from(""));
                    text.push(Line::from(Span::styled("Body:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue))));
                    
                    if let Some(body) = &resp.body {
                        text.push(Line::from(format!("  Size: {} bytes, Encoding: {}", body.size, body.encoding)));
                        if body.size > 0 {
                            text.push(Line::from(""));
                            
                            let content = if let Ok(json_val) = serde_json::from_str::<Value>(&body.content) {
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

                    let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true }).scroll((self.detail_scroll, 0));
                    f.render_widget(p, area);
                } else {
                    f.render_widget(Paragraph::new("Waiting for response...").block(block), area);
                }
            },
            Layer::WebSocket(w) => {
                let mut text = vec![
                    Line::from(Span::styled("Handshake Response Headers:", Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue))),
                ];
                for header in &w.handshake_response.headers {
                    text.push(Line::from(vec![
                        Span::styled(format!("  {}: ", header.0), Style::default().fg(Color::DarkGray)),
                        Span::raw(&header.1)
                    ]));
                }
                let p = Paragraph::new(text).block(block).wrap(Wrap { trim: true }).scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            },
            _ => f.render_widget(Paragraph::new("N/A").block(block), area),
        }
    }

    fn render_messages(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let border_style = if self.active_area == ActiveArea::FlowDetail {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let block = Block::default().borders(Borders::ALL).title("Messages").border_style(border_style);
        
        match &flow.layer {
            Layer::WebSocket(w) => {
                let lines: Vec<Line> = w.messages.iter().map(|msg| {
                    let direction = match msg.direction {
                        relay_core_api::flow::Direction::ClientToServer => "->",
                        relay_core_api::flow::Direction::ServerToClient => "<-",
                    };
                    let color = match msg.direction {
                        relay_core_api::flow::Direction::ClientToServer => Color::Green,
                        relay_core_api::flow::Direction::ServerToClient => Color::Blue,
                    };
                    
                    let content = if msg.content.size > 50 {
                        format!("{}... ({} bytes)", &msg.content.content[..50], msg.content.size)
                    } else {
                        msg.content.content.clone()
                    };

                    Line::from(vec![
                        Span::styled(format!("{} ", direction), Style::default().fg(color)),
                        Span::styled(format!("[{}] ", msg.opcode), Style::default().fg(Color::Yellow)),
                        Span::raw(content),
                    ])
                }).collect();

                let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: true }).scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            },
            _ => f.render_widget(Paragraph::new("Not a WebSocket flow").block(block), area),
        }
    }

    fn render_status_bar(&self, f: &mut Frame, area: Rect) {
        let flow_count = self.flows.len();
        let filtered_count = self.get_filtered_flows().len();
        let count_str = if filtered_count != flow_count {
            format!("{} (filtered from {})", filtered_count, flow_count)
        } else {
            flow_count.to_string()
        };

        let bar_text = match self.input_mode {
            InputMode::Normal => {
                vec![
                    Span::styled(format!("Flows: {} ", count_str), Style::default().fg(Color::Green)),
                    Span::raw("| "),
                    Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" quit | "),
                    Span::styled("/", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" filter | "),
                    Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" tabs | "),
                    Span::styled("?", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" help"),
                ]
            },
            InputMode::Filtering => {
                vec![
                    Span::raw("Filter: "),
                    Span::styled(self.filter_input.as_str(), Style::default().fg(Color::Yellow)),
                    Span::raw(" | "),
                    Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" apply | "),
                    Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" cancel"),
                ]
            },
            InputMode::Help => {
                vec![
                    Span::styled("HELP", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    Span::raw(" — press "),
                    Span::styled("? or Esc", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(" to close"),
                ]
            },
        };

        let text = Text::from(Line::from(bar_text));
        let paragraph = Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(paragraph, area);
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
