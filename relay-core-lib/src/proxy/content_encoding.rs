//! `Content-Encoding` handling for bodies that are inspected or rewritten.
//!
//! Roadmap §24.3: the engine had **no** compression handling at all, so a body-stage rule evaluated
//! a filter against still-compressed bytes and, worse, a rewritten body was sent while the upstream's
//! `Content-Encoding` header still claimed it was compressed. Both are silent correctness failures.
//!
//! This module is deliberately narrow and honest:
//!
//! * `gzip` and `deflate` are decoded, so rules see the plaintext and a rewrite is re-encoded with
//!   the header intact.
//! * `br` and `zstd` are **not** decoded. When a rewrite happens on one of them the encoding is
//!   dropped and the plaintext is sent, which is correct HTTP — header and body agree — and is
//!   recorded so the degradation is visible rather than silent.
//! * An unknown encoding is never fabricated: encoding is only claimed when it was actually applied.

use flate2::Compression;
use flate2::read::{DeflateDecoder, GzDecoder};
use flate2::write::{DeflateEncoder, GzEncoder};
use std::io::{Read, Write};

/// What happened when a body's encoding was resolved for inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodedBody {
    /// The body was not encoded, or used an encoding we do not decode; bytes are as received.
    AsReceived,
    /// The body was decoded to plaintext; re-encoding is required before sending it back.
    Decoded,
}

/// Decode a body according to `content_encoding` (case-insensitive, comma-separated).
///
/// Returns the bytes to hand to inspectors plus whether they need re-encoding. Unknown or
/// unsupported encodings are returned unchanged with [`DecodedBody::AsReceived`], because guessing
/// would corrupt traffic.
pub fn decode_for_inspection(
    body: &[u8],
    content_encoding: Option<&str>,
) -> (Vec<u8>, DecodedBody) {
    let Some(encoding) = content_encoding else {
        return (body.to_vec(), DecodedBody::AsReceived);
    };

    // Only a single, simple encoding is decoded; stacked encodings ("gzip, br") are left alone, as
    // is an encoding we do not implement.
    let encoding = encoding.trim().to_ascii_lowercase();
    let decoded = match encoding.as_str() {
        "gzip" | "x-gzip" => read_all(GzDecoder::new(body)),
        "deflate" => read_all(DeflateDecoder::new(body)),
        _ => None,
    };

    match decoded {
        Some(plain) => (plain, DecodedBody::Decoded),
        // A malformed or unsupported body falls back to the bytes as received; guessing would
        // corrupt traffic.
        None => (body.to_vec(), DecodedBody::AsReceived),
    }
}

fn read_all<R: Read>(mut reader: R) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match reader.read_to_end(&mut out) {
        Ok(_) => Some(out),
        // A malformed or unexpectedly-truncated body must not take down the request.
        Err(_) => None,
    }
}

/// Re-encode a rewritten body with `content_encoding`.
///
/// Returns the bytes to send and the `Content-Encoding` that describes them. When the encoding
/// cannot be produced, the body is sent as plaintext with no encoding claim, which keeps header and
/// body consistent.
pub fn encode_after_rewrite(
    body: &[u8],
    content_encoding: Option<&str>,
) -> (Vec<u8>, Option<String>) {
    let Some(encoding) = content_encoding else {
        return (body.to_vec(), None);
    };

    let normalized = encoding.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "gzip" | "x-gzip" => {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            match encoder.write_all(body).and_then(|_| encoder.finish()) {
                Ok(encoded) => (encoded, Some("gzip".to_string())),
                Err(_) => (body.to_vec(), None),
            }
        }
        "deflate" => {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            match encoder.write_all(body).and_then(|_| encoder.finish()) {
                Ok(encoded) => (encoded, Some("deflate".to_string())),
                Err(_) => (body.to_vec(), None),
            }
        }
        // Not implemented: send plaintext and stop claiming an encoding we did not apply.
        _ => (body.to_vec(), None),
    }
}

/// Extract `Content-Encoding` from a header list, if present.
pub fn content_encoding_of(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
        .map(|(_, v)| v.clone())
}

/// Encoding names this module can decode and re-encode.
pub const SUPPORTED_ENCODINGS: [&str; 3] = ["gzip", "x-gzip", "deflate"];

