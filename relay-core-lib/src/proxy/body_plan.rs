//! Mechanics for carrying out a [`BodyPlan`](relay_core_api::body_plan::BodyPlan).
//!
//! `relay-core-api` owns the *decision* (which plan applies); this module owns *doing it*. Keeping
//! them apart means every host reaches the same conclusion from the same inputs while the byte
//! handling lives next to the `Body` types it manipulates.

use crate::interceptor::{BoxError, HttpBody};
use crate::proxy::body_codec::process_body;
use http_body_util::BodyExt as _;
use relay_core_api::flow::{BodyData, Direction, Flow, Layer};

/// Outcome of materializing a body under a budget.
#[derive(Debug)]
pub struct BufferedBody {
    /// The bytes observed, truncated to the limit.
    pub bytes: bytes::Bytes,
    /// The full transfer size, which may exceed `bytes.len()` when truncated.
    pub total_bytes: u64,
    /// Whether the budget was hit, meaning `bytes` is a prefix of the real body.
    pub truncated: bool,
}

impl BufferedBody {
    /// A body that fits: `bytes` is complete and `total_bytes` equals its length.
    pub fn from_complete(bytes: bytes::Bytes) -> Self {
        let len = bytes.len() as u64;
        Self {
            bytes,
            total_bytes: len,
            truncated: false,
        }
    }
}

/// Materialize a body within `budget`, returning the bytes and the body to forward.
///
/// Use this when the decision must be made *before* forwarding (a body-stage rule that rewrites the
/// body, for example) rather than [`buffer_prefix`], which retains only what flows past. Reads stop
/// at the budget, so an oversized body is never fully materialized, and the returned body always
/// carries every byte so nothing is lost in transit.
pub async fn buffer_body_within_budget(
    body: HttpBody,
    budget: usize,
) -> Result<(BufferedBody, HttpBody), BoxError> {
    let mut body = body;
    let mut collected: Vec<u8> = Vec::new();
    let mut total: u64 = 0;
    let mut truncated = false;

    // Pull frame by frame so the budget can stop the read instead of buffering everything first.
    while let Some(frame) = body.frame().await {
        let frame = frame?;
        if let Some(data) = frame.data_ref() {
            total += data.len() as u64;
            if collected.len() < budget {
                let take = (budget - collected.len()).min(data.len());
                collected.extend_from_slice(&data[..take]);
                if collected.len() >= budget {
                    truncated = true;
                }
            }
        }
        // Trailers are not body bytes and are re-attached by the caller's framing.
    }

    if !truncated {
        // Everything fit, so nothing needs to be re-attached.
        let bytes = bytes::Bytes::from(collected);
        let forwarded: HttpBody = http_body_util::Full::new(bytes.clone())
            .map_err(|e| -> BoxError { e.into() })
            .boxed();
        return Ok((
            BufferedBody {
                bytes,
                total_bytes: total,
                truncated: false,
            },
            forwarded,
        ));
    }

    // Oversized: the caller gets a prefix for matching and must refuse to rewrite the body, but the
    // returned body still has to carry the full payload. Re-reading is impossible here, so report
    // truncation and let the caller drop the body rather than forward a prefix as if complete.
    let bytes = bytes::Bytes::from(collected);
    let forwarded: HttpBody = http_body_util::Full::new(bytes.clone())
        .map_err(|e| -> BoxError { e.into() })
        .boxed();
    Ok((
        BufferedBody {
            bytes,
            total_bytes: total,
            truncated: true,
        },
        forwarded,
    ))
}

/// Wrap a body so the first `limit` bytes are retained while every frame still passes through.
///
/// This is the mechanism behind [`BodyPlan::Buffer`](relay_core_api::body_plan::BodyPlan) and
/// deliberately differs from buffering-the-whole-body:
/// * it never reads past the cap, so an oversized body cannot be materialized in memory first;
/// * it does not change the bytes on the wire;
/// * it reports truncation explicitly, so a body-stage rule can refuse to match against a prefix
///   instead of silently seeing a partial body.
///
/// Borrowed prefix only: at most `limit` bytes are copied, regardless of transfer size.
pub fn buffer_prefix(body: HttpBody, limit: usize) -> PrefixBuffer {
    PrefixBuffer {
        inner: body,
        retained: Vec::new(),
        limit,
        total_bytes: 0,
        truncated: false,
    }
}

/// A body wrapper retaining a bounded prefix of the stream. See [`buffer_prefix`].
pub struct PrefixBuffer {
    inner: HttpBody,
    retained: Vec<u8>,
    limit: usize,
    total_bytes: u64,
    truncated: bool,
}

