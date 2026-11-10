//! `Content-Encoding` handling for bodies that are inspected or rewritten.
//!
//! Roadmap §24.3: the engine had **no** compression handling at all, so a body-stage rule evaluated
//! a filter against still-compressed bytes and, worse, a rewritten body was sent while the upstream's
//! `Content-Encoding` header still claimed it was compressed. Both are silent correctness failures.
//!
//! The behaviour is deliberately narrow and honest:
//!
//! * `gzip`, `deflate`, `br` and `zstd` are decoded, so rules see the plaintext and a rewrite is
//!   re-encoded with the header intact — matching mitmproxy's behaviour for the same set (see
//!   docs/mitmproxy-policy-benchmark.md §1.2).
//! * **Stacked encodings are handled in the right order.** `Content-Encoding: gzip, br` means the
//!   body was gzipped and then brotli-compressed, so decoding walks the list backwards and encoding
//!   walks it forwards. Treating a stack as "unsupported" silently skipped rules on real traffic.
//! * An encoding we do not implement is never guessed at, and never claimed: an unrewritten body
//!   passes through untouched, and a rewrite of an unknown encoding is sent as plaintext with the
//!   header dropped, so header and body still agree.
//! * The codec set is a registry rather than a `match`. A host (or a test) can add a codec without
//!   touching the decoder, and [`is_supported`] answers from the same registry the decode path uses,
//!   so "supported" and "actually works" cannot drift apart.

use flate2::Compression;
use flate2::read::{DeflateDecoder, GzDecoder};
use flate2::write::{DeflateEncoder, GzEncoder};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, OnceLock, RwLock};

/// One `Content-Encoding` implementation.
///
/// `decode` returns `None` when the bytes are not a valid stream for this codec; the caller then
/// passes the body through untouched rather than guessing.
pub trait ContentCodec: Send + Sync {
    /// The name written in `Content-Encoding`. Lowercase, no parameters.
    fn name(&self) -> &'static str;
    fn decode(&self, body: &[u8]) -> Option<Vec<u8>>;
    fn encode(&self, body: &[u8]) -> Option<Vec<u8>>;
}

/// Codecs by the name they answer to, including aliases such as `x-gzip`.
#[derive(Default)]
pub struct CodecRegistry {
    codecs: HashMap<String, Arc<dyn ContentCodec>>,
}

impl CodecRegistry {
    /// The registry every call uses unless a host installs its own.
    pub fn builtin() -> Self {
        let mut registry = Self::default();
        registry.register(Arc::new(GzipCodec));
        registry.register(Arc::new(DeflateCodec));
        registry.register(Arc::new(BrotliCodec));
        registry.register(Arc::new(ZstdCodec));
        // `x-gzip` is what an old client sends for gzip; refusing it would leave real traffic
        // un-decoded for a naming technicality.
        registry
            .alias("x-gzip", "gzip")
            .alias("x-compress", "compress")
            .alias("identity", "identity");
        registry
    }

    pub fn register(&mut self, codec: Arc<dyn ContentCodec>) -> &mut Self {
        self.codecs.insert(codec.name().to_string(), codec);
        self
    }

    /// Point another name at an existing codec.
    pub fn alias(&mut self, alias: &str, name: &str) -> &mut Self {
        if let Some(codec) = self.codecs.get(name).cloned() {
            self.codecs.insert(alias.to_string(), codec);
        }
        self
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ContentCodec>> {
        self.codecs.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.codecs.keys().cloned().collect();
        names.sort();
        names
    }
}

/// The registry used by [`decode_for_inspection`] and friends.
///
/// A `RwLock` rather than a `OnceLock` so a host can add a codec after startup; reads are the hot
/// path and only take the shared lock for the lookup.
fn registry() -> &'static RwLock<CodecRegistry> {
    static REGISTRY: OnceLock<RwLock<CodecRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(CodecRegistry::builtin()))
}

/// Add a codec at runtime. Re-registering a name replaces it.
pub fn register_codec(codec: Arc<dyn ContentCodec>) {
    if let Ok(mut registry) = registry().write() {
        registry.register(codec);
    }
}

