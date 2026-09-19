//! The stdio bridge: an MCP client on stdin/stdout, the daemon's MCP endpoint on the other side.
//!
//! Decision [`0007`](../../docs/decisions/0007-daemon-control-plane.md). A stdio MCP client cannot
//! be pointed at a URL, so it still needs a child process — but that child is a *transport*, not an
//! engine. Connecting one bridge starts no proxy and owns no state: every tool call is forwarded to
//! the daemon that owns them, so several clients (a CLI, a bridge, the Web UI) share one engine,
//! one flow history and one rule set.
//!
//! Forwarding the protocol rather than re-implementing the tools is deliberate. There is exactly
//! one definition of what `search_flows` means — the server's — so a tool cannot mean one thing
//! over HTTP and another over stdio.

use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// JSON-RPC code for "the thing behind this bridge is not available".
const BRIDGE_ERROR_CODE: i64 = -32000;

#[derive(Debug, Clone)]
pub struct BridgeOptions {
    /// Full URL of the daemon's MCP endpoint, e.g. `http://127.0.0.1:18083/mcp`.
    pub mcp_url: String,
    /// Control-plane bearer token, when the daemon requires one.
    pub token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("MCP client stream error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON-RPC message from the MCP client: {0}")]
    Malformed(String),
}

/// Run the bridge over this process's stdin/stdout.
pub async fn run_stdio_bridge(options: BridgeOptions) -> Result<(), BridgeError> {
    bridge_streams(tokio::io::stdin(), tokio::io::stdout(), options).await
}

/// Run the bridge over arbitrary streams (used by tests to drive it without a real client).
pub async fn bridge_streams<R, W>(
    reader: R,
    writer: W,
    options: BridgeOptions,
) -> Result<(), BridgeError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let state = Arc::new(BridgeState {
        http: reqwest::Client::builder()
            // Bounded so a hung daemon cannot hold a client's request forever.
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_default(),
        options,
        session: Mutex::new(None),
        out: Mutex::new(writer),
    });

    // Requests run concurrently once the session exists — MCP matches responses by id, not by
    // order — but their handles are kept so that closing stdin drains what is still in flight
    // instead of dropping responses on the floor.
    let mut in_flight = tokio::task::JoinSet::new();

    let mut lines = BufReader::new(reader).split(b'\n');
    while let Some(line) = lines.next_segment().await? {
        let trimmed = trim_ascii(&line);
        if trimmed.is_empty() {
            continue;
        }

        let message: Value = match serde_json::from_slice(trimmed) {
            Ok(message) => message,
            Err(error) => {
                // Reported on stderr rather than stdout: stdout is the MCP channel, and a
                // syntactically invalid message has no id to answer.
                eprintln!("relay-core mcp bridge: ignoring malformed message: {error}");
                continue;
            }
        };

        // `initialize` establishes the session every later request needs, so it is awaited before
        // anything else is forwarded.
        let session_ready = state.session.lock().await.is_some();
        if session_ready {
            let state = state.clone();
            in_flight.spawn(async move {
                if let Err(error) = state.forward(message).await {
                    eprintln!("relay-core mcp bridge: {error}");
                }
            });
        } else {
            state.forward(message).await?;
        }
    }

    while in_flight.join_next().await.is_some() {}

    Ok(())
}

struct BridgeState<W> {
    http: reqwest::Client,
    options: BridgeOptions,
    /// Session id issued by the daemon; must be echoed on every later request.
    session: Mutex<Option<String>>,
    out: Mutex<W>,
}

impl<W: tokio::io::AsyncWrite + Unpin + Send> BridgeState<W> {
    async fn forward(&self, message: Value) -> Result<(), BridgeError> {
        let id = message.get("id").cloned();
        let session = self.session.lock().await.clone();

        let mut request = self
            .http
            .post(&self.options.mcp_url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(message.to_string());

        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        if let Some(token) = self.options.token.as_deref() {
            request = request.bearer_auth(token);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                // Silence is the failure this design exists to remove: an unreachable daemon must
                // answer the client so the agent can report it instead of waiting forever.
                self.reply_with_error(id, &format!("RelayCore daemon unreachable: {error}"))
                    .await;
                return Ok(());
            }
        };

        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
        {
            *self.session.lock().await = Some(session.to_string());
        }

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            self.reply_with_error(
                id,
                &format!("RelayCore daemon refused the request ({status}): {body}"),
            )
            .await;
            return Ok(());
        }

        // Notifications are answered with 202 and carry no body.
        if status == reqwest::StatusCode::ACCEPTED {
            return Ok(());
        }

        let is_sse = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/event-stream"));

        if is_sse {
            self.relay_event_stream(response, id.as_ref()).await?;
        } else {
            let body = response.text().await.unwrap_or_default();
            let text = body.trim();
            if !text.is_empty() {
                self.write_message(text).await?;
            }
        }

