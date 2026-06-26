use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, TableState},
};
use relay_core_api::flow::Flow;
use relay_core_api::modification::{flow_matches_filter, parse_flow_filter};
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::time::Instant;
use uuid::Uuid;

use super::action::TuiAction;
use super::command::{Command, parse_command};
use super::format::{BodyView, LayoutProfile, copy_to_clipboard, http_flow_to_curl, main_split};
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

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Request => "Request",
            Self::Response => "Response",
            Self::Messages => "Messages",
        }
    }

    pub(super) const ALL: [Self; 4] = [
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
    Marking,
}

#[derive(PartialEq, Debug)]
pub enum ActiveArea {
    FlowList,
    FlowDetail,
}

/// Whether `--api-port` was enabled at startup (help text only; TUI behavior is the same).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ApiMode {
    /// `--api-port` set: REST/SSE HTTP API is listening for external clients.
    Connected,
    /// No `--api-port`: proxy runs without the REST/SSE HTTP API.
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
    pub toast_at: Option<Instant>,
    pub paused: bool,
    pub pending_count: u64,
    pub req_timestamps: VecDeque<Instant>,
    pub api_mode: ApiMode,
    pub command_input: String,
    pub marks: BTreeMap<Uuid, char>,
    pub body_view: BodyView,
    replay_rx: Option<tokio::sync::oneshot::Receiver<String>>,
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
            toast_at: None,
            paused: false,
            pending_count: 0,
            req_timestamps: VecDeque::with_capacity(64),
            api_mode,
            command_input: String::new(),
            marks: BTreeMap::new(),
            body_view: BodyView::Auto,
            replay_rx: None,
        };
        app.table_state.select(Some(0));
        app
    }

    fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some(msg.into());
        self.toast_at = Some(Instant::now());
    }

    fn prune_expired_toast(&mut self) {
        if self.toast_at.is_some_and(|at| at.elapsed().as_secs() >= 5) {
            self.toast = None;
            self.toast_at = None;
        }
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
            if self.flows.len() > 1000
                && let Some(evicted) = self.flows.pop_back()
            {
                self.marks.remove(&evicted.id);
            }
            if self.auto_scroll {
                self.table_state.select(Some(0));
            } else {
                self.pending_count = self.pending_count.saturating_add(1);
            }
        }
    }

    pub(super) fn get_filtered_flows(&self) -> Vec<&Flow> {
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
            self.set_toast("No flow selected");
            return;
        };
        let Some(curl) = http_flow_to_curl(flow) else {
            self.set_toast("cURL: not an HTTP/WebSocket flow");
            return;
        };
        if copy_to_clipboard(&curl) {
            self.set_toast("cURL copied to clipboard");
        } else {
            self.set_toast("cURL built (install pbcopy/xclip for clipboard)");
        }
    }

    pub fn on_key(&mut self, event: KeyEvent) {
        // Ignore Repeat/Release — e.g. `?` Press opens Help, Repeat would instantly close it.
        if event.kind != KeyEventKind::Press {
            return;
        }
        self.prune_expired_toast();
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
                if key == KeyCode::Char('m') {
                    self.input_mode = InputMode::Marking;
                    return;
                }
                if key == KeyCode::Char('\'') {
                    self.next_mark();
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
                        KeyCode::Char('R') => self.apply_action(TuiAction::ReplaySelectedFlow),
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
                        KeyCode::Char('R') => self.apply_action(TuiAction::ReplaySelectedFlow),
                        KeyCode::Char('v') => {
                            self.body_view = self.body_view.next();
                            self.set_toast(format!("View: {}", self.body_view.label()));
                        }
                        _ => {}
                    },
                }
            }
            InputMode::Filtering => match key {
                KeyCode::Enter | KeyCode::Esc => self.input_mode = InputMode::Normal,
                KeyCode::Char(c) => self.filter_input.push(c),
                KeyCode::Backspace => {
                    self.filter_input.pop();
                }
                _ => {}
            },
            InputMode::Marking => {
                if key == KeyCode::Esc || key == KeyCode::Enter || key == KeyCode::Backspace {
                    self.input_mode = InputMode::Normal;
                    return;
                }
                if let KeyCode::Char(c) = key
                    && c.is_ascii_alphabetic()
                {
                    let flow_id = self.selected_flow().map(|f| f.id);
                    if let Some(id) = flow_id {
                        let label = c.to_ascii_uppercase();
                        if self.marks.remove(&id).is_some() {
                            self.set_toast(format!("Unmarked '{}'", label));
                        } else {
                            self.marks.insert(id, label);
                            self.set_toast(format!("Marked '{}'", label));
                        }
                    }
                }
                self.input_mode = InputMode::Normal;
            }
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

    fn next_mark(&mut self) {
        if self.marks.is_empty() {
            return;
        }
        // Get all marked flow IDs sorted by mark char
        let mut marked: Vec<(char, Uuid)> = self.marks.iter().map(|(id, c)| (*c, *id)).collect();
        marked.sort_by_key(|(c, _)| *c);
        // Find first mark after cursor
        if let Some(selected_id) = self.selected_flow().map(|f| f.id)
            && let Some(pos) = marked.iter().position(|(_, id)| *id == selected_id)
        {
            let next = (pos + 1) % marked.len();
            let target_id = marked[next].1;
            if let Some(idx) = self
                .get_filtered_flows()
                .iter()
                .position(|f| f.id == target_id)
            {
                self.table_state.select(Some(idx));
                self.auto_scroll = false;
                self.detail_scroll = 0;
                self.set_toast(format!("Jumped to mark '{}'", marked[next].0));
            }
            return;
        }
        // No selection or not in marked — jump to first mark
        let target_id = marked[0].1;
        if let Some(idx) = self
            .get_filtered_flows()
            .iter()
            .position(|f| f.id == target_id)
        {
            self.table_state.select(Some(idx));
            self.auto_scroll = false;
            self.detail_scroll = 0;
            self.set_toast(format!("Jumped to mark '{}'", marked[0].0));
        } else {
            self.set_toast("Mark not in current filter");
        }
    }

    fn delete_selected(&mut self) {
        let id = self
            .table_state
            .selected()
            .and_then(|i| self.get_filtered_flows().get(i).map(|f| f.id));
        if let Some(id) = id {
            self.flows.retain(|f| f.id != id);
            self.marks.remove(&id);
            let new_len = self.get_filtered_flows().len();
            if new_len == 0 {
                self.table_state.select(None);
            } else if self.table_state.selected().unwrap_or(0) >= new_len {
                self.table_state.select(Some(new_len - 1));
            }
        }
    }

    pub fn ui(&mut self, f: &mut Frame) {
        self.prune_expired_toast();
        self.poll_replay_result();
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
            super::render::render_flow_list(self, f, main_chunks[0]);
            super::render::render_flow_detail(self, f, main_chunks[1]);
        } else {
            match self.active_area {
                ActiveArea::FlowList => super::render::render_flow_list(self, f, chunks[0]),
                ActiveArea::FlowDetail => super::render::render_flow_detail(self, f, chunks[0]),
            }
        }
        super::render::render_status_bar(self, f, chunks[1]);

        if self.input_mode == InputMode::Help {
            super::render::render_help_overlay(self, f);
        }
    }

    pub(super) fn status_hints(&self) -> Vec<(u16, String)> {
        let p_action = if self.paused { "resume" } else { "pause" };
        let mut hints = vec![
            (10, "[?]help".into()),
            (8, "[q]quit".into()),
            (8, "[/]filter".into()),
            (8, "[y]curl".into()),
            (12, format!("[p]{p_action}")),
            (8, "[c]clear".into()),
        ];
        if !self.marks.is_empty() {
            let mut marked: Vec<char> = self.marks.values().copied().collect();
            marked.sort();
            let label = format!(
                "marks: {}",
                marked
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            hints.push((10, label));
        }
        hints
    }

    fn dispatch_command(&mut self, cmd: Command) {
        let action = match cmd {
            Command::Quit => TuiAction::Quit,
            Command::Clear => TuiAction::ClearFlows,
            Command::Pause => TuiAction::SetPaused(true),
            Command::Resume => TuiAction::SetPaused(false),
            Command::Filter(filter) => TuiAction::ApplyFilter(filter),
            Command::Unfilter => TuiAction::ClearFilter,
            Command::Theme(name) => TuiAction::ApplyTheme(name),
            Command::CopyCurl => TuiAction::CopySelectedCurl,
            Command::View(name) => match name.as_str() {
                "auto" => TuiAction::SetBodyView(BodyView::Auto),
                "pretty" => TuiAction::SetBodyView(BodyView::Pretty),
                "raw" => TuiAction::SetBodyView(BodyView::Raw),
                "hex" => TuiAction::SetBodyView(BodyView::Hex),
                _ => TuiAction::SetToast(format!("Unknown view: {name} (auto|pretty|raw|hex)")),
            },
            Command::Replay => TuiAction::ReplaySelectedFlow,
            Command::Help => TuiAction::ShowCommandHelp,
            Command::Unknown(msg) => TuiAction::SetToast(format!("{msg} — try :help")),
        };
        self.apply_action(action);
    }

    fn apply_action(&mut self, action: TuiAction) {
        match action {
            TuiAction::Quit => self.should_quit = true,
            TuiAction::ClearFlows => {
                self.flows.clear();
                self.pending_count = 0;
                self.table_state.select(None);
                self.detail_scroll = 0;
                self.set_toast("Flows cleared");
            }
            TuiAction::SetPaused(paused) => {
                self.paused = paused;
                self.pending_count = 0;
                self.set_toast(if paused { "Paused" } else { "Resumed" });
            }
            TuiAction::ApplyFilter(filter) => {
                self.set_toast(format!("Filter: {filter}"));
                self.filter_input = filter;
                let filtered = self.get_filtered_flows();
                self.table_state
                    .select(if filtered.is_empty() { None } else { Some(0) });
                self.detail_scroll = 0;
            }
            TuiAction::ClearFilter => {
                self.filter_input.clear();
                self.set_toast("Filter cleared");
            }
            TuiAction::ApplyTheme(name) => match crate::ui::theme::ThemeId::parse(&name) {
                Ok(id) => {
                    crate::ui::theme::init(id);
                    self.set_toast(format!("Theme: {}", id.description()));
                }
                Err(e) => self.set_toast(e.to_string()),
            },
            TuiAction::CopySelectedCurl => self.copy_curl_selection(),
            TuiAction::SetBodyView(view) => {
                self.body_view = view;
                self.set_toast(format!("View: {}", self.body_view.label()));
            }
            TuiAction::ShowCommandHelp => {
                self.set_toast(
                    ":q :clear :pause :resume :f :uf :theme :cp :v :rr :help | press ? for keys",
                );
            }
            TuiAction::SetToast(msg) => self.set_toast(msg),
            TuiAction::ReplaySelectedFlow => self.replay_selected_flow(),
        }
    }

    fn poll_replay_result(&mut self) {
        if let Some(ref mut rx) = self.replay_rx
            && let Ok(msg) = rx.try_recv()
        {
            self.set_toast(msg);
            self.replay_rx = None;
        }
    }

    fn replay_selected_flow(&mut self) {
        let Some(flow) = self.selected_flow() else {
            self.set_toast("No flow selected to replay");
            return;
        };

        let (method, url, headers, body_bytes) = match &flow.layer {
            relay_core_api::flow::Layer::Http(h) => {
                let body_bytes = h.request.body.as_ref().and_then(|b| {
                    if b.size == 0 {
                        return None;
                    }
                    let bytes = if b.encoding == "base64" {
                        use base64::{Engine as _, engine::general_purpose::STANDARD};
                        STANDARD.decode(&b.content).unwrap_or_default()
                    } else {
                        b.content.as_bytes().to_vec()
                    };
                    Some(bytes)
                });

                (
                    h.request.method.clone(),
                    h.request.url.clone(),
                    h.request.headers.clone(),
                    body_bytes,
                )
            }
            _ => {
                self.set_toast("Replay only supported for HTTP flows");
                return;
            }
        };

        if self.replay_rx.is_some() {
            self.set_toast("Replay already in progress");
            return;
        }

        let proxy_port = self.proxy_port;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.replay_rx = Some(rx);

        let Ok(method) = reqwest::Method::from_bytes(method.as_bytes()).inspect_err(|_| {
            self.replay_rx = None;
            self.set_toast("Invalid HTTP method")
        }) else {
            return;
        };

        tokio::spawn(async move {
            let proxy_url = format!("http://127.0.0.1:{}", proxy_port);
            let Ok(proxy) = reqwest::Proxy::all(&proxy_url) else {
                let _ = tx.send("Replay failed: invalid proxy URL".into());
                return;
            };

            let Ok(client) = reqwest::Client::builder()
                .proxy(proxy)
                .danger_accept_invalid_certs(true)
                .build()
            else {
                let _ = tx.send("Replay failed: could not build HTTP client".into());
                return;
            };

            let mut req = client.request(method, url);

            for (k, v) in &headers {
                let k_lower = k.to_lowercase();
                if k_lower == "host"
                    || k_lower == "connection"
                    || k_lower == "content-length"
                    || k_lower == "transfer-encoding"
                {
                    continue;
                }
                if let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                    && let Ok(val) = reqwest::header::HeaderValue::from_str(v)
                {
                    req = req.header(name, val);
                }
            }

            if let Some(bytes) = body_bytes {
                req = req.body(bytes);
            }

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let _ = tx.send(format!("Replayed — {status}"));
                }
                Err(e) => {
                    let _ = tx.send(format!("Replay failed: {e}"));
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, style::Color};

    #[test]
    fn panel_kv_label_column_is_fixed_width() {
        assert_eq!(
            super::super::render::panel_kv_label_column("ID:").len(),
            super::super::render::PANEL_KV_LABEL_WIDTH
        );
        assert_eq!(
            super::super::render::panel_kv_label_column("Host:").len(),
            super::super::render::PANEL_KV_LABEL_WIDTH
        );
        assert_eq!(
            super::super::render::panel_kv_label_column("WS Pending:").len(),
            super::super::render::PANEL_KV_LABEL_WIDTH
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
    fn test_cap_eviction_cleans_marks() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        let first_id = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000000").unwrap();
        // Fill exactly 1000, marking the first one
        for i in 0..1000 {
            app.on_flow(make_http_flow(
                &format!("00000000-0000-0000-0000-{i:012x}"),
                &format!("http://example.com/{i}"),
                "GET",
            ));
        }
        app.marks.insert(first_id, 'Z');
        // 1001st push evicts the oldest (first, index 0)
        app.on_flow(make_http_flow(
            "00000000-0000-0000-0000-000000001000",
            "http://example.com/1000",
            "GET",
        ));
        assert!(!app.marks.contains_key(&first_id));
        assert_eq!(app.flows.len(), 1000);
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
    fn test_enter_esc_switches_focus() {
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
    fn test_status_hints_omits_pending_count() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        app.pending_count = 5;
        let hints = app.status_hints();
        assert!(!hints.iter().any(|(_, s)| s.contains("new")));
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
    fn test_render_widths_no_panic() {
        for width in [40, 50, 60, 80, 100, 120, 150, 200] {
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

    #[test]
    fn test_mark_flow_with_vim_style() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        let id = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000001",
            "http://example.com/test",
            "GET",
        ));
        app.table_state.select(Some(0));
        // m → enter mark mode, then 'a' to set mark A
        app.on_key(key(KeyCode::Char('m')));
        assert_eq!(app.input_mode, InputMode::Marking);
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(app.marks.get(&id), Some(&'A'));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.toast.as_deref() == Some("Marked 'A'"));

        // m → enter mark mode, then 'a' again to toggle off
        app.on_key(key(KeyCode::Char('m')));
        app.on_key(key(KeyCode::Char('a')));
        assert!(!app.marks.contains_key(&id));
        assert!(app.toast.as_deref() == Some("Unmarked 'A'"));
    }

    #[test]
    fn test_mark_esc_cancels() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        app.on_key(key(KeyCode::Char('m')));
        assert_eq!(app.input_mode, InputMode::Marking);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn test_next_mark_jumps_to_next_marked_flow() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        let id1 = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let id2 = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000001",
            "http://example.com/a",
            "GET",
        ));
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000002",
            "http://example.com/b",
            "GET",
        ));
        app.marks.insert(id1, 'A');
        app.marks.insert(id2, 'B');
        app.table_state.select(Some(0));
        app.on_key(key(KeyCode::Char('\'')));
        // Should jump to flow b (next mark after A)
        assert_eq!(app.table_state.selected(), Some(1));
    }

    #[test]
    fn test_delete_selected_removes_mark() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        let id = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        app.flows.push_back(make_http_flow(
            "00000000-0000-0000-0000-000000000001",
            "http://example.com/test",
            "GET",
        ));
        app.marks.insert(id, 'Z');
        app.table_state.select(Some(0));
        app.on_key(key(KeyCode::Char('d')));
        assert!(!app.marks.contains_key(&id));
    }

    #[test]
    fn test_status_hints_shows_marks() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        let id = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        app.marks.insert(id, 'A');
        let id2 = uuid::Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        app.marks.insert(id2, 'B');
        let hints = app.status_hints();
        assert!(
            hints
                .iter()
                .any(|(_, s)| s.contains("marks:") && s.contains("A") && s.contains("B"))
        );
    }

    #[test]
    fn test_set_toast_records_time() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        assert!(app.toast_at.is_none());
        app.set_toast("hello");
        assert!(app.toast_at.is_some());
    }

    #[test]
    fn test_prune_expired_toast_clears_state() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        app.set_toast("stale");
        // Simulate old toast
        app.toast_at = Some(Instant::now() - std::time::Duration::from_secs(6));
        app.prune_expired_toast();
        assert!(app.toast.is_none());
        assert!(app.toast_at.is_none());
    }

    #[test]
    fn test_prune_keeps_fresh_toast() {
        let mut app = TuiApp::new(8080, ApiMode::Offline);
        app.set_toast("fresh");
        app.prune_expired_toast();
        assert!(app.toast.is_some());
        assert!(app.toast_at.is_some());
    }
}
