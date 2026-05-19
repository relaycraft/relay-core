use relay_core_api::flow::{Direction, Flow, HttpRequest, HttpResponse, Layer, WebSocketMessage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowIndex {
    pub id: String,
    pub msg_ts: Option<u64>,
    pub method: String,
    pub url: String,
    pub host: String,
    pub path: String,
    pub status: u16,
    pub http_version: String,
    pub content_type: String,
    pub started_date_time: String,
    pub time: u64,
    pub size: u64,
    pub client_ip: Option<String>,
    pub app_name: Option<String>,
    pub app_display_name: Option<String>,
    pub has_error: bool,
    pub has_request_body: bool,
    pub has_response_body: bool,
    pub is_websocket: bool,
    pub websocket_frame_count: u32,
    pub is_intercepted: bool,
    pub hits: Vec<FlowIndexHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowIndexHit {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetail {
    pub id: String,
    pub seq: u64, // Placeholder
    pub started_date_time: String,
    pub time: u64,
    pub request: FlowDetailRequest,
    pub response: FlowDetailResponse,
    pub timings: FlowDetailTimings,
    pub cache: HashMap<String, String>,
    pub _rc: FlowDetailRc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailRequest {
    pub method: String,
    pub url: String,
    pub http_version: String,
    pub headers: Vec<FlowDetailHeader>,
    pub cookies: Vec<FlowDetailCookie>,
    pub query_string: Vec<FlowDetailQuery>,
    pub post_data: Option<FlowDetailPostData>,
    pub body_size: u64,
    pub headers_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailResponse {
    pub status: u16,
    pub status_text: String,
    pub http_version: String,
    pub headers: Vec<FlowDetailHeader>,
    pub cookies: Vec<FlowDetailCookie>,
    pub content: FlowDetailContent,
    pub redirect_url: String,
    pub body_size: u64,
    pub headers_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailCookie {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailQuery {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailPostData {
    pub mime_type: String,
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailContent {
    pub size: u64,
    pub mime_type: String,
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailTimings {
    pub send: i32,
    pub wait: i32,
    pub receive: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailRc {
    pub client_ip: String,
    pub is_websocket: bool,
    pub websocket_frame_count: u32,
    pub hits: Vec<String>, // Simplify for now
    pub intercept: FlowDetailRcIntercept,
    #[serde(rename = "websocketFrames")]
    pub websocket_messages: Vec<FlowDetailWebSocketMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailRcIntercept {
    pub intercepted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowDetailWebSocketMessage {
    pub id: String,
    pub flow_id: String,
    pub seq: u32,
    #[serde(rename = "type")]
    pub type_field: String,
    pub from_client: bool,
    pub content: String,
    pub encoding: String,
    pub timestamp: u64,
    pub length: u64,
}

impl From<Flow> for FlowIndex {
    fn from(flow: Flow) -> Self {
        let (
            method,
            url,
            host,
            path,
            status,
            http_version,
            content_type,
            size,
            has_resp_body,
            has_req_body,
            is_ws,
            ws_frames,
        ) = match &flow.layer {
            Layer::Http(http) => {
                let method = http.request.method.clone();
                let url = http.request.url.to_string();
                let host = http.request.url.host_str().unwrap_or("").to_string();
                let path = http.request.url.path().to_string();
                let has_req_body = http.request.body.is_some();

                let (status, size, content_type, has_resp_body) = if let Some(resp) = &http.response
                {
                    let c_type = resp
                        .headers
                        .iter()
                        .find(|(k, _)| k.to_lowercase() == "content-type")
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();

                    let body_size = resp.body.as_ref().map(|b| b.size).unwrap_or(0);

                    (resp.status, body_size, c_type, resp.body.is_some())
                } else {
                    (0, 0, String::new(), false)
                };

                (
                    method,
                    url,
                    host,
                    path,
                    status,
                    http.request.version.clone(),
                    content_type,
                    size,
                    has_resp_body,
                    has_req_body,
                    false,
                    0,
                )
            }
            Layer::WebSocket(ws) => {
                let method = ws.handshake_request.method.clone();
                let url = ws.handshake_request.url.to_string();
                let host = ws
                    .handshake_request
                    .url
                    .host_str()
                    .unwrap_or("")
                    .to_string();
                let path = ws.handshake_request.url.path().to_string();

                let (status, content_type) = (
                    ws.handshake_response.status,
                    ws.handshake_response
                        .headers
                        .iter()
                        .find(|(k, _)| k.to_lowercase() == "content-type")
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default(),
                );

                (
                    method,
                    url,
                    host,
                    path,
                    status,
                    ws.handshake_request.version.clone(),
                    content_type,
                    0,
                    false,
                    false,
                    true,
                    ws.messages.len() as u32,
                )
            }
            _ => (
                "UNKNOWN".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
                0,
                "".to_string(),
                "".to_string(),
                0,
                false,
                false,
                false,
                0,
            ),
        };

        let duration = if let Some(end) = flow.end_time {
            (end - flow.start_time).num_milliseconds() as u64
        } else {
            0
        };

        FlowIndex {
            id: flow.id.to_string(),
            msg_ts: Some(flow.start_time.timestamp_millis() as u64),
            method,
            url,
            host,
            path,
            status,
            http_version,
            content_type,
            started_date_time: flow.start_time.to_rfc3339(),
            time: duration,
            size,
            client_ip: Some(flow.network.client_ip),
            app_name: None,
            app_display_name: None,
            has_error: false,
            has_request_body: has_req_body,
            has_response_body: has_resp_body,
            is_websocket: is_ws,
            websocket_frame_count: ws_frames,
            is_intercepted: false,
            hits: vec![],
        }
    }
}

// --- Helper Functions for FlowDetail Conversion ---

/// Calculate HAR `headersSize`: total bytes from the start of the request/response
/// header section (status/request line + headers + final CRLF).
/// Uses the parsed header vec since raw bytes are not retained post-parsing.
fn calc_request_headers_size(req: &HttpRequest) -> u64 {
    // Request line: "METHOD path?query HTTP/version\r\n"
    let path = req.url.path();
    let query = req
        .url
        .query()
        .map(|q| format!("?{}", q))
        .unwrap_or_default();
    let request_line = req.method.len() + 1 + path.len() + query.len() + 1 + req.version.len() + 2;
    // Each header: "name: value\r\n"
    let headers_bytes: usize = req
        .headers
        .iter()
        .map(|(k, v)| k.len() + 2 + v.len() + 2)
        .sum();
    // Final blank line "\r\n"
    (request_line + headers_bytes + 2) as u64
}

fn calc_response_headers_size(resp: &HttpResponse) -> u64 {
    // Status line: "HTTP/version status status_text\r\n"
    let status_line = resp.version.len() + 1 + 3 + 1 + resp.status_text.len() + 2;
    // Each header: "name: value\r\n"
    let headers_bytes: usize = resp
        .headers
        .iter()
        .map(|(k, v)| k.len() + 2 + v.len() + 2)
        .sum();
    // Final blank line "\r\n"
    (status_line + headers_bytes + 2) as u64
}

fn convert_request(req: &HttpRequest) -> FlowDetailRequest {
    let headers: Vec<FlowDetailHeader> = req
        .headers
        .iter()
        .map(|(k, v)| FlowDetailHeader {
            name: k.clone(),
            value: v.clone(),
        })
        .collect();

    // Parse Cookies from "Cookie" header
    let cookies: Vec<FlowDetailCookie> = req
        .cookies
        .iter()
        .map(|c| FlowDetailCookie {
            name: c.name.clone(),
            value: c.value.clone(),
            path: c.path.clone(),
            domain: c.domain.clone(),
            expires: c.expires.clone(),
            http_only: c.http_only,
            secure: c.secure,
        })
        .collect();

    // Parse Query String
    let query_string: Vec<FlowDetailQuery> = req
        .query
        .iter()
        .map(|(k, v)| FlowDetailQuery {
            name: k.clone(),
            value: v.clone(),
        })
        .collect();

    // Parse Post Data
    let post_data = req.body.as_ref().map(|body| {
        let mime_type = req
            .headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == "content-type")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        FlowDetailPostData {
            mime_type,
            text: Some(body.content.clone()),
            encoding: Some(body.encoding.clone()),
        }
    });

    FlowDetailRequest {
        method: req.method.clone(),
        url: req.url.to_string(),
        http_version: req.version.clone(),
        headers,
        cookies,
        query_string,
        post_data,
        body_size: req.body.as_ref().map(|b| b.size).unwrap_or(0),
        headers_size: calc_request_headers_size(req),
    }
}

fn convert_response(resp: &HttpResponse) -> FlowDetailResponse {
    let headers: Vec<FlowDetailHeader> = resp
        .headers
        .iter()
        .map(|(k, v)| FlowDetailHeader {
            name: k.clone(),
            value: v.clone(),
        })
        .collect();

    // Parse Cookies from "Set-Cookie" header
    let cookies: Vec<FlowDetailCookie> = resp
        .cookies
        .iter()
        .map(|c| FlowDetailCookie {
            name: c.name.clone(),
            value: c.value.clone(),
            path: c.path.clone(),
            domain: c.domain.clone(),
            expires: c.expires.clone(),
            http_only: c.http_only,
            secure: c.secure,
        })
        .collect();

    let mime_type = resp
        .headers
        .iter()
        .find(|(k, _)| k.to_lowercase() == "content-type")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();

    let content = FlowDetailContent {
        size: resp.body.as_ref().map(|b| b.size).unwrap_or(0),
        mime_type,
        text: resp.body.as_ref().map(|b| b.content.clone()),
        encoding: resp.body.as_ref().map(|b| b.encoding.clone()),
    };

    let redirect_url = resp
        .headers
        .iter()
        .find(|(k, _)| k.to_lowercase() == "location")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();

    FlowDetailResponse {
        status: resp.status,
        status_text: resp.status_text.clone(),
        http_version: resp.version.clone(),
        headers,
        cookies,
        content,
        redirect_url,
        body_size: resp.body.as_ref().map(|b| b.size).unwrap_or(0),
        headers_size: calc_response_headers_size(resp),
    }
}

fn empty_response() -> FlowDetailResponse {
    FlowDetailResponse {
        status: 0,
        status_text: "".to_string(),
        http_version: "".to_string(),
        headers: vec![],
        cookies: vec![],
        content: FlowDetailContent {
            size: 0,
            mime_type: "".to_string(),
            text: None,
            encoding: None,
        },
        redirect_url: "".to_string(),
        body_size: 0,
        headers_size: 0,
    }
}

fn convert_websocket_message(
    msg: &WebSocketMessage,
    flow_id: &str,
    seq: u32,
) -> FlowDetailWebSocketMessage {
    let type_str = msg.opcode.to_lowercase();
    let from_client = matches!(msg.direction, Direction::ClientToServer);

    FlowDetailWebSocketMessage {
        id: msg.id.to_string(),
        flow_id: flow_id.to_string(),
        seq,
        type_field: type_str,
        from_client,
        content: msg.content.content.clone(),
        encoding: msg.content.encoding.clone(),
        timestamp: msg.timestamp.timestamp_millis() as u64,
        length: msg.content.size,
    }
}

impl From<Flow> for FlowDetail {
    fn from(flow: Flow) -> Self {
        let duration = if let Some(end) = flow.end_time {
            (end - flow.start_time).num_milliseconds() as u64
        } else {
            0
        };

        let flow_id_str = flow.id.to_string();

        let (request, response, is_websocket, websocket_frame_count, websocket_messages) =
            match &flow.layer {
                Layer::Http(http) => {
                    let req = convert_request(&http.request);
                    let resp = if let Some(r) = &http.response {
                        convert_response(r)
                    } else {
                        empty_response()
                    };
                    (req, resp, false, 0, vec![])
                }
                Layer::WebSocket(ws) => {
                    let req = convert_request(&ws.handshake_request);
                    let resp = convert_response(&ws.handshake_response);
                    let messages: Vec<FlowDetailWebSocketMessage> = ws
                        .messages
                        .iter()
                        .enumerate()
                        .map(|(i, msg)| convert_websocket_message(msg, &flow_id_str, i as u32))
                        .collect();
                    (req, resp, true, ws.messages.len() as u32, messages)
                }
                _ => (
                    FlowDetailRequest {
                        method: "UNKNOWN".to_string(),
                        url: "".to_string(),
                        http_version: "".to_string(),
                        headers: vec![],
                        cookies: vec![],
                        query_string: vec![],
                        post_data: None,
                        body_size: 0,
                        headers_size: 0,
                    },
                    empty_response(),
                    false,
                    0,
                    vec![],
                ),
            };

        FlowDetail {
            id: flow.id.to_string(),
            seq: 0, // Placeholder
            started_date_time: flow.start_time.to_rfc3339(),
            time: duration,
            request,
            response,
            timings: FlowDetailTimings {
                send: 0,
                wait: 0,
                receive: 0,
            },
            cache: HashMap::new(),
            _rc: FlowDetailRc {
                client_ip: flow.network.client_ip.clone(),
                is_websocket,
                websocket_frame_count,
                hits: vec![],
                intercept: FlowDetailRcIntercept { intercepted: false },
                websocket_messages,
            },
        }
    }
}
