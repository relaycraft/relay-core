use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Padding, Paragraph, Row, Table,
        TableState, Wrap,
    },
};
use relay_core_api::flow::{Flow, Layer};
use relay_core_api::modification::{flow_matches_filter, parse_flow_filter};
use serde_json::Value;
use std::collections::VecDeque;
use std::time::Instant;
use url::Url;

use super::command::{Command, parse_command};
use super::format::{
    ColumnWidth, LayoutProfile, copy_to_clipboard, display_method, display_path,
    empty_flow_list_message, flow_duration_ms, flow_list_title, format_duration_ms, format_size,
    host_from_url, http_flow_to_curl, main_split, path_budget_for, plan_columns, smart_truncate,
    tags_list_suffix,
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

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Request => "Request",
            Self::Response => "Response",
            Self::Messages => "Messages",
        }
    }

    const ALL: [Self; 4] = [
        Self::Overview,
        Self::Request,
        Self::Response,
        Self::Messages,
    ];
}

#[derive(PartialEq, Debug)]
pub enum InputMode {
    Normal,
    Filtering,
    Help,
    Command,
}

#[derive(PartialEq, Debug)]
pub enum ActiveArea {
    FlowList,
    FlowDetail,
}

/// Whether the TUI is connected to the HTTP API for richer features.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ApiMode {
    /// API available: SSE feed (rules/intercepts via `:` command in future)
    Connected,
    /// No API: broadcast-channel-only, 2-panel layout (legacy)
    Offline,
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
    pub toast: Option<String>,
    pub paused: bool,
    pub pending_count: u64,
    pub req_timestamps: VecDeque<Instant>,
    pub api_mode: ApiMode,
    pub command_input: String,
}

