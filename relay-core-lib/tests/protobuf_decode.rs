//! Protobuf views over captured gRPC bodies.
//!
//! The proxy records framing only. These tests are the read-time contract: a `.proto` directory
//! names fields, and a payload with no schema is still a field tree.

use std::path::Path;

use data_encoding::BASE64;
use relay_core_api::flow::{
    BodyData, Flow, HttpLayer, HttpRequest, HttpResponse, Layer, NetworkInfo, ResponseTiming,
    TransportProtocol,
};
use relay_core_api::grpc::GrpcDecodeSource;
use relay_core_lib::protobuf::{ProtoPool, ProtoSchemaCache, apply_to_flow};
use relay_core_lib::proxy::body_codec::process_body_with_framing;

#[test]
fn a_payload_without_schema_is_a_field_tree() {
    let payload = hello_request_bytes();
    let decoded = ProtoPool::empty().decode_payload(&payload, None);

    assert_eq!(decoded.source, GrpcDecodeSource::Wire);
    assert_eq!(decoded.error, None);
    assert_eq!(
        decoded.value,
        Some(serde_json::json!({ "1": "relay", "2": 7 }))
    );
}

#[test]
fn a_proto_directory_names_the_request_and_response() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_schema(dir.path());
    let pool = ProtoPool::compile_dir(dir.path()).expect("schema compiles");

    let request = pool.decode_payload(&hello_request_bytes(), Some("demo.v1.HelloRequest"));
    assert_eq!(request.source, GrpcDecodeSource::Schema);
    assert_eq!(
        request.message_type.as_deref(),
        Some("demo.v1.HelloRequest")
    );
    assert_eq!(
        request.value,
        Some(serde_json::json!({ "name": "relay", "id": 7 }))
    );

    let mut flow = grpc_flow();
    apply_to_flow(&mut flow, &pool);
    let http = match &flow.layer {
        Layer::Http(http) => http,
        other => panic!("http flow, got {other:?}"),
    };
    let request_body = http.request.body.as_ref().expect("request body");
    let response_body = http
        .response
        .as_ref()
        .expect("response")
        .body
        .as_ref()
        .expect("response body");
    assert_eq!(
        request_body.grpc.as_ref().unwrap().messages[0]
            .decoded
            .as_ref()
            .unwrap()
            .value,
        Some(serde_json::json!({ "name": "relay", "id": 7 }))
    );
    assert_eq!(
        response_body.grpc.as_ref().unwrap().messages[0]
            .decoded
            .as_ref()
            .unwrap()
            .value,
        Some(serde_json::json!({ "message": "ok" }))
    );
    assert_eq!(request_body.grpc.as_ref().unwrap().schema_note, None);
}

#[test]
fn a_broken_reload_keeps_the_last_schema_and_says_so() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_schema(dir.path());
    let mut cache = ProtoSchemaCache::new();
    assert!(cache.pool_for(dir.path()).has_methods());

    std::fs::write(
        dir.path().join("greeter.proto"),
        "syntax = \"proto3\";\nnot a proto\n",
    )
    .expect("overwrite");
    let pool = cache.pool_for(dir.path());
    let note = pool.diagnostic().expect("compile failure is visible");
    assert!(
        note.contains("last successful schema"),
        "note should say the previous schema is still in use, got {note}"
    );

    let decoded = pool.decode_payload(&hello_request_bytes(), Some("demo.v1.HelloRequest"));
    assert_eq!(decoded.source, GrpcDecodeSource::Schema);
    assert_eq!(
        decoded.value,
        Some(serde_json::json!({ "name": "relay", "id": 7 }))
    );
}

#[test]
fn an_unmatched_method_falls_back_to_wire_fields_and_says_so() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_schema(dir.path());
    let pool = ProtoPool::compile_dir(dir.path()).expect("schema compiles");
    let mut flow = grpc_flow();
    if let Layer::Http(http) = &mut flow.layer {
        http.request.url = "http://grpc.local/other.Svc/Missing".parse().unwrap();
    }

    apply_to_flow(&mut flow, &pool);

    let http = match &flow.layer {
        Layer::Http(http) => http,
        other => panic!("http flow, got {other:?}"),
    };
    let grpc = http.request.body.as_ref().unwrap().grpc.as_ref().unwrap();
    assert_eq!(
        grpc.schema_note.as_deref(),
        Some("no rpc matches /other.Svc/Missing")
    );
    assert_eq!(
        grpc.messages[0].decoded.as_ref().unwrap().source,
        GrpcDecodeSource::Wire
    );
}

#[test]
fn grpc_json_and_the_capture_path_do_not_grow_a_protobuf_view() {
    let headers = vec![(
        "content-type".to_string(),
        "application/grpc+json".to_string(),
    )];
    let body = frame(false, br#"{"name":"relay"}"#);
    let (_, _, grpc) = process_body_with_framing(&body, &headers);
    let grpc = grpc.expect("json grpc is still framed");
    assert!(grpc.messages[0].decoded.is_none());
    assert!(grpc.schema_note.is_none());

    let pool = ProtoPool::empty();
    let mut flow = grpc_flow();
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = headers;
        http.request.body.as_mut().unwrap().content = BASE64.encode(&body);
        // The framing helper above is what capture stores. Apply must not treat JSON as protobuf.
        http.request.body.as_mut().unwrap().grpc = Some(grpc);
    }
    apply_to_flow(&mut flow, &pool);
    let Layer::Http(http) = &flow.layer else {
        panic!("http");
    };
    assert!(
        http.request
            .body
            .as_ref()
            .unwrap()
            .grpc
            .as_ref()
            .unwrap()
            .messages[0]
            .decoded
            .is_none()
    );
}

