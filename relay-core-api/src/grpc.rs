//! gRPC message framing.
//!
//! A gRPC body is not one payload: it is a sequence of length-prefixed messages, each preceded by a
//! five-byte header — one compression flag and a four-byte big-endian length (gRPC over HTTP/2,
//! "Length-Prefixed-Message"). Without splitting it, a captured call is an opaque blob of base64 and
//! a consumer can only say that *something* was sent.
//!
//! This module deliberately stops at the framing. Decoding the payload means protobuf, which needs
//! the service's schema; there is none here, so messages are described by size and compression flag
//! rather than guessed at. A unary call is then legible as "one 42-byte message", streaming as
//! "seven messages", and a truncated capture as exactly that instead of a silent short read.

use serde::{Deserialize, Serialize};

/// One length-prefixed message inside a gRPC body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrpcMessage {
    /// Position in the body, from zero.
    pub index: u32,
    /// Payload length in bytes, as declared by the frame.
    pub length: u32,
    /// Whether the frame says the payload is compressed (the first byte, non-zero).
    pub compressed: bool,
    /// The payload as text, when it is text.
    ///
    /// This is what makes a message readable rather than merely sized. It matters most for
    /// `application/grpc+json`, where the payload *is* text, and for any payload carrying UTF-8 —
    /// Chinese, for instance, is unreadable through a length and a base64 blob.
    ///
    /// `None` means the payload is binary, compressed, or larger than the preview budget: protobuf
    /// is usually binary, and pretending otherwise would produce mojibake rather than information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Protobuf payload, filled on a detail read.
    ///
    /// Capture stores framing only. A detail read may set this from a `.proto` directory, or from
    /// the wire format when no schema matches. It is not written back to the capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoded: Option<GrpcDecoded>,
}

/// Where a protobuf view came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrpcDecodeSource {
    /// A user `.proto` named the message, via the call's `:path`.
    Schema,
    /// No schema matched. Field numbers and wire types only.
    Wire,
}

/// One gRPC message payload read as protobuf.
///
/// `value` is JSON. With a schema it uses proto3 JSON names. Without one, object keys are field
/// numbers, length-delimited text is a string, and other bytes are base64.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrpcDecoded {
    pub source: GrpcDecodeSource,
    /// Fully qualified message name when `source` is [`GrpcDecodeSource::Schema`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    /// Set when this message could not be decoded. The capture itself is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Largest payload rendered as text in a message preview.
///
/// Bounded because this is a capture aid, not a body store: the bytes are recorded separately, and an
/// unbounded preview would duplicate a large payload into every serialised Flow.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 4096;

/// What a body turned out to contain, structurally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrpcBody {
    /// Messages in arrival order.
    pub messages: Vec<GrpcMessage>,
    /// Bytes that were not part of a complete message — a capture cut short, or trailing junk.
    ///
    /// Reported rather than ignored: a partial capture that looks complete is worse than one that
    /// admits what it lost.
    pub unparsed_bytes: usize,
    /// Why a schema was not applied on a detail read.
    ///
    /// Set when the `.proto` directory failed to compile, or when it compiled and no `rpc` matches
    /// this call. Absent when there is no schema, and when one matched. Capture leaves it empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_note: Option<String>,
}

impl GrpcBody {
    /// Total declared payload bytes across all messages.
    pub fn total_payload_bytes(&self) -> u64 {
        self.messages.iter().map(|m| u64::from(m.length)).sum()
    }

    /// Is this a unary call (exactly one request or response message)?
    pub fn is_unary(&self) -> bool {
        self.messages.len() == 1
    }
}

/// Why a body could not be read as gRPC framing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrpcParseError {
    /// A frame declared a payload longer than the bytes available.
    Incomplete {
        /// Index of the incomplete message.
        index: u32,
        /// Bytes the frame declared.
        declared: u32,
        /// Bytes actually available.
        available: usize,
    },
}

impl std::fmt::Display for GrpcParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrpcParseError::Incomplete {
                index,
                declared,
                available,
            } => write!(
                f,
                "gRPC message {index} declares {declared} bytes but only {available} are present"
            ),
        }
    }
}

impl std::error::Error for GrpcParseError {}

/// Is this content type a gRPC one?
///
/// gRPC uses `application/grpc` with an optional suffix for the message encoding
/// (`application/grpc+proto`, `application/grpc+json`), so the prefix identifies the family.
///
/// gRPC-Web is deliberately **excluded**. It reuses the length-prefixed message framing but not the
/// trailer mechanism: its status travels in a body-encoded trailer frame rather than in HTTP
/// trailers, so treating it as gRPC would report the framing of one protocol alongside the status
/// semantics of another. It is unrecognised rather than half-recognised.
pub fn is_grpc_content_type(content_type: &str) -> bool {
    let content_type = content_type.trim().to_ascii_lowercase();
    content_type.starts_with("application/grpc")
        && !content_type.starts_with("application/grpc-web")
}

