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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrpcMessage {
    /// Position in the body, from zero.
    pub index: u32,
    /// Payload length in bytes, as declared by the frame.
    pub length: u32,
    /// Whether the frame says the payload is compressed (the first byte, non-zero).
    pub compressed: bool,
}

/// What a body turned out to contain, structurally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrpcBody {
    /// Messages in arrival order.
    pub messages: Vec<GrpcMessage>,
    /// Bytes that were not part of a complete message — a capture cut short, or trailing junk.
    ///
    /// Reported rather than ignored: a partial capture that looks complete is worse than one that
    /// admits what it lost.
    pub unparsed_bytes: usize,
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
            return Err(GrpcParseError::Incomplete {
                index,
                declared: length,
                available: body.len() - payload_start,
            });
        }

        messages.push(GrpcMessage {
            index,
            length,
            compressed,
        });
        offset = payload_end;
        index += 1;
    }

    Ok(GrpcBody {
        messages,
        unparsed_bytes: body.len() - offset,
    })
}

#[cfg(test)]
mod tests {
    use super::{GrpcBody, GrpcMessage, GrpcParseError, is_grpc_content_type, parse_messages};

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
}