/// The names a chain would need, in the order they are applied to the body.
pub fn parse_encodings(content_encoding: Option<&str>) -> Vec<String> {
    let Some(value) = content_encoding else {
        return Vec::new();
    };
    value
        .split(',')
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty() && part != "identity")
        .collect()
}

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
    let encodings = parse_encodings(content_encoding);
    if encodings.is_empty() {
        return (body.to_vec(), DecodedBody::AsReceived);
    }

    let registry = match registry().read() {
        Ok(registry) => registry,
        Err(poisoned) => poisoned.into_inner(),
    };

    // The header lists the encodings in the order they were applied, so undoing them means walking
    // the list backwards.
    let mut bytes = body.to_vec();
    for name in encodings.iter().rev() {
        let Some(codec) = registry.get(name) else {
            return (body.to_vec(), DecodedBody::AsReceived);
        };
        match codec.decode(&bytes) {
            Some(plain) => bytes = plain,
            None => return (body.to_vec(), DecodedBody::AsReceived),
        }
    }
    (bytes, DecodedBody::Decoded)
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

struct GzipCodec;
struct DeflateCodec;
struct BrotliCodec;
struct ZstdCodec;

impl ContentCodec for GzipCodec {
    fn name(&self) -> &'static str {
        "gzip"
    }

    fn decode(&self, body: &[u8]) -> Option<Vec<u8>> {
        read_all(GzDecoder::new(body))
    }

    fn encode(&self, body: &[u8]) -> Option<Vec<u8>> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body).ok()?;
        encoder.finish().ok()
    }
}

impl ContentCodec for DeflateCodec {
    fn name(&self) -> &'static str {
        "deflate"
    }

    fn decode(&self, body: &[u8]) -> Option<Vec<u8>> {
        read_all(DeflateDecoder::new(body))
    }

    fn encode(&self, body: &[u8]) -> Option<Vec<u8>> {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body).ok()?;
        encoder.finish().ok()
    }
}

impl ContentCodec for BrotliCodec {
    fn name(&self) -> &'static str {
        "br"
    }

    fn decode(&self, body: &[u8]) -> Option<Vec<u8>> {
        decode_brotli(body)
    }

    fn encode(&self, body: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        brotli::BrotliCompress(
            &mut std::io::Cursor::new(body),
            &mut out,
            &brotli::enc::BrotliEncoderParams::default(),
        )
        .ok()?;
        Some(out)
    }
}

impl ContentCodec for ZstdCodec {
    fn name(&self) -> &'static str {
        "zstd"
    }

    fn decode(&self, body: &[u8]) -> Option<Vec<u8>> {
        decode_zstd(body)
    }

    fn encode(&self, body: &[u8]) -> Option<Vec<u8>> {
        zstd::stream::encode_all(body, ZSTD_LEVEL).ok()
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
    let encodings = parse_encodings(content_encoding);
    if encodings.is_empty() {
        return (body.to_vec(), None);
    }

    let registry = match registry().read() {
        Ok(registry) => registry,
        Err(poisoned) => poisoned.into_inner(),
    };

    // Applied in the order the header lists them, which is the reverse of decoding.
    let mut bytes = body.to_vec();
    for name in &encodings {
        let Some(codec) = registry.get(name) else {
            return (body.to_vec(), None);
        };
        match codec.encode(&bytes) {
            Some(encoded) => bytes = encoded,
            None => return (body.to_vec(), None),
        }
    }
    (bytes, Some(encodings.join(", ")))
}

/// Extract `Content-Encoding` from a header list, if present.
pub fn content_encoding_of(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
        .map(|(_, v)| v.clone())
}

/// Encoding names the built-in registry can decode and re-encode.
pub const SUPPORTED_ENCODINGS: [&str; 5] = ["gzip", "x-gzip", "deflate", "br", "zstd"];