/// Split a body into its gRPC messages.
///
/// Trailing bytes that cannot form a complete message are counted in
/// [`GrpcBody::unparsed_bytes`] rather than dropped, and a frame that declares more payload than the
/// body holds is an error: an incomplete capture must not be presented as a complete call.
pub fn parse_messages(body: &[u8]) -> Result<GrpcBody, GrpcParseError> {
    split_messages(body, true)
}

/// Messages that have fully arrived, leaving a trailing partial frame unparsed.
///
/// A stream that is still open ends mid-message. That remainder is not a failed call yet: the
/// messages before it are complete, and the leftover bytes are what has not arrived.
pub fn parse_arrived_messages(body: &[u8]) -> GrpcBody {
    split_messages(body, false).unwrap_or(GrpcBody {
        messages: Vec::new(),
        unparsed_bytes: body.len(),
        schema_note: None,
    })
}

/// `grpc-status` and `grpc-message` from response trailers.
///
/// Status `0` is success. `grpc-message` is percent-encoded UTF-8, so a space arrives as `%20`.
pub fn outcome_from_trailers(trailers: &[(String, String)]) -> (Option<i32>, Option<String>) {
    let status = trailers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("grpc-status"))
        .and_then(|(_, value)| value.trim().parse::<i32>().ok());
    let message = trailers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("grpc-message"))
        .map(|(_, value)| decode_grpc_message(value.trim()))
        .filter(|value| !value.is_empty());
    (status, message)
}

fn split_messages(body: &[u8], strict: bool) -> Result<GrpcBody, GrpcParseError> {
    const PREFIX_LEN: usize = 5;

    let mut messages = Vec::new();
    let mut offset = 0usize;
    let mut index = 0u32;

    while offset + PREFIX_LEN <= body.len() {
        let compressed = body[offset] != 0;
        let length = u32::from_be_bytes([
            body[offset + 1],
            body[offset + 2],
            body[offset + 3],
            body[offset + 4],
        ]);
        let payload_start = offset + PREFIX_LEN;
        let payload_end = payload_start + length as usize;

        if payload_end > body.len() {
            if !strict {
                return Ok(GrpcBody {
                    messages,
                    unparsed_bytes: body.len() - offset,
                    schema_note: None,
                });
            }
            return Err(GrpcParseError::Incomplete {
                index,
                declared: length,
                available: body.len() - payload_start,
            });
        }

        let payload = &body[payload_start..payload_end];
        // Only for uncompressed payloads that are text: a compressed payload is not text until it is
        // inflated, and inflating it here would report content the frame did not contain.
        let text = if compressed || payload.len() > MAX_MESSAGE_TEXT_BYTES {
            None
        } else {
            std::str::from_utf8(payload).ok().map(str::to_string)
        };

        messages.push(GrpcMessage {
            index,
            length,
            compressed,
            text,
            decoded: None,
        });
        offset = payload_end;
        index += 1;
    }

    Ok(GrpcBody {
        messages,
        unparsed_bytes: body.len() - offset,
        schema_note: None,
    })
}

