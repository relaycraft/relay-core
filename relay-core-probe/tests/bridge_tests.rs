//! Bridge contract: MCP traffic from stdin/stdout reaches the daemon, and a daemon that is not
//! there is reported rather than silently swallowed.
//!
//! The mock server speaks the subset of the streamable-HTTP transport the bridge depends on, so
//! the tests pin framing (newline-delimited JSON, SSE responses), session handling and the
//! early-exit behaviour on a stream the daemon keeps open.

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use relay_core_probe::bridge::{BridgeOptions, bridge_streams};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::AsyncWrite;

#[derive(Default)]
struct Recording {
    /// `mcp-session-id` header seen on each request, in arrival order.
    session_headers: Mutex<Vec<Option<String>>>,
    methods: Mutex<Vec<String>>,
}

/// A mock daemon that replies to every request with an SSE stream.
async fn start_mock_daemon(recording: Arc<Recording>, hold_stream_open: bool) -> String {
    let state = MockState {
        recording,
        hold_stream_open,
    };

    let app = Router::new()
        .route("/mcp", post(mock_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock daemon");
    let addr = listener.local_addr().expect("addr");

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{addr}/mcp")
}

#[derive(Clone)]
struct MockState {
    recording: Arc<Recording>,
    hold_stream_open: bool,
}

async fn mock_handler(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let message: serde_json::Value = serde_json::from_slice(&body).expect("json body");

    state.recording.session_headers.lock().unwrap().push(
        headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
    );
    state.recording.methods.lock().unwrap().push(
        message
            .get("method")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
    );

    // A notification gets 202 with no body, the way the MCP streamable-HTTP transport answers it.
    if message.get("id").is_none() {
        return Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .expect("response");
    }

    let id = message.get("id").cloned().unwrap_or(serde_json::json!(1));
    let result = if message.get("method").and_then(|m| m.as_str()) == Some("initialize") {
        serde_json::json!({ "protocolVersion": "2024-11-05", "serverInfo": { "name": "mock" } })
    } else {
        serde_json::json!({ "echo": id })
    };
    let frame = format!(
        "event: message\ndata: {}\n\n",
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
    );

    let body = if state.hold_stream_open {
        // The daemon keeps request streams open with keep-alive comments; the bridge must return
        // once the response has arrived instead of waiting for end-of-stream.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::convert::Infallible>>(4);
        tokio::spawn(async move {
            let _ = tx.send(Ok(frame)).await;
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if tx.send(Ok(": keep-alive\n\n".to_string())).await.is_err() {
                    break;
                }
            }
        });
        use tokio_stream::wrappers::ReceiverStream;
        Body::from_stream(ReceiverStream::new(rx))
    } else {
        Body::from(frame)
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header("mcp-session-id", "sess-1")
        .body(body)
        .expect("response")
}

/// Collects everything the bridge writes, so a test can inspect it after the bridge returns.
#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl SharedWriter {
    fn lines(&self) -> Vec<serde_json::Value> {
        let bytes = self.0.lock().unwrap().clone();
        String::from_utf8(bytes)
            .expect("utf-8 output")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("each line is one JSON-RPC message"))
            .collect()
    }
}

impl AsyncWrite for SharedWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn options(url: String) -> BridgeOptions {
    BridgeOptions {
        mcp_url: url,
        token: None,
    }
}

#[tokio::test]
async fn a_request_reaches_the_daemon_and_its_response_comes_back() {
    let recording = Arc::new(Recording::default());
    let url = start_mock_daemon(recording, false).await;
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
    );

    let out = SharedWriter::default();
    bridge_streams(input.as_bytes(), out.clone(), options(url))
        .await
        .expect("bridge should run to EOF");

    let lines = out.lines();
    assert_eq!(lines.len(), 2, "one response per request: {lines:?}");
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(lines[1]["id"], 2);
    assert_eq!(lines[1]["result"]["echo"], 2);
}

#[tokio::test]
async fn the_session_id_issued_by_initialize_is_echoed_afterwards() {
    let recording = Arc::new(Recording::default());
    let url = start_mock_daemon(recording.clone(), false).await;
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
    );

    bridge_streams(input.as_bytes(), SharedWriter::default(), options(url))
        .await
        .expect("bridge should run to EOF");

    let headers = recording.session_headers.lock().unwrap().clone();
    assert_eq!(headers.len(), 3, "all three messages must be forwarded");
    assert_eq!(headers[0], None, "initialize establishes the session");
    assert_eq!(headers[1].as_deref(), Some("sess-1"));
    assert_eq!(headers[2].as_deref(), Some("sess-1"));
}

#[tokio::test]
async fn a_notification_produces_no_output() {
    let recording = Arc::new(Recording::default());
    let url = start_mock_daemon(recording.clone(), false).await;
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
    );

    let out = SharedWriter::default();
    bridge_streams(input.as_bytes(), out.clone(), options(url))
        .await
        .expect("bridge should run to EOF");

    assert_eq!(out.lines().len(), 1, "only the request has a response");
    assert_eq!(
        recording.methods.lock().unwrap().len(),
        2,
        "the notification must still be forwarded"
    );
}

#[tokio::test]
async fn a_stream_the_daemon_keeps_open_does_not_hold_the_bridge() {
    let recording = Arc::new(Recording::default());
    let url = start_mock_daemon(recording, true).await;
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
    );

    let out = SharedWriter::default();
    let run = bridge_streams(input.as_bytes(), out.clone(), options(url));

    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("the bridge must return once the response arrives, not when the stream ends")
        .expect("bridge should run to EOF");

    assert_eq!(out.lines().len(), 1);
}

#[tokio::test]
async fn an_unreachable_daemon_is_reported_to_the_client() {
    // Nothing is listening here.
    let dead = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("ephemeral port");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        format!("http://{addr}/mcp")
    };

    let out = SharedWriter::default();
    let input = concat!(r#"{"jsonrpc":"2.0","id":9,"method":"tools/list"}"#, "\n");

    bridge_streams(input.as_bytes(), out.clone(), options(dead))
        .await
        .expect("bridge should run to EOF");

    let lines = out.lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["id"], 9, "the error must answer the request");
    assert!(
        lines[0]["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("unreachable")),
        "the client must learn the daemon is gone: {lines:?}"
    );
}

#[tokio::test]
async fn malformed_input_is_skipped_without_killing_the_stream() {
    let recording = Arc::new(Recording::default());
    let url = start_mock_daemon(recording, false).await;
    let input = concat!(
        "not json at all\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
    );

    let out = SharedWriter::default();

    bridge_streams(input.as_bytes(), out.clone(), options(url))
        .await
        .expect("a malformed line must not end the bridge");

    assert_eq!(out.lines().len(), 1);
}