#[test]
fn a_gzip_message_is_inflated_before_the_schema_is_applied() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_schema(dir.path());
    let pool = ProtoPool::compile_dir(dir.path()).expect("schema compiles");

    let payload = hello_request_bytes();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &payload).expect("gzip");
    let compressed = encoder.finish().expect("gzip finish");
    let framed = frame(true, &compressed);
    let headers = vec![
        ("content-type".to_string(), "application/grpc".to_string()),
        ("grpc-encoding".to_string(), "gzip".to_string()),
    ];
    let (encoding, content, grpc) = process_body_with_framing(&framed, &headers);

    let mut flow = grpc_flow();
    if let Layer::Http(http) = &mut flow.layer {
        http.request.headers = headers;
        http.request.body = Some(BodyData {
            encoding,
            content,
            size: framed.len() as u64,
            grpc,
        });
    }
    apply_to_flow(&mut flow, &pool);

    let Layer::Http(http) = &flow.layer else {
        panic!("http");
    };
    let decoded = http
        .request
        .body
        .as_ref()
        .unwrap()
        .grpc
        .as_ref()
        .unwrap()
        .messages[0]
        .decoded
        .as_ref()
        .expect("gzip payload is decoded");
    assert_eq!(decoded.source, GrpcDecodeSource::Schema);
    assert_eq!(
        decoded.value,
        Some(serde_json::json!({ "name": "relay", "id": 7 }))
    );
}

/// `HelloRequest { string name = 1; int32 id = 2; }` with name = "relay", id = 7.
fn hello_request_bytes() -> Vec<u8> {
    vec![0x0a, 0x05, b'r', b'e', b'l', b'a', b'y', 0x10, 0x07]
}

fn hello_reply_bytes() -> Vec<u8> {
    vec![0x0a, 0x02, b'o', b'k']
}

fn frame(compressed: bool, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![u8::from(compressed)];
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn write_schema(dir: &Path) {
    std::fs::write(
        dir.join("types.proto"),
        r#"syntax = "proto3";
package demo.v1;
message HelloRequest {
  string name = 1;
  int32 id = 2;
}
message HelloReply {
  string message = 1;
}
"#,
    )
    .expect("types.proto");
    std::fs::write(
        dir.join("greeter.proto"),
        r#"syntax = "proto3";
package demo.v1;
import "types.proto";
service Greeter {
  rpc SayHello (HelloRequest) returns (HelloReply);
}
"#,
    )
    .expect("greeter.proto");
}

fn grpc_body(payload: &[u8]) -> BodyData {
    let framed = frame(false, payload);
    let (_, _, grpc) = process_body_with_framing(
        &framed,
        &[(
            "content-type".to_string(),
            "application/grpc+proto".to_string(),
        )],
    );
    BodyData {
        encoding: "base64".to_string(),
        content: BASE64.encode(&framed),
        size: framed.len() as u64,
        grpc,
    }
}

fn grpc_flow() -> Flow {
    use chrono::Utc;
    use uuid::Uuid;

    Flow {
        id: Uuid::new_v4(),
        start_time: Utc::now(),
        end_time: None,
        close_reason: None,
        network: NetworkInfo {
            client_ip: "127.0.0.1".to_string(),
            client_port: 1,
            server_ip: "127.0.0.1".to_string(),
            server_port: 50051,
            server_host: None,
            protocol: TransportProtocol::TCP,
            tls: false,
            tls_version: None,
            sni: None,
        },
        layer: Layer::Http(HttpLayer {
            request: HttpRequest {
                method: "POST".to_string(),
                url: "http://grpc.local/demo.v1.Greeter/SayHello"
                    .parse()
                    .unwrap(),
                version: "HTTP/2".to_string(),
                headers: vec![(
                    "content-type".to_string(),
                    "application/grpc+proto".to_string(),
                )],
                cookies: vec![],
                query: vec![],
                body: Some(grpc_body(&hello_request_bytes())),
            },
            response: Some(HttpResponse {
                status: 200,
                status_text: "OK".to_string(),
                version: "HTTP/2".to_string(),
                headers: vec![(
                    "content-type".to_string(),
                    "application/grpc+proto".to_string(),
                )],
                cookies: vec![],
                body: Some(grpc_body(&hello_reply_bytes())),
                timing: ResponseTiming {
                    time_to_first_byte: None,
                    time_to_last_byte: None,
                    connect_time_ms: None,
                    ssl_time_ms: None,
                },
                trailers: vec![],
                sse_events: vec![],
            }),
            error: None,
        }),
        tags: vec![],
        meta: std::collections::HashMap::new(),
        resilience_trace: None,
        rule_variables: std::collections::HashMap::new(),
        matched_rules: vec![],
        connection_id: None,
        parent_flow_id: None,
        protocol_stack: vec![],
    }
}
