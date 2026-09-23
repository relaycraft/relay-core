//! Wire checks for script hooks.
//!
//! Header edits must not replace the upstream body, and `body.json()` must run against
//! bytes buffered on the proxy runtime. In-memory `Full` bodies do not catch either bug.

use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use relay_core_api::flow::FlowUpdate;
use relay_core_api::policy::ProxyPolicy;
use relay_core_lib::engine::TcpCaptureSource;
use relay_core_lib::interceptor::Interceptor;
use relay_core_lib::start_proxy;
use relay_core_lib::tls::CertificateAuthority;
use relay_core_script::ScriptInterceptor;
use std::net::SocketAddr;
use std::sync::{Arc, Once};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
    });
}

async fn spawn_upstream(body: &'static str) -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("upstream addr");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = Vec::new();
        loop {
            let n = match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            received.extend_from_slice(&buf[..n]);
            if received.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let payload = body.as_bytes();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.write_all(payload).await;
    });
    addr
}

async fn round_trip(script: &str, upstream_body: &'static str) -> (u16, String, Vec<u8>) {
    init_crypto();
    let interceptor = ScriptInterceptor::new().await.expect("script engine");
    interceptor
        .load_script(script)
        .await
        .expect("script should load");

    let upstream = spawn_upstream(upstream_body).await;
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();
    let source = TcpCaptureSource::new(listener);
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
    tokio::spawn(async move { while flow_rx.recv().await.is_some() {} });
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(ProxyPolicy::default());
    let interceptor = Arc::new(interceptor) as Arc<dyn Interceptor>;

    tokio::spawn(async move {
        let _ = start_proxy(
            source,
            flow_tx,
            interceptor,
            ca,
            policy_rx,
            None,
            None,
            None,
        )
        .await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("proxy handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let uri = format!("http://{upstream}/probe")
        .parse::<hyper::Uri>()
        .expect("uri");
    let req = hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", upstream.to_string())
        .header("user-agent", "script-wire")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .expect("request");
    let resp = sender.send_request(req).await.expect("send via proxy");
    let status = resp.status().as_u16();
    let mut headers = String::new();
    for (name, value) in resp.headers() {
        headers.push_str(&format!(
            "{}: {}\n",
            name,
            value.to_str().unwrap_or("<binary>")
        ));
    }
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect response")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

#[tokio::test]
async fn on_response_headers_keeps_the_upstream_body() {
    let (status, headers, body) = round_trip(
        r#"
        globalThis.onResponseHeaders = (ctx, flow) => {
            flow.layer.data.response.headers.push(["X-Relay-Script", "headers-ok"]);
            return flow;
        };
        "#,
        "upstream-body-ok",
    )
    .await;

    assert_eq!(status, 200, "headers:\n{headers}");
    assert!(
        headers
            .to_lowercase()
            .contains("x-relay-script: headers-ok"),
        "script header must reach the client, got:\n{headers}"
    );
    assert_eq!(
        body, b"upstream-body-ok",
        "returning the flow from onResponseHeaders must not drop the upstream body"
    );
}

#[tokio::test]
async fn async_on_response_json_rewrite_reaches_the_client() {
    let (status, headers, body) = round_trip(
        r#"
        globalThis.onResponse = async (body, flow) => {
            const data = await body.json();
            data.n = data.n + 1;
            const content = JSON.stringify(data);
            flow.layer.data.response.body = {
                encoding: "utf-8",
                content,
                size: content.length,
            };
            flow.layer.data.response.headers.push(["X-Relay-Script", "json"]);
            return flow;
        };
        "#,
        r#"{"n":1}"#,
    )
    .await;

    assert_eq!(
        status,
        200,
        "headers:\n{headers}\nbody: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(
        headers.to_lowercase().contains("x-relay-script: json"),
        "rewritten response must carry the script header, got:\n{headers}"
    );
    assert_eq!(body, br#"{"n":2}"#);
}

/// A response hook must see a decoded body under the daemon's default `prefixed` observation.
///
/// `prefixed` keeps streaming when nobody rewrites. A script that reads
/// `response.body.content` is a rewrite, so a gzip upstream has to be buffered and decoded
/// before `onResponseHeaders` runs. Leaving that to `body_observation = full` makes response
/// rewriting fail until someone changes policy.
#[tokio::test]
async fn a_response_hook_rewrites_a_gzip_body_under_prefixed_observation() {
    init_crypto();
    let plain = b"original-body";
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, plain).expect("gzip");
    let gzipped = encoder.finish().expect("finish gzip");

    let interceptor = ScriptInterceptor::new().await.expect("script engine");
    interceptor
        .load_script(
            r#"
            globalThis.onResponseHeaders = (_ctx, flow) => {
                const body = flow.layer.data.response && flow.layer.data.response.body;
                if (!body || body.content == null) return flow;
                const next = "rewritten";
                body.content = next;
                body.encoding = "utf-8";
                body.size = next.length;
                return flow;
            };
            "#,
        )
        .await
        .expect("script should load");

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let upstream = listener.local_addr().expect("upstream addr");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = Vec::new();
        loop {
            let n = match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            received.extend_from_slice(&buf[..n]);
            if received.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            gzipped.len()
        );
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(&gzipped).await;
    });

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();
    let source = TcpCaptureSource::new(listener);
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
    tokio::spawn(async move { while flow_rx.recv().await.is_some() {} });
    let policy = ProxyPolicy {
        body_observation: relay_core_api::body_plan::BodyObservation::Prefixed,
        ..ProxyPolicy::default()
    };
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(policy);
    let interceptor = Arc::new(interceptor) as Arc<dyn Interceptor>;
    tokio::spawn(async move {
        let _ = start_proxy(
            source,
            flow_tx,
            interceptor,
            ca,
            policy_rx,
            None,
            None,
            None,
        )
        .await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("proxy handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = hyper::Request::builder()
        .method("GET")
        .uri(format!("http://{upstream}/probe"))
        .header("host", upstream.to_string())
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .expect("request");
    let resp = sender.send_request(req).await.expect("send via proxy");
    let encoding = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect response")
        .to_bytes();
    let decoded = if encoding.as_deref() == Some("gzip") {
        let mut decoder = flate2::read::GzDecoder::new(bytes.as_ref());
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut out).expect("gunzip");
        out
    } else {
        bytes.to_vec()
    };
    assert_eq!(
        decoded,
        b"rewritten",
        "a response hook must rewrite the decoded gzip body under prefixed observation, got {}",
        String::from_utf8_lossy(&decoded)
    );
}

/// A response hook must not hold an event stream until the upstream body ends.
///
/// Header and body hooks both ask for a buffered response. An event stream has no end, so
/// waiting for that buffer means the client never sees the response headers.
#[tokio::test]
async fn an_event_stream_reaches_the_client_while_a_response_hook_is_loaded() {
    init_crypto();
    let interceptor = ScriptInterceptor::new().await.expect("script engine");
    interceptor
        .load_script(
            r#"
            globalThis.onResponseHeaders = (_ctx, flow) => {
                flow.layer.data.response.headers.push(["x-sse", "1"]);
                return flow;
            };
            globalThis.onResponse = (_body, _flow) => {};
            "#,
        )
        .await
        .expect("script should load");

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind upstream");
    let upstream = listener.local_addr().expect("upstream addr");
    tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = Vec::new();
        loop {
            let n = match socket.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            received.extend_from_slice(&buf[..n]);
            if received.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(b": hello\n\n").await;
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    });

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind proxy");
    let proxy_port = listener.local_addr().expect("proxy addr").port();
    let source = TcpCaptureSource::new(listener);
    let ca = Arc::new(CertificateAuthority::new().expect("create CA"));
    let (flow_tx, mut flow_rx) = tokio::sync::mpsc::channel::<FlowUpdate>(64);
    tokio::spawn(async move { while flow_rx.recv().await.is_some() {} });
    let policy = ProxyPolicy {
        body_observation: relay_core_api::body_plan::BodyObservation::Prefixed,
        ..ProxyPolicy::default()
    };
    let (_policy_tx, policy_rx) = tokio::sync::watch::channel(policy);
    let interceptor = Arc::new(interceptor) as Arc<dyn Interceptor>;
    tokio::spawn(async move {
        let _ = start_proxy(
            source,
            flow_tx,
            interceptor,
            ca,
            policy_rx,
            None,
            None,
            None,
        )
        .await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
        .await
        .expect("connect proxy");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .expect("proxy handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = hyper::Request::builder()
        .method("GET")
        .uri(format!("http://{upstream}/events"))
        .header("host", upstream.to_string())
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .expect("request");
    let resp = tokio::time::timeout(std::time::Duration::from_secs(2), sender.send_request(req))
        .await
        .expect("event-stream headers must arrive before the upstream body ends")
        .expect("send via proxy");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("x-sse")
            .and_then(|value| value.to_str().ok()),
        Some("1")
    );
}
