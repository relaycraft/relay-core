use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use hyper::{Request, StatusCode};
use relay_core_api::flow::{Flow, NetworkInfo, TransportProtocol};
use relay_core_lib::interceptor::HttpBody;
use relay_core_lib::proxy::outbound::{
    DirectConnector, HttpUpstreamConnector, OutboundConnector, UpstreamError,
    upstream_proxy_authorization,
};

/// A mock upstream HTTP proxy that can be configured to return a specific CONNECT status.
struct MockUpstream {
    listener: TcpListener,
    connect_status: u16,
}

impl MockUpstream {
    async fn bind(connect_status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        Self {
            listener,
            connect_status,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.listener.local_addr().unwrap()
    }

    fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = self.listener.accept().await else {
                    break;
                };
                let status = self.connect_status;
                tokio::spawn(async move {
                    handle_upstream_conn(stream, status).await;
                });
            }
        })
    }
}

async fn handle_upstream_conn(mut stream: TcpStream, connect_status: u16) {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap_or(0);
    if n == 0 {
        return;
    }
    let request = String::from_utf8_lossy(&buf[..n]);

    if request.starts_with("CONNECT") {
        let resp = format!(
            "HTTP/1.1 {} {}\r\nProxy-Connection: close\r\n\r\n",
            connect_status,
            if (200..300).contains(&connect_status) {
                "Connection Established"
            } else {
                "Forbidden"
            }
        );
        stream.write_all(resp.as_bytes()).await.ok();

        if (200..300).contains(&connect_status) {
            let connect_line = request.lines().next().unwrap_or("");
            let target = connect_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("127.0.0.1:80");
            let Ok(target_stream) = TcpStream::connect(target).await else {
                return;
            };
            let (mut tr, mut tw) = target_stream.into_split();
            let (mut cr, mut cw) = stream.into_split();
            tokio::spawn(async move { tokio::io::copy(&mut tr, &mut cw).await.ok() });
            tokio::spawn(async move { tokio::io::copy(&mut cr, &mut tw).await.ok() });
        }
    } else {
        let first_line = request.lines().next().unwrap_or("");
        let parts: Vec<&str> = first_line.split_whitespace().collect();
        if parts.len() < 2 {
            return;
        }
        let url_str = parts[1];
        let Ok(url) = url::Url::parse(url_str) else {
            return;
        };
        let host = url.host_str().unwrap_or("127.0.0.1");
        let port = url.port().unwrap_or(80);
        let path = url.path();
        let query = url.query().map(|q| format!("?{}", q)).unwrap_or_default();

        let Ok(mut target) = TcpStream::connect(format!("{}:{}", host, port)).await else {
            return;
        };

        let fwd_req = format!(
            "GET {}{} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            path, query, host
        );
        target.write_all(fwd_req.as_bytes()).await.ok();
        let mut resp_buf = Vec::new();
        target.read_to_end(&mut resp_buf).await.ok();
        stream.write_all(&resp_buf).await.ok();
    }
}

/// Start a simple HTTP target server that returns "hello".
async fn start_target_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let resp =
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";
                let _ = stream.write_all(resp).await;
            });
        }
    });
    addr
}

fn dummy_flow() -> Flow {
    use chrono::Utc;
    use std::collections::HashMap;
    use uuid::Uuid;
    Flow {
        id: Uuid::new_v4(),
        start_time: Utc::now(),
        end_time: None,
        network: NetworkInfo {
            client_ip: "127.0.0.1".to_string(),
            client_port: 0,
            server_ip: "".to_string(),
            server_port: 0,
            protocol: TransportProtocol::TCP,
            tls: false,
            tls_version: None,
            sni: None,
        },
        layer: relay_core_api::flow::Layer::Unknown,
        tags: vec![],
        meta: HashMap::new(),
        resilience_trace: None,
        rule_variables: HashMap::new(),
        matched_rules: vec![],
    }
}

// ── Tests ───────────────────────────────────────────────

