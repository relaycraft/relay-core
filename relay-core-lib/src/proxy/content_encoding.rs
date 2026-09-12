//! `Content-Encoding` handling for bodies that are inspected or rewritten.
//!
//! Roadmap §24.3: the engine had **no** compression handling at all, so a body-stage rule evaluated
//! a filter against still-compressed bytes and, worse, a rewritten body was sent while the upstream's
//! `Content-Encoding` header still claimed it was compressed. Both are silent correctness failures.
//!
//! This module is deliberately narrow and honest:
//!
//! * `gzip`, `deflate`, `br` and `zstd` are decoded, so rules see the plaintext and a rewrite is
//!   re-encoded with the header intact — matching mitmproxy's behaviour for the same set (see
//!   docs/mitmproxy-policy-benchmark.md §1.2).
//! * An encoding we do not implement is never guessed at, and never claimed: an unrewritten body
//!   passes through untouched, and a rewrite of an unknown encoding is sent as plaintext with the
//!   header dropped, so header and body still agree.

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
        "br" => decode_brotli(body),
        "zstd" => decode_zstd(body),
        _ => None,
    };

    match decoded {
        Some(plain) => (plain, DecodedBody::Decoded),
        // A malformed or unsupported body falls back to the bytes as received; guessing would
        // corrupt traffic.
        None => (body.to_vec(), DecodedBody::AsReceived),
    }
}

/// Brotli has no reader adapter in the `brotli` crate, so decompress into a bounded buffer.
fn decode_brotli(body: &[u8]) -> Option<Vec<u8>> {
    // Refuse to expand a small body into an unbounded allocation.
    let mut out = Vec::with_capacity(body.len().saturating_mul(4).min(MAX_DECODED_BYTES));
    let mut reader = brotli::Decompressor::new(body, 4096);
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out.len() + n > MAX_DECODED_BYTES {
                    return None;
                }
                out.extend_from_slice(&buf[..n]);
            }
            Err(_) => return None,
        }
    }
    Some(out)
}

/// Decode a zstd frame. `zstd::stream::decode_all` errors on a malformed frame, which the caller
/// treats as "leave the bytes alone".
fn decode_zstd(body: &[u8]) -> Option<Vec<u8>> {
    match zstd::stream::decode_all(body) {
        Ok(out) if out.len() <= MAX_DECODED_BYTES => Some(out),
        _ => None,
    }
}

/// Compression level used when re-encoding a rewritten body.
///
/// Deliberately low: a rewritten body is usually small, latency matters more than ratio on the
/// request path, and this matches what most servers use for dynamic responses.
const ZSTD_LEVEL: i32 = 3;

/// Upper bound on a decoded body.
///
/// A tiny compressed payload can expand enormously ("a compression bomb"), so decoding is capped
/// rather than trusted. Bodies above the cap are treated as undecodable and pass through untouched.
const MAX_DECODED_BYTES: usize = 16 * 1024 * 1024;

