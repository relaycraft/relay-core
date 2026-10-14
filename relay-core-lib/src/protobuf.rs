//! Read-time protobuf views for captured gRPC messages.
//!
//! Capture stores length-prefixed frames and leaves the payload bytes alone. A detail read calls
//! [`apply_to_flow`] with a [`ProtoPool`] built from a directory of `.proto` files. The directory
//! is one include root: imports resolve inside it, and `google/protobuf` well-known types come
//! with the compiler. The call path `/package.Service/Method` selects the request and response
//! message. When nothing matches, the payload is still walked as wire fields.
//!
//! Nothing here runs on the forward path, and nothing it writes is stored with the capture.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use data_encoding::BASE64;
use prost_reflect::{DescriptorPool, DynamicMessage};
use relay_core_api::flow::{BodyData, Flow, Layer};
use relay_core_api::grpc::{GrpcDecodeSource, GrpcDecoded};
use serde_json::{Map, Value};

const MAX_DECODE_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: u32 = 32;
const MAX_FIELDS: u32 = 4096;
const MAX_NOTE_CHARS: usize = 2000;

/// Request or response message selected by one `rpc`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MethodMessages {
    request: String,
    response: String,
}

/// Compiled `.proto` files, keyed by gRPC method path.
#[derive(Debug, Clone)]
pub struct ProtoPool {
    descriptors: DescriptorPool,
    methods: HashMap<String, MethodMessages>,
    diagnostic: Option<String>,
}

impl Default for ProtoPool {
    fn default() -> Self {
        Self::empty()
    }
}

impl ProtoPool {
    /// A pool that only knows the wire format.
    pub fn empty() -> Self {
        Self {
            descriptors: DescriptorPool::new(),
            methods: HashMap::new(),
            diagnostic: None,
        }
    }

    /// Compile every `.proto` under `dir`, which is the sole include root.
    ///
    /// An empty or missing directory is an empty pool, not an error: the caller has not provided
    /// a schema. A file that does not compile is an error, and the caller keeps the previous
    /// successful pool when it has one.
    pub fn compile_dir(dir: &Path) -> Result<Self, String> {
        let files = proto_files(dir);
        if files.is_empty() {
            return Ok(Self::empty());
        }
        let mut compiler = protox::Compiler::new([dir]).map_err(|err| note(&err.to_string()))?;
        compiler.include_imports(true);
        compiler
            .open_files(files)
            .map_err(|err| note(&err.to_string()))?;
        let encoded = compiler.encode_file_descriptor_set();
        let descriptors =
            DescriptorPool::decode(encoded.as_slice()).map_err(|err| note(&err.to_string()))?;
        let methods = index_methods(&descriptors);
        Ok(Self {
            descriptors,
            methods,
            diagnostic: None,
        })
    }

    /// Compiler diagnostic from the last failed reload of a directory that previously succeeded.
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }

    pub fn has_methods(&self) -> bool {
        !self.methods.is_empty()
    }

    /// Remember a compile failure. `kept_previous` says the indexed methods are from the last
    /// directory that compiled, so a detail read can still name fields.
    pub fn note_compile_failure(&mut self, err: &str, kept_previous: bool) {
        let text = if kept_previous {
            format!("proto compile failed; the last successful schema is still in use. {err}")
        } else {
            format!("proto compile failed. {err}")
        };
        self.diagnostic = Some(note(&text));
    }

    /// Decode one protobuf payload.
    ///
    /// `message_name` is the fully qualified type from an `rpc`. Without it, the payload is read
    /// as wire fields.
    pub fn decode_payload(&self, bytes: &[u8], message_name: Option<&str>) -> GrpcDecoded {
        if bytes.len() > MAX_DECODE_BYTES {
            return failed(
                if message_name.is_some() {
                    GrpcDecodeSource::Schema
                } else {
                    GrpcDecodeSource::Wire
                },
                message_name.map(str::to_string),
                "protobuf payload exceeds the decode budget",
            );
        }
        match message_name {
            Some(name) => self.decode_schema(bytes, name),
            None => decode_wire_view(bytes),
        }
    }

    fn message_name(&self, path: &str, response: bool) -> Option<&str> {
        let key = normalize_method_path(path);
        let method = self.methods.get(&key)?;
        Some(if response {
            method.response.as_str()
        } else {
            method.request.as_str()
        })
    }

    fn decode_schema(&self, bytes: &[u8], name: &str) -> GrpcDecoded {
        let Some(descriptor) = self.descriptors.get_message_by_name(name) else {
            return failed(
                GrpcDecodeSource::Schema,
                Some(name.to_string()),
                &format!("message {name} is not in the schema"),
            );
        };
        let message = match DynamicMessage::decode(descriptor, bytes) {
            Ok(message) => message,
            Err(err) => {
                return failed(
                    GrpcDecodeSource::Schema,
                    Some(name.to_string()),
                    &format!("protobuf decode failed: {err}"),
                );
            }
        };
        match serde_json::to_value(&message) {
            Ok(value) => GrpcDecoded {
                source: GrpcDecodeSource::Schema,
                message_type: Some(name.to_string()),
                value: Some(value),
                error: None,
            },
            Err(err) => failed(
                GrpcDecodeSource::Schema,
                Some(name.to_string()),
                &format!("protobuf json failed: {err}"),
            ),
        }
    }
}