/// Percent-decode a `grpc-message` trailer. Invalid escapes are left as written.
fn decode_grpc_message(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        GrpcBody, GrpcMessage, GrpcParseError, is_grpc_content_type, outcome_from_trailers,
        parse_arrived_messages, parse_messages,
    };

    fn frame(compressed: bool, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![u8::from(compressed)];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn grpc_content_types_are_recognised() {
        for content_type in [
            "application/grpc",
            "application/grpc+proto",
            "application/grpc+json",
            "APPLICATION/GRPC",
            "application/grpc; charset=utf-8",
        ] {
            assert!(
                is_grpc_content_type(content_type),
                "{content_type} should be recognised"
            );
        }

        for content_type in ["application/json", "application/grpc-web", ""] {
            assert!(
                !is_grpc_content_type(content_type),
                "{content_type} is not grpc framing"
            );
        }
    }

    /// A unary call is one message; that is the fact a consumer most needs, and it is unreadable from
    /// an opaque body.
    #[test]
    fn a_unary_body_is_one_message() {
        let body = frame(false, b"hello protobuf");

        let parsed = parse_messages(&body).expect("a complete message parses");

        assert_eq!(
            parsed.messages,
            vec![GrpcMessage {
                index: 0,
                length: 14,
                compressed: false,
                text: Some("hello protobuf".to_string()),
                decoded: None,
            }]
        );
        assert_eq!(parsed.unparsed_bytes, 0);
        assert!(parsed.is_unary());
        assert_eq!(parsed.total_payload_bytes(), 14);
    }

    #[test]
    fn a_streaming_body_is_every_message_in_order() {
        let mut body = frame(false, b"one");
        body.extend_from_slice(&frame(true, b"two"));
        body.extend_from_slice(&frame(false, b""));

        let parsed = parse_messages(&body).expect("all messages parse");

        assert_eq!(
            parsed
                .messages
                .iter()
                .map(|m| (m.index, m.length, m.compressed))
                .collect::<Vec<_>>(),
            vec![(0, 3, false), (1, 3, true), (2, 0, false)]
        );
        assert!(!parsed.is_unary());
        assert_eq!(parsed.total_payload_bytes(), 6);
        // The compressed payload is not text until it is inflated, and the empty one is empty text.
        assert_eq!(
            parsed.messages[1].text, None,
            "a compressed payload is not text yet"
        );
        assert_eq!(parsed.messages[2].text, Some(String::new()));
    }

    /// An empty body is a valid gRPC body with no messages — not an error, and not a parse failure.
    #[test]
    fn an_empty_body_has_no_messages() {
        let parsed = parse_messages(&[]).expect("an empty body is valid");
        assert!(parsed.messages.is_empty());
        assert_eq!(parsed.unparsed_bytes, 0);
    }

    /// A capture cut short must say so. Reporting the messages that did arrive as if the call were
    /// complete is the failure mode this guards against.
    #[test]
    fn a_truncated_message_is_an_error_not_a_shorter_message() {
        let mut body = frame(false, b"complete");
        body.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x20, b'x', b'y']);

        let error = parse_messages(&body).expect_err("a short payload must not be accepted");

        assert_eq!(
            error,
            GrpcParseError::Incomplete {
                index: 1,
                declared: 32,
                available: 2,
            }
        );
        assert!(error.to_string().contains("declares 32 bytes"));
    }

    /// Bytes too short to be a frame are counted, so a consumer can tell "no messages" from "data we
    /// could not read".
    #[test]
    fn trailing_bytes_too_short_for_a_frame_are_counted() {
        let mut body = frame(false, b"ok");
        body.extend_from_slice(b"\x01\x02\x03");

        let parsed = parse_messages(&body).expect("the first message parses");

        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.unparsed_bytes, 3);
    }

    /// An empty message is a real message, and its payload is empty text — not a run of NUL bytes.
    ///
    /// `00 00 00 00 00` is both a valid frame and valid UTF-8, so a reader that simply decoded the
    /// body as text would show five control characters instead of "one empty message".
    #[test]
    fn an_empty_message_has_empty_text_not_control_characters() {
        let parsed =
            parse_messages(&[0x00, 0x00, 0x00, 0x00, 0x00]).expect("an empty message parses");

        assert!(parsed.is_unary());
        assert_eq!(parsed.messages[0].length, 0);
        assert_eq!(parsed.messages[0].text, Some(String::new()));
    }

    /// Text-bearing payloads must survive as text, including non-ASCII.
    #[test]
    fn a_text_payload_is_previewed_including_non_ascii() {
        let payload = r#"{"name":"继电器","ok":true}"#.as_bytes().to_vec();
        let parsed = parse_messages(&frame(false, &payload)).expect("text payload parses");

        assert_eq!(
            parsed.messages[0].text.as_deref(),
            Some(r#"{"name":"继电器","ok":true}"#),
            "the preview must not mangle non-ASCII text"
        );
    }

    /// Binary payloads must report no text rather than mojibake.
    #[test]
    fn a_binary_payload_has_no_text_preview() {
        let payload = [0xff, 0xfe, 0x00, 0x9c];
        let parsed = parse_messages(&frame(false, &payload)).expect("binary payload parses");

        assert_eq!(parsed.messages[0].length, 4);
        assert_eq!(parsed.messages[0].text, None);
    }

    /// The framing must not depend on the payload being text: protobuf is binary, and a parser that
    /// decoded as UTF-8 first would corrupt lengths.
    #[test]
    fn binary_payloads_are_framed_correctly() {
        let payload = [0x00, 0xff, 0x0a, 0x00, 0x7f];
        let body = frame(false, &payload);

        let parsed = parse_messages(&body).expect("binary payload parses");

        assert_eq!(parsed.messages[0].length, 5);
        assert_eq!(parsed.unparsed_bytes, 0);
        let _ = GrpcBody::total_payload_bytes(&parsed);
    }

    #[test]
    fn an_open_stream_keeps_complete_messages_and_counts_the_rest() {
        let mut body = frame(false, b"one");
        body.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x20, b'x']);

        let arrived = parse_arrived_messages(&body);

        assert_eq!(arrived.messages.len(), 1);
        assert_eq!(arrived.messages[0].text.as_deref(), Some("one"));
        assert_eq!(arrived.unparsed_bytes, 6);
        assert!(parse_messages(&body).is_err());
    }

    #[test]
    fn trailers_carry_the_call_outcome() {
        let trailers = vec![
            ("grpc-status".to_string(), "14".to_string()),
            ("grpc-message".to_string(), "unavailable%20now".to_string()),
        ];

        let (status, message) = outcome_from_trailers(&trailers);

        assert_eq!(status, Some(14));
        assert_eq!(message.as_deref(), Some("unavailable now"));
    }

    #[test]
    fn a_successful_call_is_status_zero() {
        let trailers = vec![("grpc-status".to_string(), "0".to_string())];

        let (status, message) = outcome_from_trailers(&trailers);

        assert_eq!(status, Some(0));
        assert_eq!(message, None);
    }
}