/// Is this `Content-Encoding` chain round-trippable with the current registry?
///
/// Answers from the same registry the decode path uses, so "supported" cannot drift from "works".
/// An empty or `identity` chain needs no codec and is supported trivially.
pub fn is_supported(content_encoding: &str) -> bool {
    let encodings = parse_encodings(Some(content_encoding));
    if encodings.is_empty() {
        return true;
    }
    let registry = match registry().read() {
        Ok(registry) => registry,
        Err(poisoned) => poisoned.into_inner(),
    };
    encodings.iter().all(|name| registry.get(name).is_some())
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
        for encoding_name in ["x-weird", "gzip, x-weird"] {
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
        for e in ["unknown-codec", "gzip, unknown-codec"] {
            assert!(!is_supported(e), "{e} must not claim round-trip support");
        }
    }

    #[test]
    fn supported_set_matches_what_can_be_round_tripped() {
        for e in ["gzip", "x-gzip", "deflate", "GZIP"] {
            assert!(is_supported(e), "{e} should be supported");
        }
        // `identity` needs no codec: a body that is not encoded is trivially round-trippable, and
        // refusing it would make a harmless header look like an unsupported encoding.
        for e in ["identity", ""] {
            assert!(is_supported(e), "{e:?} needs no codec");
        }
        for e in ["unknown-codec", "identity, unknown-codec"] {
            assert!(!is_supported(e), "{e} should not claim round-trip support");
        }
    }

    /// The header lists encodings in the order they were applied, so decoding must undo them
    /// backwards. A stack treated as "unsupported" silently skipped body rules on real traffic.
    #[test]
    fn stacked_encodings_decode_backwards_and_encode_forwards() {
        let plain = b"stacked payload";
        let gzipped = gzip(plain);
        let (double, encoding) = encode_after_rewrite(&gzipped, Some("br"));
        assert_eq!(
            encoding.as_deref(),
            Some("br"),
            "brotli wraps the gzip bytes"
        );

        // The wire form of gzip-then-brotli.
        let wire = double;
        let (decoded, state) = decode_for_inspection(&wire, Some("gzip, br"));
        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(decoded, plain, "the stack must be undone in reverse order");

        let (re_encoded, claimed) = encode_after_rewrite(plain, Some("gzip, br"));
        assert_eq!(claimed.as_deref(), Some("gzip, br"));
        let (again, state) = decode_for_inspection(&re_encoded, Some("gzip, br"));
        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(again, plain);
    }

    /// A stack with one unknown member must pass through untouched: decoding the known half and
    /// sending the unknown half through would produce bytes nobody can read.
    #[test]
    fn a_stack_with_an_unknown_member_is_left_alone() {
        let wire = gzip(b"payload");
        let (decoded, state) = decode_for_inspection(&wire, Some("gzip, mystery"));
        assert_eq!(state, DecodedBody::AsReceived);
        assert_eq!(decoded, wire);
    }

    /// A host can add a codec without touching the decoder, and `is_supported` follows the registry
    /// rather than a hard-coded list.
    #[test]
    fn a_registered_codec_becomes_supported_and_usable() {
        struct Reverse;
        impl super::ContentCodec for Reverse {
            fn name(&self) -> &'static str {
                "reverse"
            }
            fn decode(&self, body: &[u8]) -> Option<Vec<u8>> {
                Some(body.iter().rev().copied().collect())
            }
            fn encode(&self, body: &[u8]) -> Option<Vec<u8>> {
                Some(body.iter().rev().copied().collect())
            }
        }

        assert!(!is_supported("reverse"), "not registered yet");
        super::register_codec(std::sync::Arc::new(Reverse));
        assert!(is_supported("reverse"), "registration makes it supported");

        let (decoded, state) = decode_for_inspection(b"abc", Some("reverse"));
        assert_eq!(state, DecodedBody::Decoded);
        assert_eq!(decoded, b"cba");

        let (encoded, claimed) = encode_after_rewrite(b"abc", Some("reverse"));
        assert_eq!(claimed.as_deref(), Some("reverse"));
        assert_eq!(encoded, b"cba");
    }

    #[test]
    fn content_encoding_lookup_ignores_case() {
        let headers = vec![("Content-Encoding".to_string(), "gzip".to_string())];
        assert_eq!(content_encoding_of(&headers).as_deref(), Some("gzip"));
        assert_eq!(content_encoding_of(&[]), None);
    }
}

/// Adversarial and malformed inputs (roadmap §24.10 "parser/codec fuzz targets").
///
/// A general-purpose fuzzer needs a nightly toolchain and a separate build; these cases are
/// deterministic, run in the normal suite, and target the failures that actually matter for a
/// decoder sitting in the traffic path: truncated frames, wrong magic bytes, stacked encodings,
/// expansion bombs, and inputs that must never panic.
#[cfg(test)]
mod adversarial_tests {
    use super::{DecodedBody, decode_for_inspection, encode_after_rewrite};

