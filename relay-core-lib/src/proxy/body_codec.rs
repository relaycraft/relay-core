use data_encoding::BASE64;

/// Helper to process body bytes into BodyData content and encoding
pub fn process_body(bytes: &[u8], headers: &[(String, String)]) -> (String, String) {
    // 1. Check content-type header
    let content_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.to_lowercase())
        .unwrap_or_default();

    // Heuristic 1: If explicit charset is present, treat as text
    if content_type.contains("charset=utf-8")
        || content_type.contains("charset=us-ascii")
        || content_type.contains("text/")
        || content_type.contains("application/json")
        || content_type.contains("application/xml")
        || content_type.contains("application/javascript")
    {
        // Try UTF-8 decode first to be safe, if fails, fallback to base64
        match std::str::from_utf8(bytes) {
            Ok(s) => return ("utf-8".to_string(), s.to_string()),
            Err(_) => return ("base64".to_string(), BASE64.encode(bytes)),
        }
    }

    // Heuristic 2: Known binary types
    if content_type.starts_with("image/")
        || content_type.starts_with("audio/")
        || content_type.starts_with("video/")
        || content_type.contains("application/octet-stream")
        || content_type.contains("application/pdf")
        || content_type.contains("application/zip")
    {
        return ("base64".to_string(), BASE64.encode(bytes));
    }

    // Heuristic 3: Try UTF-8 decode as fallback
    match std::str::from_utf8(bytes) {
        Ok(s) => ("utf-8".to_string(), s.to_string()),
        Err(_) => ("base64".to_string(), BASE64.encode(bytes)),
    }
}

/// Choose the representation for a body **and** describe its gRPC framing when it has any.
///
/// The framing lives next to the representation because it is decided from the same two inputs — the
/// bytes and the content type — and the two must agree: a body reported as `base64` with a gRPC
/// summary is a captured call a consumer can read, whereas either alone is not.
pub fn process_body_with_framing(
    bytes: &[u8],
    headers: &[(String, String)],
) -> (String, String, Option<relay_core_api::grpc::GrpcBody>) {
    let (encoding, content) = process_body(bytes, headers);

    let content_type = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or_default();

    let grpc = if relay_core_api::grpc::is_grpc_content_type(content_type) {
        // A body that does not parse is reported as "no framing" rather than as a partial one: the
        // messages that did fit are not a smaller call, they are a truncated capture, and the
        // trailing-byte count would be mistaken for the whole picture.
        relay_core_api::grpc::parse_messages(bytes).ok()
    } else {
        None
    };

    // A gRPC body is a binary framing (a five-byte prefix in front of each message), so it is never
    // text even when its payloads are: `00 00 00 00 00` is an empty message *and* valid UTF-8, and a
    // reader that decoded the body as text would render five control characters instead. The readable
    // form is the per-message preview, so the raw bytes are reported as bytes.
    if grpc.is_some() {
        return ("base64".to_string(), BASE64.encode(bytes), grpc);
    }

    (encoding, content, grpc)
}

#[cfg(test)]
mod framing_tests {
    use super::process_body_with_framing;

    fn headers(content_type: &str) -> Vec<(String, String)> {
        vec![("content-type".to_string(), content_type.to_string())]
    }

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn a_grpc_body_gains_framing_and_a_normal_body_does_not() {
        let body = frame(b"");

        let (_, _, grpc) = process_body_with_framing(&body, &headers("application/grpc+proto"));
        let parsed = grpc.expect("a grpc body must carry framing");
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].length, 2);
        assert!(parsed.is_unary());

        let (_, _, none) = process_body_with_framing(&body, &headers("application/octet-stream"));
        assert!(none.is_none(), "a non-grpc body has no framing to report");
    }

    /// A gRPC body must be reported as bytes, with its content readable through the message
    /// previews. The framing prefix is binary, so decoding the whole body as text mangles it — and an
    /// empty message is the clearest case, being five NUL bytes that happen to be valid UTF-8.
    #[test]
    fn a_grpc_body_is_bytes_even_when_its_payload_is_text() {
        let body = frame(b"\x08\x01");

        let (encoding, content, grpc) =
            process_body_with_framing(&body, &headers("application/grpc+proto"));

        assert_eq!(
            encoding, "base64",
            "the framing is binary and must not be decoded as text"
        );
        assert_eq!(content, data_encoding::BASE64.encode(&body));
        assert!(grpc.is_some());
    }

    #[test]
    fn an_empty_grpc_message_is_bytes_with_empty_text() {
        let body = [0x00u8, 0x00, 0x00, 0x00, 0x00];

        let (encoding, _, grpc) = process_body_with_framing(&body, &headers("application/grpc"));

        assert_eq!(encoding, "base64");
        let parsed = grpc.expect("an empty message is still a message");
        assert!(parsed.is_unary());
        assert_eq!(parsed.messages[0].text, Some(String::new()));
    }

    /// A truncated capture must not be presented as a smaller call.
    #[test]
    fn an_unparsable_grpc_body_reports_no_framing_rather_than_partial() {
        let mut body = frame(b"complete");
        body.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x20, b'x']);

        let (_, _, grpc) = process_body_with_framing(&body, &headers("application/grpc"));

        assert!(
            grpc.is_none(),
            "a short frame is a truncated capture, not a one-message call"
        );
    }
}