/// Remembers which directory was compiled, and the file metadata that produced it.
///
/// A failed compile of the same directory keeps the previous pool and records the diagnostic.
/// A different directory that fails does not keep the previous directory's messages.
#[derive(Debug)]
pub struct ProtoSchemaCache {
    loaded: Option<(PathBuf, String)>,
    pool: ProtoPool,
}

impl Default for ProtoSchemaCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtoSchemaCache {
    pub fn new() -> Self {
        Self {
            loaded: None,
            pool: ProtoPool::empty(),
        }
    }

    /// Pool for `dir`, recompiling only when the `.proto` files there have changed.
    pub fn pool_for(&mut self, dir: &Path) -> &ProtoPool {
        let fingerprint = fingerprint(dir);
        let same = self
            .loaded
            .as_ref()
            .is_some_and(|(path, fp)| path == dir && fp == &fingerprint);
        if !same {
            let previously_this_dir = self.loaded.as_ref().is_some_and(|(path, _)| path == dir);
            match ProtoPool::compile_dir(dir) {
                Ok(pool) => self.pool = pool,
                Err(err) => {
                    if previously_this_dir {
                        self.pool.note_compile_failure(&err, true);
                    } else {
                        let mut pool = ProtoPool::empty();
                        pool.note_compile_failure(&err, false);
                        self.pool = pool;
                    }
                }
            }
            self.loaded = Some((dir.to_path_buf(), fingerprint));
        }
        &self.pool
    }
}

/// Fill `decoded` on gRPC messages in this flow.
///
/// `application/grpc+json` is already text and is left alone. A redacted body is left alone so
/// the view cannot restore bytes the redaction removed.
pub fn apply_to_flow(flow: &mut Flow, pool: &ProtoPool) {
    let Layer::Http(http) = &mut flow.layer else {
        return;
    };
    let path = http.request.url.path().to_string();
    let request_headers = http.request.headers.clone();
    let response_headers = http
        .response
        .as_ref()
        .map(|response| response.headers.clone());
    let request_type = pool.message_name(&path, false).map(str::to_string);
    let response_type = pool.message_name(&path, true).map(str::to_string);

    if let Some(body) = &mut http.request.body {
        fill_body(body, &request_headers, pool, request_type.as_deref(), &path);
    }
    if let Some(response) = &mut http.response
        && let Some(body) = &mut response.body
    {
        fill_body(
            body,
            response_headers.as_deref().unwrap_or(&[]),
            pool,
            response_type.as_deref(),
            &path,
        );
    }
}

/// Whether a detail read has anything to decode.
pub fn flow_has_grpc(flow: &Flow) -> bool {
    let Layer::Http(http) = &flow.layer else {
        return false;
    };
    body_is_grpc(http.request.body.as_ref())
        || http
            .response
            .as_ref()
            .is_some_and(|response| body_is_grpc(response.body.as_ref()))
}

fn body_is_grpc(body: Option<&BodyData>) -> bool {
    body.is_some_and(|body| body.grpc.is_some())
}

fn fill_body(
    body: &mut BodyData,
    headers: &[(String, String)],
    pool: &ProtoPool,
    message_name: Option<&str>,
    path: &str,
) {
    let Some(grpc) = body.grpc.as_mut() else {
        return;
    };
    if is_grpc_json(headers) || body.content == "[REDACTED]" {
        return;
    }
    grpc.schema_note = schema_note(pool, message_name, path);

    let Ok(bytes) = BASE64.decode(body.content.as_bytes()) else {
        return;
    };
    let frames = frame_payloads(&bytes);
    let encoding = grpc_encoding(headers);
    for message in &mut grpc.messages {
        let Some(frame) = frames.get(message.index as usize) else {
            break;
        };
        message.decoded = Some(decode_frame(frame, encoding.as_deref(), pool, message_name));
    }
}