/// Is this `Content-Encoding` one we can round-trip?
pub fn is_supported(content_encoding: &str) -> bool {
    let normalized = content_encoding.trim().to_ascii_lowercase();
    SUPPORTED_ENCODINGS.contains(&normalized.as_str())
}

#[cfg(test)]
mod tests {
    use super::{
        DecodedBody, content_encoding_of, decode_for_inspection, encode_after_rewrite, is_supported,
    };
    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder};
    use std::io::Write;

    fn gzip(body: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(body).expect("write");
        e.finish().expect("finish")
    }

    fn deflate(body: &[u8]) -> Vec<u8> {
        let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
        e.write_all(body).expect("write");
        e.finish().expect("finish")
    }

    #[test]
    fn gzip_body_is_decoded_for_inspection() {
        let plain = b"{\"hello\":\"world\"}";
        let (decoded, state) = decode_for_inspection(&gzip(plain), Some("gzip"));

        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(decoded, plain, "a gzip body must be plaintext to a rule");
    }

    #[test]
    fn deflate_body_is_decoded_for_inspection() {
        let plain = b"payload";
        let (decoded, state) = decode_for_inspection(&deflate(plain), Some("deflate"));

        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(decoded, plain);
    }

    #[test]
    fn encoding_name_is_case_insensitive() {
        let (decoded, state) = decode_for_inspection(&gzip(b"x"), Some("GZip"));
        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(decoded, b"x");
    }

    #[test]
    fn unencoded_body_is_left_alone() {
        let (decoded, state) = decode_for_inspection(b"plain", None);
        assert_eq!(state, DecodedBody::AsReceived);
        assert_eq!(decoded, b"plain");
    }

    #[test]
    fn unsupported_encoding_is_not_guessed_at() {
        // Brotli is not decoded yet. Passing the bytes through is the only safe behaviour: decoding
        // them as if they were gzip would corrupt traffic.
        let (decoded, state) = decode_for_inspection(b"\x21\x00brotli-ish", Some("br"));
        assert_eq!(state, DecodedBody::AsReceived);
        assert_eq!(decoded, b"\x21\x00brotli-ish");
    }

    #[test]
    fn malformed_encoding_falls_back_to_the_original_bytes() {
        let (decoded, state) = decode_for_inspection(b"not really gzip", Some("gzip"));
        assert_eq!(state, DecodedBody::AsReceived);
        assert_eq!(decoded, b"not really gzip");
    }

    #[test]
    fn rewrite_is_re_encoded_with_a_matching_header() {
        // The point of the module: a rewritten body must be sent with a header that describes it.
        let (encoded, encoding) = encode_after_rewrite(b"REWRITTEN", Some("gzip"));

        assert_eq!(encoding.as_deref(), Some("gzip"));
        let (round_tripped, state) = decode_for_inspection(&encoded, Some("gzip"));
        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(
            round_tripped, b"REWRITTEN",
            "a client must be able to decode what we sent"
        );
    }

    #[test]
    fn rewriting_an_unsupported_encoding_sends_plaintext_without_claiming_otherwise() {
        // Previously the upstream's `Content-Encoding: br` survived a rewrite, so the client was told
        // the plaintext was brotli. Dropping the header is the honest outcome.
        let (encoded, encoding) = encode_after_rewrite(b"REWRITTEN", Some("br"));

        assert_eq!(
            encoding, None,
            "must not claim an encoding that was not applied"
        );
        assert_eq!(encoded, b"REWRITTEN");
    }

    #[test]
    fn rewriting_without_an_encoding_adds_none() {
        let (encoded, encoding) = encode_after_rewrite(b"REWRITTEN", None);
        assert_eq!(encoding, None);
        assert_eq!(encoded, b"REWRITTEN");
    }

    #[test]
    fn supported_set_matches_what_can_be_round_tripped() {
        for e in ["gzip", "x-gzip", "deflate", "GZIP"] {
            assert!(is_supported(e), "{e} should be supported");
        }
        for e in ["br", "zstd", "identity", "gzip, br"] {
            assert!(!is_supported(e), "{e} should not claim round-trip support");
        }
    }

    #[test]
    fn content_encoding_lookup_ignores_case() {
        let headers = vec![("Content-Encoding".to_string(), "gzip".to_string())];
        assert_eq!(content_encoding_of(&headers).as_deref(), Some("gzip"));
        assert_eq!(content_encoding_of(&[]), None);
    }
}
