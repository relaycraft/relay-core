use serde::{Deserialize, Serialize, Serializer, Deserializer};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use url::Url;
use std::sync::Arc;
use std::any::Any;

/// Trait for custom protocol layers to implement
pub trait ProtocolLayer: Any + Send + Sync + std::fmt::Debug {
    fn protocol_name(&self) -> &str;
    fn as_any(&self) -> &dyn Any;
    fn to_json(&self) -> serde_json::Value;
}

// Custom serializer for Arc<dyn ProtocolLayer>
fn serialize_custom_layer<S>(layer: &Arc<dyn ProtocolLayer>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use serde::ser::SerializeStruct;
    let mut state = serializer.serialize_struct("CustomLayer", 2)?;
    state.serialize_field("protocol", layer.protocol_name())?;
    state.serialize_field("data", &layer.to_json())?;
    state.end()
}

// Custom deserializer for Arc<dyn ProtocolLayer>
fn deserialize_custom_layer<'de, D>(deserializer: D) -> Result<Arc<dyn ProtocolLayer>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    
    // Try to extract protocol and data
    let protocol = value.get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
        
    let data = value.get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    Ok(Arc::new(GenericProtocolLayer { 
        protocol,
        data 
    }))
}

#[derive(Debug)]
struct GenericProtocolLayer {
    protocol: String,
    data: serde_json::Value,
}

impl ProtocolLayer for GenericProtocolLayer {
    fn protocol_name(&self) -> &str {
        &self.protocol
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn to_json(&self) -> serde_json::Value {
        self.data.clone()
    }
}

/// The central data structure representing a captured traffic flow.
/// Designed to support L3/L4 layers initially, with L7 (HTTP) as a specialized layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flow {
    pub id: Uuid,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    
    /// L3/L4 Connection Info (IP, Port, Protocol)
    pub network: NetworkInfo,
    
    /// The application layer protocol detected or parsed
    pub layer: Layer,

    /// Analysis tags (e.g., "error", "large-body", "injected")
    pub tags: Vec<String>,
    
    /// Internal processing metadata (not necessarily for UI)
    #[serde(skip)]
    pub meta: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInfo {
    pub client_ip: String,
    pub client_port: u16,
    pub server_ip: String,
    pub server_port: u16,
    pub protocol: TransportProtocol,
    pub tls: bool,
    pub tls_version: Option<String>,
    pub sni: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TransportProtocol {
    TCP,
    UDP,
    ICMP,
    Unknown,
}

/// The specific application layer data.
/// Using an enum allows us to support HTTP now, but easily add DNS, WebSocket,
/// or raw TCP streams later without breaking the top-level Flow structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Layer {
    Http(HttpLayer),
    WebSocket(WebSocketLayer),
    Tcp(TcpLayer), // For raw TCP flows without L7 parsing
    Udp(UdpLayer), // For raw UDP flows
    Quic(QuicLayer), // For QUIC flows
    #[serde(serialize_with = "serialize_custom_layer", deserialize_with = "deserialize_custom_layer")]
    Custom(Arc<dyn ProtocolLayer>),
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpLayer {
    pub payload_size: usize,
    pub packet_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuicLayer {
    pub sni: Option<String>,
    pub alpn: Option<String>,
    pub version: Option<String>,
}

// --- HTTP Layer ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpLayer {
    pub request: HttpRequest,
    pub response: Option<HttpResponse>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: Url,
    pub version: String, // HTTP/1.1, HTTP/2
    pub headers: Vec<(String, String)>, // Vec preserves order, unlike HashMap
    pub cookies: Vec<Cookie>,
    pub query: Vec<(String, String)>,
    pub body: Option<BodyData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<Cookie>,
    pub body: Option<BodyData>,
    pub timing: ResponseTiming,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub path: Option<String>,
    pub domain: Option<String>,
    pub expires: Option<String>,
    pub http_only: Option<bool>,
    pub secure: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseTiming {
    pub time_to_first_byte: Option<u64>, // ms
    pub time_to_last_byte: Option<u64>,  // ms
    /// TCP connect time in milliseconds for the upstream connection (new connections only).
    pub connect_time_ms: Option<u64>,
    /// TLS handshake time in milliseconds for the upstream connection (new connections only).
    pub ssl_time_ms: Option<u64>,
}

// --- WebSocket Layer ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketLayer {
    pub handshake_request: HttpRequest,
    pub handshake_response: HttpResponse,
    pub messages: Vec<WebSocketMessage>,
    pub closed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketMessage {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub direction: Direction, // ClientToServer, ServerToClient
    pub content: BodyData,
    pub opcode: String, // Text, Binary, Ping, Pong
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Direction {
    ClientToServer,
    ServerToClient,
}

// --- Raw TCP Layer ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpLayer {
    pub bytes_up: u64,
    pub bytes_down: u64,
    // Future: packets or data chunks
}

// --- Shared ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyData {
    /// Encoding (e.g., "utf-8", "base64")
    pub encoding: String,
    /// The actual content. If binary, it should be base64 encoded string.
    pub content: String, 
    /// Original size in bytes before any decoding/unzipping
    pub size: u64,
}

/// Enum for Incremental Flow Updates to avoid cloning the entire Flow object
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum FlowUpdate {
    /// Full Flow update (e.g., initial creation, request/response headers)
    Full(Box<Flow>),
    /// Incremental WebSocket Message
    WebSocketMessage { 
        flow_id: String, 
        message: WebSocketMessage 
    },
    /// Incremental HTTP Body Update (Request or Response)
    HttpBody {
        flow_id: String,
        direction: Direction,
        body: BodyData,
    },
}