fn decode_frame(
    frame: &Frame<'_>,
    encoding: Option<&str>,
    pool: &ProtoPool,
    message_name: Option<&str>,
) -> GrpcDecoded {
    let source = if message_name.is_some() {
        GrpcDecodeSource::Schema
    } else {
        GrpcDecodeSource::Wire
    };
    let payload = match inflate(frame, encoding) {
        Ok(payload) => payload,
        Err(err) => return failed(source, message_name.map(str::to_string), &err),
    };
    pool.decode_payload(&payload, message_name)
}

fn schema_note(pool: &ProtoPool, message_name: Option<&str>, path: &str) -> Option<String> {
    if let Some(diagnostic) = pool.diagnostic() {
        return Some(diagnostic.to_string());
    }
    if message_name.is_none() && pool.has_methods() {
        return Some(format!("no rpc matches {path}"));
    }
    None
}

fn is_grpc_json(headers: &[(String, String)]) -> bool {
    content_type(headers).contains("+json")
}

fn content_type(headers: &[(String, String)]) -> String {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.to_ascii_lowercase())
        .unwrap_or_default()
}

fn grpc_encoding(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("grpc-encoding"))
        .map(|(_, value)| {
            value
                .split([',', ';'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|value| !value.is_empty())
}

struct Frame<'a> {
    compressed: bool,
    payload: &'a [u8],
}

fn frame_payloads(bytes: &[u8]) -> Vec<Frame<'_>> {
    const PREFIX_LEN: usize = 5;
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset + PREFIX_LEN <= bytes.len() {
        let compressed = bytes[offset] != 0;
        let length = u32::from_be_bytes([
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
            bytes[offset + 4],
        ]) as usize;
        let start = offset + PREFIX_LEN;
        let end = start + length;
        if end > bytes.len() {
            break;
        }
        frames.push(Frame {
            compressed,
            payload: &bytes[start..end],
        });
        offset = end;
    }
    frames
}

fn inflate(frame: &Frame<'_>, encoding: Option<&str>) -> Result<Vec<u8>, String> {
    if !frame.compressed {
        if frame.payload.len() > MAX_DECODE_BYTES {
            return Err("protobuf payload exceeds the decode budget".to_string());
        }
        return Ok(frame.payload.to_vec());
    }
    let encoding = encoding.unwrap_or("gzip");
    if encoding != "gzip" {
        return Err(format!("grpc-encoding {encoding} is not inflated"));
    }
    let mut decoder = flate2::read::GzDecoder::new(frame.payload);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut out)
        .map_err(|err| format!("gzip inflate failed: {err}"))?;
    if out.len() > MAX_DECODE_BYTES {
        return Err("inflated protobuf exceeds the decode budget".to_string());
    }
    Ok(out)
}

fn index_methods(descriptors: &DescriptorPool) -> HashMap<String, MethodMessages> {
    let mut methods = HashMap::new();
    for service in descriptors.services() {
        let service_name = bare_name(service.full_name());
        for method in service.methods() {
            let path = format!("/{service_name}/{}", method.name());
            methods.entry(path).or_insert_with(|| MethodMessages {
                request: bare_name(method.input().full_name()).to_string(),
                response: bare_name(method.output().full_name()).to_string(),
            });
        }
    }
    methods
}

fn bare_name(name: &str) -> &str {
    name.strip_prefix('.').unwrap_or(name)
}

fn normalize_method_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn failed(source: GrpcDecodeSource, message_type: Option<String>, error: &str) -> GrpcDecoded {
    GrpcDecoded {
        source,
        message_type,
        value: None,
        error: Some(error.to_string()),
    }
}

fn decode_wire_view(bytes: &[u8]) -> GrpcDecoded {
    let mut fields = 0u32;
    match decode_message(bytes, 0, &mut fields) {
        Ok(value) => GrpcDecoded {
            source: GrpcDecodeSource::Wire,
            message_type: None,
            value: Some(Value::Object(value)),
            error: None,
        },
        Err(err) => failed(GrpcDecodeSource::Wire, None, &err),
    }
}