        Ok(())
    }

    /// Relay an SSE response until the answer to this request arrives.
    ///
    /// Returning early matters: the daemon keeps request streams open with keep-alive comments, so
    /// waiting for end-of-stream would hang the bridge forever.
    async fn relay_event_stream(
        &self,
        mut response: reqwest::Response,
        request_id: Option<&Value>,
    ) -> Result<(), BridgeError> {
        let mut buffer: Vec<u8> = Vec::new();

        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => return Ok(()),
                Err(error) => {
                    eprintln!("relay-core mcp bridge: stream error: {error}");
                    return Ok(());
                }
            };
            buffer.extend_from_slice(&chunk);

            for payload in take_sse_payloads(&mut buffer) {
                self.write_message(&payload).await?;
                if request_id.is_some_and(|id| is_response_for(&payload, id)) {
                    return Ok(());
                }
            }
        }
    }

    async fn write_message(&self, message: &str) -> Result<(), BridgeError> {
        let mut out = self.out.lock().await;
        out.write_all(message.as_bytes()).await?;
        out.write_all(b"\n").await?;
        out.flush().await?;
        Ok(())
    }

    async fn reply_with_error(&self, id: Option<Value>, message: &str) {
        // A notification has no id and cannot be answered; the client's stderr is the only place
        // left to say what happened.
        let Some(id) = id else {
            eprintln!("relay-core mcp bridge: {message}");
            return;
        };

        let error = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": BRIDGE_ERROR_CODE, "message": message },
        });
        if let Err(write_error) = self.write_message(&error.to_string()).await {
            eprintln!("relay-core mcp bridge: could not report {message}: {write_error}");
        }
    }
}

/// Strip trailing `\r` and whitespace from a line read as bytes.
fn trim_ascii(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] as char).is_whitespace() {
        end -= 1;
    }
    &line[..end]
}

/// Pull every complete SSE event out of `buffer`, leaving any partial event behind.
///
/// Events are `data:` lines terminated by a blank line; comments (`: keep-alive`) carry no payload
/// and are dropped.
fn take_sse_payloads(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut payloads = Vec::new();

    while let Some(end) = find_event_boundary(buffer) {
        let event: Vec<u8> = buffer.drain(..end).collect();
        // Drop the blank line that terminated the event.
        let separator = if buffer.starts_with(b"\r\n\r\n") {
            4
        } else {
            2
        };
        buffer.drain(..separator);

        let text = String::from_utf8_lossy(&event).to_string();
        let data = text
            .lines()
            .filter_map(|line| {
                let line = line.trim_end_matches('\r');
                line.strip_prefix("data:")
                    .map(|value| value.strip_prefix(' ').unwrap_or(value).to_string())
            })
            .collect::<Vec<_>>()
            .join("\n");

        if !data.trim().is_empty() {
            payloads.push(data);
        }
    }

    payloads
}

fn find_event_boundary(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .or_else(|| buffer.windows(4).position(|window| window == b"\r\n\r\n"))
}

/// Does this payload answer `id`?
fn is_response_for(payload: &str, id: &Value) -> bool {
    serde_json::from_str::<Value>(payload)
        .ok()
        .is_some_and(|value| {
            value.get("id") == Some(id)
                && (value.get("result").is_some() || value.get("error").is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_payloads_are_extracted_and_completed_events_removed() {
        let mut buffer =
            b"event: message\ndata: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: {\"parti".to_vec();

        let payloads = take_sse_payloads(&mut buffer);

        assert_eq!(
            payloads,
            vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]
        );
        assert_eq!(
            String::from_utf8_lossy(&buffer),
            "data: {\"parti",
            "a partial event must stay buffered until its terminator arrives"
        );
    }

    #[test]
    fn keep_alive_comments_produce_no_payload() {
        let mut buffer = b": keep-alive\n\n".to_vec();
        assert!(take_sse_payloads(&mut buffer).is_empty());
    }

    #[test]
    fn multi_line_data_is_joined_the_way_sse_specifies() {
        let mut buffer = b"data: {\"a\":\ndata: 1}\n\n".to_vec();
        assert_eq!(
            take_sse_payloads(&mut buffer),
            vec!["{\"a\":\n1}".to_string()]
        );
    }

    #[test]
    fn a_response_is_matched_by_id_not_by_order() {
        let id = serde_json::json!(7);
        assert!(is_response_for(
            r#"{"jsonrpc":"2.0","id":7,"result":{}}"#,
            &id
        ));
        assert!(!is_response_for(
            r#"{"jsonrpc":"2.0","id":8,"result":{}}"#,
            &id
        ));
        // A server-initiated notification on the same stream is relayed but does not end the wait.
        assert!(!is_response_for(
            r#"{"jsonrpc":"2.0","method":"notifications/message","params":{}}"#,
            &id
        ));
    }

    #[test]
    fn trailing_carriage_returns_are_trimmed() {
        assert_eq!(trim_ascii(b"{\"a\":1}\r\n"), b"{\"a\":1}");
        assert_eq!(trim_ascii(b"   "), b"");
    }
}