impl TuiApp {
    pub fn new(port: u16, api_mode: ApiMode) -> Self {
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
            paused: false,
            pending_count: 0,
            req_timestamps: VecDeque::with_capacity(64),
            api_mode,
            command_input: String::new(),
        };
        app.table_state.select(Some(0));
        app
    }

    pub fn on_flow(&mut self, flow: Flow) {
        // Track arrival for req/s sliding window (always, even when paused).
        let now = Instant::now();
        self.req_timestamps.push_back(now);
        // Prune timestamps older than 5 seconds.
        while self
            .req_timestamps
            .front()
            .is_some_and(|t| now.duration_since(*t).as_secs() > 5)
        {
            self.req_timestamps.pop_front();
        }

        if let Some(pos) = self.flows.iter().position(|f| f.id == flow.id) {
            self.flows[pos] = flow;
        } else {
            self.flow_count_total = self.flow_count_total.saturating_add(1);
            if self.paused {
                self.pending_count = self.pending_count.saturating_add(1);
                return;
            }
            self.flows.push_front(flow);
            if self.flows.len() > 1000 {
                self.flows.pop_back();
            }
            if self.auto_scroll {
                self.table_state.select(Some(0));
            } else {
                self.pending_count = self.pending_count.saturating_add(1);
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
                if key == KeyCode::Char(':') {
                    self.input_mode = InputMode::Command;
                    self.command_input.clear();
                    return;
                }
                match self.active_area {
                    ActiveArea::FlowList => match key {
                        KeyCode::Char('q') => self.should_quit = true,
                        KeyCode::Char('d') => {
                            self.delete_selected();
                            self.detail_scroll = 0;
                        }
                        KeyCode::Char('p') => {
                            self.paused = !self.paused;
                            self.pending_count = 0;
                        }
                        KeyCode::Char('c') => {
                            self.flows.clear();
                            self.pending_count = 0;
                            self.table_state.select(None);
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
                        KeyCode::Home | KeyCode::Char('g') => {
                            if !self.get_filtered_flows().is_empty() {
                                self.table_state.select(Some(0));
                            }
                            self.auto_scroll = true;
                            self.pending_count = 0;
                            self.detail_scroll = 0;
                        }
                        KeyCode::End | KeyCode::Char('G') => {
                            let len = self.get_filtered_flows().len();
                            if len > 0 {
                                self.table_state.select(Some(len - 1));
                            }
                            self.auto_scroll = false;
                            self.detail_scroll = 0;
                        }
                        KeyCode::Tab => {
                            self.active_area = ActiveArea::FlowDetail;
                            self.detail_scroll = 0;
                        }
                        KeyCode::Char('1') => self.detail_tab = DetailTab::Overview,
                        KeyCode::Char('2') => self.detail_tab = DetailTab::Request,
                        KeyCode::Char('3') => self.detail_tab = DetailTab::Response,
                        KeyCode::Char('4') => self.detail_tab = DetailTab::Messages,
                        KeyCode::Char('/') => self.input_mode = InputMode::Filtering,
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
            InputMode::Command => match key {
                KeyCode::Enter => {
                    let cmd = parse_command(&self.command_input);
                    self.dispatch_command(cmd);
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Esc => self.input_mode = InputMode::Normal,
                KeyCode::Backspace => {
                    self.command_input.pop();
                }
                KeyCode::Char(c) => self.command_input.push(c),
                _ => {}
            },
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
        f.render_widget(Block::default().style(Theme::surface()), area);

        // Top rule + text (+ optional toast) + bottom rule — one row each, no blank filler band.
        let status_height = if matches!(self.input_mode, InputMode::Normal) && self.toast.is_some()
        {
            4
        } else {
            3
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(status_height)].as_ref())
            .split(area);

        let profile = LayoutProfile::for_width(area.width);

        if profile.is_two_pane() {
            let (list_pct, detail_pct) =
                main_split(profile).expect("two-pane profile must have split");
            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(list_pct),
                    Constraint::Percentage(detail_pct),
                ])
                .split(chunks[0]);
            self.render_flow_list(f, main_chunks[0]);
            self.render_flow_detail(f, main_chunks[1]);
        } else {
            match self.active_area {
                ActiveArea::FlowList => self.render_flow_list(f, chunks[0]),
                ActiveArea::FlowDetail => self.render_flow_detail(f, chunks[0]),
            }
        }
        self.render_status_bar(f, chunks[1]);

        if self.input_mode == InputMode::Help {
            self.render_help_overlay(f);
        }
    }

    fn render_help_overlay(&self, f: &mut Frame) {
        let mut lines: Vec<Line> = vec![
            Line::from(vec![Span::styled("RelayCore TUI", Theme::accent_bold())]),
            Line::from(""),
        ];

        match self.api_mode {
            ApiMode::Connected => {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("Mode: ", Theme::label()),
                    Span::styled("API Connected ", Theme::stat_ok()),
                    Span::styled(
                        "(2-panel: flows + detail)",
                        Theme::muted(),
                    ),
                ]));
            }
            ApiMode::Offline => {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("Mode: ", Theme::label()),
                    Span::styled("Offline ", Theme::muted()),
                    Span::styled("(2-panel: flows + detail)", Theme::muted()),
                ]));
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("Enable API mode: ", Theme::label()),
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
            help_binding("Enter  →", "Focus detail panel"),
        ]);

        lines.push(Line::from(""));
        lines.push(help_section("Detail Panel"));
        lines.extend([
            help_binding("Esc  ←", "Back to flow list"),
            help_binding(
                "Tab",
                "Cycle tabs: Overview → Request → Response → Messages",
            ),
            help_binding("1 – 4", "Jump to tab by number"),
            help_binding("PgUp  PgDown", "Scroll content"),
            help_binding("Ctrl+u  Ctrl+d", "Scroll up / down 10 lines"),
        ]);

        lines.push(Line::from(""));
        lines.push(help_section("Actions"));
        lines.extend([
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

    fn render_flow_list(&mut self, f: &mut Frame, area: Rect) {
        let filtered_flows = self.get_filtered_flows();
        let profile = LayoutProfile::for_width(area.width);
        let columns = plan_columns(profile);
        let filter = &self.filter_input;
        let filtering = !filter.is_empty();
        let path_budget = path_budget_for(profile, area.width);

        let list_focused = self.active_area == ActiveArea::FlowList;
        let title = flow_list_title(filter, filtered_flows.len(), self.flows.len());

        let widths: Vec<Constraint> = columns
            .iter()
            .map(|c| match c.width {
                ColumnWidth::Fixed(w) => Constraint::Length(w),
                ColumnWidth::Rest => Constraint::Min(10),
            })
            .collect();

        if filtered_flows.is_empty() {
            let cells: Vec<Cell> = columns
                .iter()
                .map(|c| {
                    if matches!(c.width, ColumnWidth::Rest) {
                        Cell::from(Line::from(Span::styled(
                            empty_flow_list_message(filtering),
                            Theme::muted(),
                        )))
                    } else {
                        Cell::from("")
                    }
                })
                .collect();
            let row = Row::new(cells);
            let table = Table::new(vec![row], widths)
                .block(outer_panel_block(&title, list_focused));
            f.render_widget(table, area);
            return;
        }

        let selected = self.table_state.selected();
        let rows: Vec<Row> = filtered_flows
            .iter()
            .enumerate()
            .map(|(i, flow)| {
                flow_table_row(
                    flow,
                    profile,
                    path_budget,
                    filter,
                    filtering,
                    selected == Some(i),
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

        f.render_stateful_widget(table, area, &mut self.table_state);
    }

    fn render_flow_detail(&self, f: &mut Frame, area: Rect) {
        let detail_focused = self.active_area == ActiveArea::FlowDetail;
        let detail_title = "Detail";
        let block = outer_panel_block(detail_title, detail_focused);
        let inner = block.inner(area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)].as_ref())
            .split(inner);

        render_detail_tab_bar(f, chunks[0], self.detail_tab);
        f.render_widget(block, area);

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

    fn render_overview(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let block = panel_body_block(self.detail_scroll);

        let mut lines: Vec<Line> = Vec::new();

        if let Some(summary) = flow_summary_line(flow) {
            lines.push(summary);
        }

        lines.push(panel_kv_line("ID:", flow.id.to_string(), Theme::muted()));

        match &flow.layer {
            Layer::Http(h) => {
                lines.push(panel_kv_line(
                    "Host:",
                    host_from_url(&h.request.url),
                    Theme::value(),
                ));
                lines.push(panel_kv_line(
                    "Path:",
                    display_path(&h.request.url, 160, false),
                    Theme::value(),
                ));
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
                    "Host:",
                    host_from_url(&w.handshake_request.url),
                    Theme::value(),
                ));
                lines.push(panel_kv_line(
                    "Path:",
                    display_path(&w.handshake_request.url, 160, false),
                    Theme::value(),
                ));
                if let Some(query) = w.handshake_request.url.query() {
                    lines.push(panel_kv_line(
                        "Query:",
                        smart_truncate(query, 160),
                        Theme::muted(),
                    ));
                }
                lines.push(panel_kv_line(
                    "Messages:",
                    w.messages.len().to_string(),
                    Theme::value(),
                ));
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
            push_panel_section(&mut lines, "Tags");
            lines.push(Line::from(vec![
                Span::raw(PANEL_SECTION_INDENT),
                Span::styled(flow.tags.join("  "), Theme::accent_dim()),
            ]));
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
            .scroll((self.detail_scroll, 0));
        f.render_widget(p, area);
    }

    fn render_request(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let block = panel_body_block(self.detail_scroll);

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
                        for line in render_body_lines(&body.content) {
                            text.push(line);
                        }
                    }
                } else {
                    text.push(Line::from("(No Body)"));
                }

                let p = Paragraph::new(text)
                    .block(block)
                    .wrap(Wrap { trim: true })
                    .scroll((self.detail_scroll, 0));
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
                    .scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            }
            _ => f.render_widget(Paragraph::new("N/A").block(block), area),
        }
    }

    fn render_response(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let block = panel_body_block(self.detail_scroll);

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
                            for line in render_body_lines(&body.content) {
                                text.push(line);
                            }
                        }
                    } else {
                        text.push(Line::from("(No Body)"));
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
                    .scroll((self.detail_scroll, 0));
                f.render_widget(p, area);
            }
            _ => f.render_widget(Paragraph::new("N/A").block(block), area),
        }
    }

    fn render_messages(&self, f: &mut Frame, area: Rect, flow: &Flow) {
        let block = panel_body_block(self.detail_scroll);

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

    fn status_hints(&self) -> Vec<(u16, String)> {
        let p_action = if self.paused { "resume" } else { "pause" };
        let active = match self.active_area {
            ActiveArea::FlowList => "LIST",
            ActiveArea::FlowDetail => "DETAIL",
        };
        let mut hints = vec![
            (10, "[?]help".into()),
            (12, format!("[{active}]")),
            (8, "[q]quit".into()),
            (8, "[/]filter".into()),
            (8, "[y]curl".into()),
            (12, format!("[p]{p_action}")),
            (8, "[c]clear".into()),
        ];
        if self.pending_count > 0 {
            hints.insert(0, (12, format!("↓{} new", self.pending_count)));
        }
        hints
    }

    fn dispatch_command(&mut self, cmd: Command) {
        match cmd {
            Command::Quit => self.should_quit = true,
            Command::Clear => {
                self.flows.clear();
                self.pending_count = 0;
                self.table_state.select(None);
                self.detail_scroll = 0;
                self.toast = Some("Flows cleared".into());
            }
            Command::Pause => {
                self.paused = true;
                self.toast = Some("Paused".into());
            }
            Command::Resume => {
                self.paused = false;
                self.pending_count = 0;
                self.toast = Some("Resumed".into());
            }
            Command::Filter(filter) => {
                self.toast = Some(format!("Filter: {filter}"));
                self.filter_input = filter;
            }
            Command::Unfilter => {
                self.filter_input.clear();
                self.toast = Some("Filter cleared".into());
            }
            Command::Theme(name) => {
                match crate::ui::theme::ThemeId::parse(&name) {
                    Ok(id) => {
                        crate::ui::theme::init(id);
                        self.toast = Some(format!("Theme: {}", id.description()));
                    }
                    Err(e) => self.toast = Some(format!("{e}")),
                }
            }
            Command::Copy(_) => self.copy_curl_selection(),
            Command::Help => {
                self.toast = Some(
                    "Commands: :q :clear :pause :resume :filter :unfilter :theme :copy :help".into(),
                );
            }
            Command::Unknown(msg) => {
                self.toast = Some(format!("{} — :help for commands", msg));
            }
            // Mark/Unmark/NextMark dispatched in on_key for direct key access
            _ => {}
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

        // req/s: count timestamps in the last 1 second
        let now = Instant::now();
        let req_per_sec = self
            .req_timestamps
            .iter()
            .rev()
            .take_while(|t| now.duration_since(**t).as_secs() < 1)
            .count();

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
                let rec = if self.paused {
                    Span::styled("[⏸ PAUSED] ", Theme::error_bold())
                } else {
                    Span::styled("[●REC] ", Theme::stat_ok())
                };

                let pending = if self.pending_count > 0 {
                    Span::styled(
                        format!("↓{} new ", self.pending_count),
                        Theme::accent_bold(),
                    )
                } else {
                    Span::raw("")
                };

                // Left section
                let left = vec![
                    rec,
                    pending,
                    Span::styled("Flows: ", Theme::label()),
                    Span::styled(format!("{} ", count_str), Theme::stat_ok()),
                    Span::styled(format!("{}req/s ", req_per_sec), Theme::stat_info()),
                    Span::styled("| ", Theme::muted()),
                    Span::styled("Total: ", Theme::label()),
                    Span::styled(format!("{}", self.flow_count_total), Theme::text()),
                ];

                // Middle section
                let middle = vec![
                    Span::styled(" :{} ", Theme::uptime()),
                    Span::styled(format!("{}", self.proxy_port), Theme::uptime()),
                    Span::styled(" ", Theme::muted()),
                    Span::styled(format!("up {}", uptime), Theme::uptime()),
                ];

                // Right section — priority-based hints that truncate on narrow terminals.
                let hints = self.status_hints();
                let left_width = Text::from(Line::from(left.clone())).width() as u16;
                let middle_width = Text::from(Line::from(middle.clone())).width() as u16;
                let right_budget = area.width.saturating_sub(left_width + middle_width);

                let mut right_spans: Vec<Span> = Vec::new();
                let mut used = 0u16;
                for (_min_width, hint) in &hints {
                    let hint_w = hint.len() as u16 + 1; // +1 for separator
                    if used + hint_w <= right_budget || right_spans.is_empty() {
                        right_spans.push(Span::styled(
                            if right_spans.is_empty() { "" } else { " " },
                            Theme::muted(),
                        ));
                        right_spans.push(Span::styled(hint.as_str(), Theme::hotkey()));
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

                if let Some(ref msg) = self.toast {
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
                    Span::styled(self.filter_input.as_str(), Theme::accent()),
                ];
                if self.filter_input.is_empty() {
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
                let colon = if self.command_input.is_empty() {
                    Span::styled(":", Theme::accent())
                } else {
                    Span::styled(":", Theme::muted())
                };
                vec![
                    colon,
                    Span::styled(self.command_input.as_str(), Theme::accent()),
                    Span::styled(" | ", Theme::muted()),
                    Span::styled("Enter", Theme::hotkey()),
                    Span::styled(" execute | ", Theme::muted()),
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

fn flow_table_row(
    flow: &Flow,
    profile: LayoutProfile,
    path_budget: usize,
    filter: &str,
    filtering: bool,
    selected: bool,
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
    let dur_ms = flow_duration_ms(flow);
    let dur_label = format_duration_ms(dur_ms);

    let marker_cell = if selected {
        Cell::from(Span::styled("▸", Theme::row_marker()))
    } else {
        Cell::from(Span::raw(""))
    };
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

    match profile {
        LayoutProfile::TooNarrow | LayoutProfile::SinglePane => {
            let url_text = smart_truncate(url.as_str(), path_budget);
            Row::new(vec![
                marker_cell,
                Cell::from(Span::styled(url_text, Theme::text())),
            ])
        }
        LayoutProfile::TwoPaneCompact | LayoutProfile::TwoPaneStandard => {
            let url_text = smart_truncate(url.as_str(), path_budget);
            let url_with_tags = if tags_str.is_empty() {
                url_text
            } else {
                format!("{}{}", url_text, tags_str)
            };
            Row::new(vec![
                marker_cell,
                method_cell,
                status_cell,
                dur_cell,
                styled_text_cell(&url_with_tags, filter, filtering, Theme::text()),
            ])
        }
        LayoutProfile::TwoPaneWide | LayoutProfile::TwoPaneExtraWide => {
            let host = host_from_url(&url);
            let host_color = Theme::host_color(&host);
            let path = display_path(&url, path_budget, has_query);
            let path_with_tags = if tags_str.is_empty() {
                path
            } else {
                format!("{}{}", path, tags_str)
            };
            Row::new(vec![
                marker_cell,
                method_cell,
                status_cell,
                dur_cell,
                Cell::from(Span::styled(format!("{:>7}", size_str), Theme::text())),
                Cell::from(Span::styled(host, Style::default().fg(host_color))),
                styled_text_cell(&path_with_tags, filter, filtering, Theme::text()),
            ])
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

/// Maximum body bytes to render before truncating.
const BODY_RENDER_LIMIT: usize = 4096;

/// Render body content as styled lines. Handles JSON colouring and 4 KB truncation.
fn render_body_lines(body_content: &str) -> Vec<Line<'static>> {
    let truncated = body_content.len() > BODY_RENDER_LIMIT;
    let display = if truncated {
        &body_content[..BODY_RENDER_LIMIT]
    } else {
        body_content
    };

    let is_json = serde_json::from_str::<Value>(display).is_ok();

    let mut lines: Vec<Line> = if is_json {
        // JSON syntax highlighting
        display
            .lines()
            .map(|line| {
                Line::from(
                    highlight_json_line(line)
                        .into_iter()
                        .map(|(text, style)| Span::styled(text, style))
                        .collect::<Vec<Span>>(),
                )
            })
            .collect()
    } else {
        display
            .lines()
            .map(|line| Line::from(Span::styled(line.to_string(), Theme::text())))
            .collect()
    };

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
const PANEL_KV_LABEL_WIDTH: usize = 12;
const PANEL_SECTION_INDENT: &str = "  ";

fn panel_kv_label_column(label: &str) -> String {
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
            let host = host_from_url(&h.request.url);
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
                Span::styled(format!("{host}{path_show}"), Theme::value()),
            ]))
        }
        Layer::WebSocket(w) => {
            let status = w.handshake_response.status.to_string();
            let dur = format_duration_ms(flow_duration_ms(flow));
            let host = host_from_url(&w.handshake_request.url);
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
                Span::styled(format!("{host}{path_show}"), Theme::value()),
                Span::styled(format!("  ({msg_count} msgs)"), Theme::muted()),
            ]))
        }
        _ => None,
    }
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
        Span::styled(description, Theme::text()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    #[test]
    fn panel_kv_label_column_is_fixed_width() {
        assert_eq!(panel_kv_label_column("ID:").len(), PANEL_KV_LABEL_WIDTH);
        assert_eq!(panel_kv_label_column("Host:").len(), PANEL_KV_LABEL_WIDTH);
        assert_eq!(
            panel_kv_label_column("WS Pending:").len(),
            PANEL_KV_LABEL_WIDTH
        );
    }
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
            resilience_trace: None,
            rule_variables: std::collections::HashMap::new(),
            matched_rules: vec![],
        }
    }

    #[test]
    fn test_new_app_has_selection() {
        let app = TuiApp::new(8080, ApiMode::Offline);
        assert_eq!(app.table_state.selected(), Some(0));
        assert_eq!(app.detail_tab, DetailTab::Overview);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.active_area, ActiveArea::FlowList);
        assert!(app.auto_scroll);
    }

    #[test]
    fn test_next_previous_wraps_around() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
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
        let mut app = TuiApp::new(8080, ApiMode::Offline);
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
        let mut app = TuiApp::new(8080, ApiMode::Offline);
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
        let mut app = TuiApp::new(8080, ApiMode::Offline);
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
        let mut app = TuiApp::new(8080, ApiMode::Offline);
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
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn test_detail_tab_cycle() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        assert_eq!(app.detail_tab, DetailTab::Overview);
        app.active_area = ActiveArea::FlowDetail;
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
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Help);
        app.on_key(key_repeat(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Help);
        app.on_key(key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn test_filter_mode_toggle() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
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
    fn test_home_and_g_jump_to_newest_at_top() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        for i in 0..3 {
            app.on_flow(make_http_flow(
                &format!("00000000-0000-0000-0000-00000000000{i}"),
                &format!("http://example.com/{i}"),
                "GET",
            ));
        }
        app.table_state.select(Some(2));
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.table_state.selected(), Some(0));
        assert!(app.auto_scroll);
        app.table_state.select(Some(2));
        app.on_key(key(KeyCode::Char('g')));
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn test_end_and_g_jump_to_oldest_at_bottom() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        for i in 0..3 {
            app.on_flow(make_http_flow(
                &format!("00000000-0000-0000-0000-00000000000{i}"),
                &format!("http://example.com/{i}"),
                "GET",
            ));
        }
        app.table_state.select(Some(0));
        app.on_key(key(KeyCode::End));
        assert_eq!(app.table_state.selected(), Some(2));
        assert!(!app.auto_scroll);
        app.table_state.select(Some(0));
        app.on_key(key(KeyCode::Char('G')));
        assert_eq!(app.table_state.selected(), Some(2));
    }

    #[test]
    fn test_ui_paints_opaque_background() {
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000001",
            "http://api.example.com/v1/users?limit=10",
            "GET",
        ));

        terminal.draw(|f| app.ui(f)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_ne!(buffer[(119, 31)].bg, Color::Reset);
        assert_eq!(buffer[(119, 31)].bg, Theme::bg_elevated());
    }

    #[test]
    fn test_tab_from_flow_list_focuses_detail() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        assert_eq!(app.active_area, ActiveArea::FlowList);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.active_area, ActiveArea::FlowDetail);
    }

    #[test]
    fn test_copy_curl_sets_toast() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
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
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        assert_eq!(app.active_area, ActiveArea::FlowList);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.active_area, ActiveArea::FlowDetail);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.active_area, ActiveArea::FlowList);
    }

    #[test]
    fn test_keyboard_navigation_moves_selection() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
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

    #[test]
    fn test_status_hints_includes_pending_count() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        let hints = app.status_hints();
        // No pending hint when count is 0
        assert!(!hints.iter().any(|(_, s)| s.contains("new")));

        app.pending_count = 5;
        let hints = app.status_hints();
        assert_eq!(hints[0].1, "↓5 new");
    }

    #[test]
    fn test_status_hints_changes_pause_label() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        let hints = app.status_hints();
        assert!(hints.iter().any(|(_, s)| s.contains("[p]pause")));

        app.paused = true;
        let hints = app.status_hints();
        assert!(hints.iter().any(|(_, s)| s.contains("[p]resume")));
    }

    #[test]
    fn test_render_six_widths_no_panic() {
        for width in [60, 80, 100, 120, 150, 200] {
            let backend = TestBackend::new(width, 32);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut app = TuiApp::new(8080, ApiMode::Offline);
            app.flows.push_back(make_http_flow(
                "00000000-0000-0000-0000-000000000001",
                "http://api.example.com/v1/users",
                "GET",
            ));
            terminal.draw(|f| app.ui(f)).unwrap();
        }
    }
}