fn init_crypto() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[tokio::test]
async fn test_direct_connector_basic_request() {
    init_crypto();
    let target_addr = start_target_server().await;

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .unwrap()
        .https_or_http()
        .enable_http1()
        .build();
    let client: relay_core_lib::proxy::http_utils::HttpsClient =
        hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build(https);
    let connector = DirectConnector::new(Arc::new(client));

    let req = Request::builder()
        .method("GET")
        .uri(format!("http://{}", target_addr))
        .body(HttpBody::default())
        .unwrap();

    let mut flow = dummy_flow();
    let resp = connector
        .send_request(
            req,
            &target_addr.ip().to_string(),
            target_addr.port(),
            &mut flow,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_upstream_http_proxy_absolute_uri() {
    init_crypto();
    let upstream = MockUpstream::bind(200).await;
    let upstream_addr = upstream.addr();
    let _upstream_handle = upstream.spawn();

    let target_addr = start_target_server().await;

    let config = relay_core_api::policy::UpstreamProxyConfig {
        proxy_url: format!("http://{}", upstream_addr),
        auth: None,
        bypass_hosts: vec![],
        fail_open: false,
    };

    let connector = HttpUpstreamConnector::new(&config).await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "http://{}:{}/",
            target_addr.ip(),
            target_addr.port()
        ))
        .header("Host", format!("{}", target_addr))
        .body(HttpBody::default())
        .unwrap();

    let mut flow = dummy_flow();
    let resp = connector
        .send_request(
            req,
            &target_addr.ip().to_string(),
            target_addr.port(),
            &mut flow,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_upstream_proxy_connect_refused() {
    init_crypto();
    let upstream = MockUpstream::bind(403).await;
    let upstream_addr = upstream.addr();
    let _upstream_handle = upstream.spawn();

    let config = relay_core_api::policy::UpstreamProxyConfig {
        proxy_url: format!("http://{}", upstream_addr),
        auth: None,
        bypass_hosts: vec![],
        fail_open: false,
    };

    let connector = HttpUpstreamConnector::new(&config).await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri("https://example.com:443/")
        .body(HttpBody::default())
        .unwrap();

    let mut flow = dummy_flow();
    let result = connector
        .send_request(req, "example.com", 443, &mut flow)
        .await;

    match result {
        Err(UpstreamError::ConnectRefused { status }) => assert_eq!(status, 403),
        other => panic!("expected ConnectRefused(403), got {:?}", other),
    }
}

#[tokio::test]
async fn test_upstream_proxy_unreachable() {
    init_crypto();
    let dead_addr = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        drop(l);
        addr
    };

    let config = relay_core_api::policy::UpstreamProxyConfig {
        proxy_url: format!("http://{}", dead_addr),
        auth: None,
        bypass_hosts: vec![],
        fail_open: false,
    };

    let connector = HttpUpstreamConnector::new(&config).await.unwrap();

    let req = Request::builder()
        .method("GET")
        .uri("http://example.com/")
        .body(HttpBody::default())
        .unwrap();

    let mut flow = dummy_flow();
    let result = connector
        .send_request(req, "example.com", 80, &mut flow)
        .await;

    match result {
        Err(UpstreamError::Unreachable(_)) | Err(UpstreamError::Io(_)) => {}
        other => panic!("expected unreachable, got {:?}", other),
    }
}

#[tokio::test]
async fn test_upstream_proxy_authorization_header() {
    use secrecy::SecretString;

    let config = relay_core_api::policy::UpstreamProxyConfig {
        proxy_url: "http://proxy:8080".to_string(),
        auth: Some(relay_core_api::policy::UpstreamAuth {
            username: "user".to_string(),
            password: SecretString::new("pass".to_string().into()),
        }),
        bypass_hosts: vec![],
        fail_open: false,
    };

    let auth_header = upstream_proxy_authorization(&config).unwrap();
    assert!(auth_header.starts_with("Basic "));
    let decoded = String::from_utf8(
        data_encoding::BASE64
            .decode(auth_header.strip_prefix("Basic ").unwrap().as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(decoded, "user:pass");
}