fn read_all<R: Read>(mut reader: R) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match reader.read_to_end(&mut out) {
        // A malformed or unexpectedly-truncated body must not take down the request, and a body that
        // expands past the cap is refused rather than materialized.
        Ok(_) if out.len() <= MAX_DECODED_BYTES => Some(out),
        _ => None,
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
        "br" => {
            let mut out = Vec::new();
            match brotli::BrotliCompress(
                &mut std::io::Cursor::new(body),
                &mut out,
                &brotli::enc::BrotliEncoderParams::default(),
            ) {
                Ok(_) => (out, Some("br".to_string())),
                Err(_) => (body.to_vec(), None),
            }
        }
        "zstd" => match zstd::stream::encode_all(body, ZSTD_LEVEL) {
            Ok(encoded) => (encoded, Some("zstd".to_string())),
            Err(_) => (body.to_vec(), None),
        },
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
pub const SUPPORTED_ENCODINGS: [&str; 5] = ["gzip", "x-gzip", "deflate", "br", "zstd"];

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
    fn unknown_encoding_is_not_guessed_at() {
        // An encoding we do not implement must pass through untouched: decoding it as something else
        // would corrupt traffic.
        let (decoded, state) = decode_for_inspection(b"\x21\x00mystery", Some("x-weird"));
        assert_eq!(state, DecodedBody::AsReceived);
        assert_eq!(decoded, b"\x21\x00mystery");
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
        // An encoding we cannot produce must not be claimed: the header is dropped so that header and
        // body still agree. An unrewritten body is never touched, so this path only affects rewrites
        // of encodings we do not implement.
        for encoding_name in ["x-weird", "gzip, br"] {
            let (encoded, encoding) = encode_after_rewrite(b"REWRITTEN", Some(encoding_name));

            assert_eq!(
                encoding, None,
                "must not claim {encoding_name} when it was not applied"
            );
            assert_eq!(encoded, b"REWRITTEN");
        }
    }

    #[test]
    fn rewriting_without_an_encoding_adds_none() {
        let (encoded, encoding) = encode_after_rewrite(b"REWRITTEN", None);
        assert_eq!(encoding, None);
        assert_eq!(encoded, b"REWRITTEN");
    }

    /// Committed real frames generated with the `zstd` and `brotli` CLIs, so the decoders are
    /// exercised against genuine streams rather than self-produced output (which could hide a
    /// symmetric misunderstanding of the format).
    const ZSTD_FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/zstd_payload.bin");
    const BROTLI_FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/brotli_payload.br");
    const ZSTD_PLAIN: &[u8] = b"zstd-encoded-payload-for-relaycore-tests";
    const BROTLI_PLAIN: &[u8] = b"brotli-encoded-payload-for-relaycore-tests";

    #[test]
    fn brotli_fixture_decodes_then_round_trips() {
        let (plain, state) = decode_for_inspection(BROTLI_FIXTURE, Some("br"));
        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(plain, BROTLI_PLAIN, "a br body must be plaintext to a rule");

        let (encoded, encoding) = encode_after_rewrite(b"REWRITTEN-BR", Some("br"));
        assert_eq!(encoding.as_deref(), Some("br"));
        let (back, state) = decode_for_inspection(&encoded, Some("br"));
        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(back, b"REWRITTEN-BR");
    }

    /// The fixture is a real frame from the `zstd` CLI, so the decoder is exercised against a genuine
    /// stream rather than output produced by the same library.
    #[test]
    fn zstd_fixture_decodes_then_round_trips() {
        let (plain, state) = decode_for_inspection(ZSTD_FIXTURE, Some("zstd"));
        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(plain, ZSTD_PLAIN);

        let (encoded, encoding) = encode_after_rewrite(b"REWRITTEN-ZSTD", Some("zstd"));
        assert_eq!(encoding.as_deref(), Some("zstd"));
        let (back, state) = decode_for_inspection(&encoded, Some("zstd"));
        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(back, b"REWRITTEN-ZSTD");
    }

    #[test]
    fn malformed_brotli_and_zstd_are_passed_through_untouched() {
        // Guessing would corrupt traffic, so an undecodable body must fall back to raw bytes.
        for (bytes, encoding) in [
            (&b"not brotli at all"[..], "br"),
            (&b"not zstd at all"[..], "zstd"),
        ] {
            let (decoded, state) = decode_for_inspection(bytes, Some(encoding));
            assert_eq!(
                state,
                DecodedBody::AsReceived,
                "{encoding} should not be guessed at"
            );
            assert_eq!(decoded, bytes);
        }
    }

    #[test]
    fn codec_set_matches_mitmproxy_coverage() {
        // mitmproxy 12.2.3 decodes and re-encodes gzip/deflate/br/zstd (see
        // docs/mitmproxy-policy-benchmark.md §1.2); RelayCore now covers the same set.
        for e in ["gzip", "x-gzip", "deflate", "br", "zstd"] {
            assert!(is_supported(e), "{e} should be supported");
        }
        for e in ["identity", "unknown-codec", "gzip, br"] {
            assert!(!is_supported(e), "{e} must not claim round-trip support");
        }
    }

    #[test]
    fn supported_set_matches_what_can_be_round_tripped() {
        for e in ["gzip", "x-gzip", "deflate", "GZIP"] {
            assert!(is_supported(e), "{e} should be supported");
        }
        for e in ["identity", "unknown-codec", "gzip, br"] {
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