    /// Truncating a valid frame at every length must never panic and must never yield a bogus
    /// decode: either it decodes or the caller gets the bytes back untouched.
    #[test]
    fn truncated_frames_are_handled_at_every_length() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, b"payload-that-compresses").expect("write");
        let full = encoder.finish().expect("finish");

        for cut in 0..full.len() {
            let (decoded, state) = decode_for_inspection(&full[..cut], Some("gzip"));
            match state {
                // A complete-but-smaller decode is impossible here, so decoding must not claim success
                // unless it really produced the payload.
                DecodedBody::Decoded => assert_eq!(
                    decoded, b"payload-that-compresses",
                    "a truncated frame must not decode to something else"
                ),
                DecodedBody::AsReceived => assert_eq!(
                    decoded,
                    &full[..cut],
                    "an undecodable prefix must be returned unchanged"
                ),
            }
        }
    }

    /// A body that claims an encoding it does not have must be passed through, never guessed at.
    #[test]
    fn wrong_magic_bytes_are_not_guessed_at() {
        let impostors: [&[u8]; 5] = [
            b"\x1f\x8b\x08\x00",     // gzip magic, but truncated garbage
            b"\x28\xb5\x2f\xfd\x00", // zstd magic, but not a frame
            b"not compressed at all",
            b"\x00\x01\x02\x03\x04\x05",
            b"",
        ];

        for encoding in ["gzip", "deflate", "br", "zstd"] {
            for body in impostors {
                let (decoded, state) = decode_for_inspection(body, Some(encoding));
                assert_eq!(
                    decoded, body,
                    "{encoding} must return undecodable bytes unchanged"
                );
                if body.is_empty() {
                    continue;
                }
                assert_eq!(
                    state,
                    DecodedBody::AsReceived,
                    "{encoding} must not claim to have decoded {body:?}"
                );
            }
        }
    }

    /// Stacked encodings are not decoded: applying one decoder to a two-layer body would produce
    /// garbage, so the input must survive untouched.
    #[test]
    fn stacked_encodings_are_left_alone() {
        let body = b"anything";
        let (decoded, state) = decode_for_inspection(body, Some("gzip, br"));
        assert_eq!(state, DecodedBody::AsReceived);
        assert_eq!(decoded, body);
    }

    /// Encoding names arrive from the wire, so they may be arbitrarily malformed.
    #[test]
    fn malformed_encoding_names_do_not_panic() {
        for encoding in [
            "",
            " ",
            ",",
            "gzip,",
            ",gzip",
            "GZIP",
            "gzip;q=1",
            "unknown-encoding-with-unicode-编码",
            "\u{0}",
        ] {
            let (decoded, _) = decode_for_inspection(b"body", Some(encoding));
            assert_eq!(
                decoded, b"body",
                "encoding {encoding:?} must not corrupt the body"
            );
        }
    }

    /// A tiny compressed body can expand enormously. The decoder must refuse rather than allocate
    /// without bound, and must not panic.
    #[test]
    fn an_expansion_bomb_is_refused_rather_than_materialized() {
        // ~64 MiB of zeros compresses to a few KiB under gzip.
        let bomb_plain = vec![0u8; 64 * 1024 * 1024];
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut encoder, &bomb_plain).expect("write");
        let bomb = encoder.finish().expect("finish");

        assert!(
            bomb.len() < 1024 * 1024,
            "precondition: the bomb must be small relative to what it expands to"
        );

        let (decoded, state) = decode_for_inspection(&bomb, Some("gzip"));
        assert_eq!(
            state,
            DecodedBody::AsReceived,
            "an expansion past the cap must be refused, not decoded"
        );
        assert_eq!(
            decoded, bomb,
            "a refused bomb must be passed through unchanged"
        );
    }

    /// Re-encoding must never panic on arbitrary input, and must only claim an encoding it applied.
    #[test]
    fn re_encoding_arbitrary_bytes_never_panics() {
        let inputs: [&[u8]; 4] = [b"", b"x", &[0u8; 1024], &[0xffu8; 300]];

        for encoding in ["gzip", "deflate", "br", "zstd", "x-unknown"] {
            for input in inputs {
                let (encoded, claimed) = encode_after_rewrite(input, Some(encoding));
                if let Some(claimed) = claimed {
                    // If an encoding is claimed, the bytes must genuinely be in that encoding.
                    let (round_tripped, state) = decode_for_inspection(&encoded, Some(&claimed));
                    assert_eq!(
                        state,
                        DecodedBody::Decoded,
                        "claimed {claimed} but produced undecodable bytes"
                    );
                    assert_eq!(
                        round_tripped, input,
                        "a claimed encoding must round-trip the input"
                    );
                } else {
                    assert_eq!(
                        encoded, input,
                        "unclaimed output must be the input verbatim"
                    );
                }
            }
        }
    }
}