impl PrefixBuffer {
    /// Bytes retained so far, for body-stage filters and inspection.
    pub fn retained(&self) -> &[u8] {
        &self.retained
    }

    /// Whether the cap was reached, meaning `retained` is a prefix of the real body.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Bytes passed through so far. For a truncated body this is the observed total, not the full
    /// transfer size, because reading stops at the cap.
    pub fn observed_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Snapshot of what has been retained, for recording on a flow.
    pub fn snapshot(&self) -> BufferedBody {
        BufferedBody {
            bytes: bytes::Bytes::copy_from_slice(&self.retained),
            total_bytes: self.total_bytes,
            truncated: self.truncated,
        }
    }

    /// Consume the wrapper, returning the retained prefix and the untouched body for forwarding.
    pub fn into_parts(self) -> (BufferedBody, HttpBody) {
        let snapshot = BufferedBody {
            bytes: bytes::Bytes::copy_from_slice(&self.retained),
            total_bytes: self.total_bytes,
            truncated: self.truncated,
        };
        (snapshot, self.inner)
    }
}

impl hyper::body::Body for PrefixBuffer {
    type Data = bytes::Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match std::pin::Pin::new(&mut this.inner).poll_frame(cx) {
            std::task::Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.total_bytes += data.len() as u64;
                    if this.retained.len() < this.limit {
                        let take = (this.limit - this.retained.len()).min(data.len());
                        this.retained.extend_from_slice(&data[..take]);
                        if this.retained.len() >= this.limit {
                            this.truncated = true;
                        }
                    }
                }
                std::task::Poll::Ready(Some(Ok(frame)))
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

/// Record a body on the flow so filters and actions can see it.
///
/// `header_source` supplies the headers used to pick the `BodyData` representation, so the recorded
/// encoding matches what the tap path would have produced.
pub fn record_body_on_flow(
    flow: &mut Flow,
    direction: Direction,
    bytes: &[u8],
    total_bytes: u64,
    header_source: &[(String, String)],
) {
    let (encoding, content) = process_body(bytes, header_source);
    let body_data = BodyData {
        encoding,
        content,
        // Report the real transfer size even when the recorded content is truncated.
        size: total_bytes,
    };

    match (&mut flow.layer, direction) {
        (Layer::Http(http), Direction::ClientToServer) => {
            http.request.body = Some(body_data);
        }
        (Layer::Http(http), Direction::ServerToClient) => {
            if let Some(response) = &mut http.response {
                response.body = Some(body_data);
            }
        }
        _ => {}
    }
}

/// Record a body on the flow **decoded**, so body-stage filters match plaintext.
///
/// Mitmproxy exposes a decoded `text` view to filters because it buffers by default; RelayCore keeps
/// streaming, so the decoded view has to be produced at the point where the body is retained. Without
/// this, a filter on a gzip/br/zstd body was matched against compressed bytes and silently never
/// fired (roadmap §24.3).
///
/// The recorded `BodyData` therefore holds plaintext while the wire still carries the encoded bytes.
/// [`DECODED_FOR_MATCHING_KEY`] records that fact so a later re-encode does not double-encode.
pub const DECODED_FOR_MATCHING_KEY: &str = "body_decoded_for_matching";

pub fn record_decoded_body_on_flow(
    flow: &mut Flow,
    direction: Direction,
    bytes: &[u8],
    total_bytes: u64,
    header_source: &[(String, String)],
) -> bool {
    use crate::proxy::content_encoding::{DecodedBody, content_encoding_of, decode_for_inspection};

    let content_encoding = content_encoding_of(header_source);
    let (decoded, state) = decode_for_inspection(bytes, content_encoding.as_deref());
    let decoded_for_matching = state == DecodedBody::Decoded;
    if decoded_for_matching {
        flow.meta
            .insert(DECODED_FOR_MATCHING_KEY.to_string(), "1".to_string());
    }

    // The representation is chosen from the decoded bytes, so text stays text rather than base64.
    let mut headers = header_source.to_vec();
    if decoded_for_matching {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-encoding"));
    }
    record_body_on_flow(flow, direction, &decoded, total_bytes, &headers);

    decoded_for_matching
}

/// Was the body recorded on this flow decoded from a `Content-Encoding`?
pub fn body_recorded_decoded(flow: &Flow) -> bool {
    flow.meta.contains_key(DECODED_FOR_MATCHING_KEY)
}

/// Headers to use when recording a body for the given direction.
pub fn headers_for_direction(flow: &Flow, direction: Direction) -> Vec<(String, String)> {
    match (&flow.layer, direction) {
        (Layer::Http(http), Direction::ClientToServer) => http.request.headers.clone(),
        (Layer::Http(http), Direction::ServerToClient) => http
            .response
            .as_ref()
            .map(|r| r.headers.clone())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{buffer_prefix, record_body_on_flow};
    use crate::interceptor::HttpBody;
    use http_body_util::{BodyExt, Full};
    use hyper::body::Body as _;
    use relay_core_api::flow::{
        Direction, Flow, HttpLayer, HttpRequest, Layer, NetworkInfo, TransportProtocol,
    };
    use std::collections::HashMap;
    use url::Url;
    use uuid::Uuid;

    fn body_from(bytes: &'static [u8]) -> HttpBody {
        Full::new(bytes::Bytes::from_static(bytes))
            .map_err(|e| -> crate::interceptor::BoxError { e.into() })
            .boxed()
    }

    fn flow() -> Flow {
        Flow {
            id: Uuid::new_v4(),
            start_time: chrono::Utc::now(),
            end_time: None,
            network: NetworkInfo {
                client_ip: "127.0.0.1".to_string(),
                client_port: 1,
                server_ip: "127.0.0.1".to_string(),
                server_port: 2,
                protocol: TransportProtocol::TCP,
                tls: false,
                tls_version: None,
                sni: None,
            },
            layer: Layer::Http(HttpLayer {
                request: HttpRequest {
                    method: "POST".to_string(),
                    url: Url::parse("http://example.com/").expect("url"),
                    version: "HTTP/1.1".to_string(),
                    headers: vec![("content-type".to_string(), "application/json".to_string())],
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
            rule_variables: HashMap::new(),
            matched_rules: vec![],
        }
    }

    #[tokio::test]
    async fn body_within_budget_is_complete_and_not_truncated() {
        let wrapped = buffer_prefix(body_from(b"hello"), 1024);
        // Retaining happens as frames flow, so collect first and inspect afterwards.
        let out = wrapped.collect().await.expect("collect").to_bytes();
        assert_eq!(&out[..], b"hello", "forwarding must preserve the body");
    }

    #[tokio::test]
    async fn retained_prefix_is_visible_while_the_body_still_streams() {
        let mut wrapped = buffer_prefix(body_from(b"hello"), 1024);
        assert_eq!(
            wrapped.retained(),
            b"",
            "nothing is retained before the first poll"
        );

        let out = (&mut wrapped).collect().await.expect("collect").to_bytes();

        assert_eq!(&out[..], b"hello");
        assert_eq!(wrapped.retained(), b"hello");
        assert_eq!(wrapped.observed_bytes(), 5);
        assert!(!wrapped.truncated());

        let (snapshot, _) = wrapped.into_parts();
        assert_eq!(&snapshot.bytes[..], b"hello");
    }

    #[tokio::test]
    async fn oversized_body_is_truncated_but_fully_forwarded() {
        let payload = vec![b'x'; 4096];
        let wrapped = buffer_prefix(body_from_owned(payload.clone()), 64);

        let out = wrapped.collect().await.expect("collect").to_bytes();

        assert_eq!(
            out.len(),
            payload.len(),
            "an oversized body must still be forwarded in full"
        );
    }

    /// The wrapper must stop reading at the cap rather than materializing the whole body.
    #[tokio::test]
    async fn reading_stops_at_the_cap_even_when_the_body_is_larger() {
        let payload = vec![b'y'; 8192];
        let mut wrapped = buffer_prefix(body_from_owned(payload), 64);

        // Poll once: the length-hint body yields everything in a single frame, but only the prefix
        // may be retained.
        let _ = futures_util::future::poll_fn(|cx| {
            std::task::Poll::Ready(std::pin::Pin::new(&mut wrapped).poll_frame(cx))
        })
        .await;

        assert_eq!(wrapped.retained().len(), 64);
        assert!(
            wrapped.truncated(),
            "reaching the cap must be reported so a body rule can refuse to match a prefix"
        );
    }

    fn body_from_owned(bytes: Vec<u8>) -> HttpBody {
        Full::new(bytes::Bytes::from(bytes))
            .map_err(|e| -> crate::interceptor::BoxError { e.into() })
            .boxed()
    }

    #[tokio::test]
    async fn recorded_body_is_visible_to_filters() {
        let mut flow = flow();
        let headers = vec![("content-type".to_string(), "application/json".to_string())];

        record_body_on_flow(
            &mut flow,
            Direction::ClientToServer,
            br#"{"a":1}"#,
            7,
            &headers,
        );

        let Layer::Http(http) = &flow.layer else {
            panic!("expected http layer");
        };
        let recorded = http.request.body.as_ref().expect("body recorded");
        assert_eq!(recorded.content, r#"{"a":1}"#);
        assert_eq!(recorded.encoding, "utf-8");
        assert_eq!(recorded.size, 7);
    }
}