fn decode_message(
    bytes: &[u8],
    depth: u32,
    fields: &mut u32,
) -> Result<Map<String, Value>, String> {
    if depth > MAX_DEPTH {
        return Err("protobuf nesting exceeds the decode budget".to_string());
    }
    let mut object = Map::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if *fields >= MAX_FIELDS {
            return Err("protobuf field count exceeds the decode budget".to_string());
        }
        let tag =
            read_varint(bytes, &mut offset).ok_or_else(|| "truncated protobuf tag".to_string())?;
        let field_number = tag >> 3;
        let wire_type = tag & 0x7;
        if field_number == 0 || field_number > 536_870_911 {
            return Err("invalid protobuf field number".to_string());
        }
        let value = match wire_type {
            0 => {
                let value = read_varint(bytes, &mut offset)
                    .ok_or_else(|| "truncated varint".to_string())?;
                json_number(value)
            }
            1 => json_number(read_fixed(bytes, &mut offset, 8)?),
            5 => json_number(read_fixed(bytes, &mut offset, 4)?),
            2 => {
                let len = read_varint(bytes, &mut offset)
                    .ok_or_else(|| "truncated length".to_string())?;
                let len = usize::try_from(len).map_err(|_| "length does not fit".to_string())?;
                if offset + len > bytes.len() {
                    return Err("truncated length-delimited field".to_string());
                }
                let slice = &bytes[offset..offset + len];
                offset += len;
                interpret_delimited(slice, depth, fields)?
            }
            3 | 4 => return Err("protobuf groups are not decoded".to_string()),
            other => return Err(format!("unknown protobuf wire type {other}")),
        };
        *fields += 1;
        push_field(&mut object, field_number.to_string(), value);
    }
    Ok(object)
}

fn interpret_delimited(bytes: &[u8], depth: u32, fields: &mut u32) -> Result<Value, String> {
    if is_textual(bytes) {
        let text = String::from_utf8(bytes.to_vec())
            .map_err(|_| "utf-8 check disagreed with itself".to_string())?;
        return Ok(Value::String(text));
    }
    let saved = *fields;
    match decode_message(bytes, depth + 1, fields) {
        Ok(object) if !object.is_empty() => Ok(Value::Object(object)),
        _ => {
            *fields = saved;
            Ok(Value::String(BASE64.encode(bytes)))
        }
    }
}

fn is_textual(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.chars()
        .all(|ch| ch == '\n' || ch == '\r' || ch == '\t' || !ch.is_control())
}

fn push_field(object: &mut Map<String, Value>, key: String, value: Value) {
    match object.remove(&key) {
        None => {
            object.insert(key, value);
        }
        Some(Value::Array(mut items)) => {
            items.push(value);
            object.insert(key, Value::Array(items));
        }
        Some(previous) => {
            object.insert(key, Value::Array(vec![previous, value]));
        }
    }
}

fn json_number(value: u64) -> Value {
    if let Ok(signed) = i64::try_from(value) {
        Value::Number(signed.into())
    } else {
        Value::Number(value.into())
    }
}

fn read_fixed(bytes: &[u8], offset: &mut usize, width: usize) -> Result<u64, String> {
    if *offset + width > bytes.len() {
        return Err(format!("truncated fixed{width}"));
    }
    let mut buf = [0u8; 8];
    buf[..width].copy_from_slice(&bytes[*offset..*offset + width]);
    *offset += width;
    Ok(u64::from_le_bytes(buf))
}

fn read_varint(bytes: &[u8], offset: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    while *offset < bytes.len() && shift < 64 {
        let byte = bytes[*offset];
        *offset += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
    None
}

fn proto_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_proto_files(dir, &mut files);
    files.sort();
    files
}

fn collect_proto_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_proto_files(&path, files);
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "proto") {
            files.push(path);
        }
    }
}

fn fingerprint(dir: &Path) -> String {
    if !dir.is_dir() {
        return "absent".to_string();
    }
    let mut parts = Vec::new();
    for path in proto_files(dir) {
        let modified = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let len = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        parts.push(format!("{}:{modified}:{len}", path.display()));
    }
    if parts.is_empty() {
        "empty".to_string()
    } else {
        parts.join("\n")
    }
}

fn note(text: &str) -> String {
    if text.chars().count() <= MAX_NOTE_CHARS {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(MAX_NOTE_CHARS)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    format!("{}…", &text[..end])
}
