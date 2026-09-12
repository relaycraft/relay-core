//! Minimal high-throughput HTTP/1.1 upstream for the benchmark harness.
//!
//! Replaces `echo_server.py`, which is single-threaded Python and saturates near 2.7k req/s on an
//! M4 Max — far below the 10k req/s throughput DoD, so every measurement was really measuring the
//! upstream rather than the proxy.
//!
//! Behaviour mirrors the Python server's contract closely enough for benchmarks:
//! * `GET /payload/<KB>` → a deterministic body of `<KB>` KiB
//! * any other path → the request target echoed back as text
//!
//! Deliberately dependency-free (std only) and built outside the cargo workspace by
//! `benchmarks/bench_minimal.sh`, so it cannot affect workspace lint/test gates.

use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

const MAX_BODY_KB: usize = 2048;

fn main() {
    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(19100);
    let tls_mode = env::var("TLS_PORT").is_ok();
    if tls_mode {
        eprintln!("rust_echo_server: TLS mode is not supported; ignoring TLS_PORT");
    }

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind echo server");
    println!("rust echo server listening on 127.0.0.1:{port}");

    let payload_cache: Arc<Vec<Vec<u8>>> = Arc::new(
        (0..=MAX_BODY_KB)
            .map(|kb| {
                let mut body = Vec::with_capacity(kb * 1024);
                let chunk = b"abcdefghijklmnopqrstuvwxyz0123456789";
                let mut written = 0;
                while written < kb * 1024 {
                    let take = chunk.len().min(kb * 1024 - written);
                    body.extend_from_slice(&chunk[..take]);
                    written += take;
                }
                body
            })
            .collect(),
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let cache = Arc::clone(&payload_cache);
                // Thread per connection keeps this trivially correct; the harness drives
                // 100 concurrent connections, which is well within budget.
                thread::spawn(move || {
                    let _ = serve(stream, &cache);
                });
            }
            Err(_) => continue,
        }
    }
}

fn serve(stream: TcpStream, cache: &[Vec<u8>]) -> std::io::Result<()> {
    stream.set_nodelay(true).ok();
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    loop {
        // ── request line ──
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(()); // client closed
        }
        let request_line = request_line.trim_end().to_string();
        if request_line.is_empty() {
            continue;
        }
        let mut parts = request_line.split(' ');
        let _method = parts.next().unwrap_or("GET");
        let target = parts.next().unwrap_or("/").to_string();

        // ── headers ──
        let mut content_length = 0usize;
        let mut keep_alive = true; // HTTP/1.1 default
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                let name = name.trim();
                let value = value.trim();
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.parse().unwrap_or(0);
                } else if name.eq_ignore_ascii_case("connection") {
                    keep_alive = !value.eq_ignore_ascii_case("close");
                }
            }
        }

        // ── body (drained so the connection stays reusable) ──
        if content_length > 0 {
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body)?;
        }

        // ── response ──
        let body: &[u8] = match target.strip_prefix("/payload/") {
            Some(kb) => {
                let kb: usize = kb.parse().unwrap_or(1).min(MAX_BODY_KB);
                &cache[kb]
            }
            None => target.as_bytes(),
        };

        let connection_header = if keep_alive { "keep-alive" } else { "close" };
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Echo-Target: {target}\r\nContent-Length: {}\r\nConnection: {connection_header}\r\n\r\n",
            body.len()
        );
        writer.write_all(head.as_bytes())?;
        writer.write_all(body)?;
        writer.flush()?;

        if !keep_alive {
            return Ok(());
        }
    }
}
