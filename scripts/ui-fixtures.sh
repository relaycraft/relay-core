#!/usr/bin/env bash
#
# Start a throwaway RelayCore instance with traffic worth looking at, then screenshot the UI.
#
# The point is to review the interface against data that exercises it: non-ASCII text (this project's
# users read Chinese), errors, redirects, binary and large bodies, every common method, a WebSocket,
# and a gRPC-shaped exchange. A UI inspected only against a couple of 200s hides most of its bugs.
#
# Usage: ./scripts/ui-fixtures.sh [out-dir]
#
# Everything runs on loopback on ports 18080-18099, and the instance is killed on exit.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-/tmp/relay-core-ui}"
PROXY_PORT=18080
API_PORT=18082
CONTROL_PORT=18081
UPSTREAM_PORT=18090
GRPC_PORT=18091
WORKDIR="$(mktemp -d)"
PIDS=()

cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  pkill -f "remote-debugging-port=9222" 2>/dev/null || true
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

echo "==> building the CLI (the Web UI is embedded at build time)"
cargo build -p relay-core-cli --manifest-path "$ROOT/Cargo.toml" >/dev/null

BIN="$(cargo metadata --manifest-path "$ROOT/Cargo.toml" --format-version 1 --no-deps \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["target_directory"])')/debug/relay-core-cli"
[ -x "$BIN" ] || { echo "no CLI at $BIN" >&2; exit 1; }

echo "==> starting an upstream with text, errors, redirects and binary bodies"
python3 - "$UPSTREAM_PORT" <<'PY' &
import gzip, json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
BIG = ("x" * 4096 + "\n") * 64


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, status, body: bytes, ctype="application/json; charset=utf-8", extra=None):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/hello":
            self._send(200, json.dumps(
                {"message": "你好，世界", "status": "ok", "emoji": "🚦", "nested": {"键": "值"}},
                ensure_ascii=False).encode())
        elif path == "/big":
            self._send(200, BIG.encode(), "text/plain; charset=utf-8")
        elif path == "/binary":
            self._send(200, bytes(range(256)) * 8, "application/octet-stream")
        elif path == "/gz":
            body = gzip.compress(("压缩内容：" + BIG[:200]).encode())
            self._send(200, body, "text/plain; charset=utf-8", {"Content-Encoding": "gzip"})
        elif path == "/redirect":
            self._send(302, b"", "text/plain", {"Location": f"http://127.0.0.1:{PORT}/hello"})
        elif path == "/notfound":
            self._send(404, json.dumps({"error": "未找到该资源"}, ensure_ascii=False).encode())
        elif path == "/boom":
            self._send(500, json.dumps({"error": "internal", "detail": "失败原因"}).encode())
        elif path == "/slow":
            import time; time.sleep(2)
            self._send(200, b'{"slow":true}')
        else:
            self._send(200, json.dumps({"path": path}, ensure_ascii=False).encode())

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(length)
        self._send(201, json.dumps({"created": True, "你好": "世界"}, ensure_ascii=False).encode())

    def do_PUT(self): self.do_POST()
    def do_PATCH(self): self.do_POST()
    def do_DELETE(self): self._send(204, b"")
    def do_HEAD(self): self._send(200, b"")

    def log_message(self, *args): pass


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
PY
PIDS+=($!)

echo "==> starting an h2c upstream that answers like gRPC"
python3 - "$GRPC_PORT" <<'PY' &
import socket, struct, sys, threading

PORT = int(sys.argv[1])
# Enough of HTTP/2 to answer a preface and one request with a gRPC-shaped response, so the capture has
# a real h2c flow with trailers to render.
PREFACE = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
H2_HEADERS = b"\x00\x00\x00\x04\x00\x00\x00\x00\x00"      # SETTINGS
H2_ACK = b"\x00\x00\x00\x04\x01\x00\x00\x00\x00"           # SETTINGS ack


def frame(kind, flags, stream, payload=b""):
    return struct.pack(">I", len(payload))[1:] + bytes([kind, flags]) + struct.pack(">I", stream) + payload


def serve(conn):
    try:
        conn.recv(24)
        conn.sendall(H2_ACK)
        while True:
            data = conn.recv(65535)
            if not data:
                return
            if data[3:4] == b"\x01":  # HEADERS
                conn.sendall(H2_ACK)
                body = b"\x00\x00\x00\x00\x05hello"        # one 5-byte gRPC message
                headers = (b"\x88" + b"\x0f\x10" + b"\x0f\x0d" + b"\x0f\x0d\x09"
                           + b"\x0f\x0d\x0b\x61\x70\x70\x6c\x69\x63\x61\x74\x69\x6f\x6e"
                           b"\x2f\x67\x72\x70\x63")
                conn.sendall(frame(0x01, 0x04, 1, headers))
                conn.sendall(frame(0x00, 0x00, 1, body))
                trailers = b"\x0f\x0b\x67\x72\x70\x63\x2d\x73\x74\x61\x74\x75\x73\x01\x30"
                conn.sendall(frame(0x01, 0x05, 1, trailers))
    except OSError:
        pass
    finally:
        conn.close()


server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", PORT))
server.listen(16)
while True:
    conn, _ = server.accept()
    threading.Thread(target=serve, args=(conn,), daemon=True).start()
PY
PIDS+=($!)

sleep 2

echo "==> starting relay-core with the Web UI"
"$BIN" run --listen "127.0.0.1:$PROXY_PORT" --control-port "$CONTROL_PORT" --api-port "$API_PORT" --web \
  > "$WORKDIR/proxy.log" 2>&1 &
PIDS+=($!)

for _ in $(seq 1 40); do
  if curl -sf -o /dev/null "http://127.0.0.1:$API_PORT/api/v1/flows"; then break; fi
  sleep 0.5
done

echo "==> driving traffic through the proxy"
PROXY="http://127.0.0.1:$PROXY_PORT"
U="http://127.0.0.1:$UPSTREAM_PORT"
fetch() { curl -s -o /dev/null -x "$PROXY" --max-time 12 "$1" || true; }

fetch "$U/hello"
fetch "$U/hello?token=secret-value&q=中文"
fetch "$U/big"
fetch "$U/gz"
fetch "$U/binary"
fetch "$U/notfound"
fetch "$U/boom"
fetch "$U/redirect"
fetch "$U/slow"
curl -s -o /dev/null -x "$PROXY" -X POST -H 'Content-Type: application/json' \
  --data '{"name":"测试","value":42}' "$U/create" || true
curl -s -o /dev/null -x "$PROXY" -X PUT --data '更新' "$U/update" || true
curl -s -o /dev/null -x "$PROXY" -X DELETE "$U/delete" || true
curl -s -o /dev/null -x "$PROXY" -I "$U/hello" || true
# A gRPC-shaped call to the h2c upstream: h2c through the proxy, which is the path most likely to
# render oddly because the framing is binary.
curl -s -o /dev/null -x "$PROXY" --http2-prior-knowledge --max-time 12 \
  -H 'content-type: application/grpc+proto' -H 'te: trailers' \
  --data-binary $'\x00\x00\x00\x00\x03req' "http://127.0.0.1:$GRPC_PORT/pkg.Service/Method" || true
# Real TLS traffic, so the detail view has a certificate-backed flow too.
fetch "https://example.com/"

sleep 1
COUNT=$(curl -s "http://127.0.0.1:$API_PORT/api/v1/flows?limit=100" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("items", [])))')
echo "==> $COUNT flows captured"

echo "==> starting headless Chrome with a debugging port"
rm -rf "$WORKDIR/chrome"
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless --disable-gpu --no-sandbox --hide-scrollbars \
  --remote-debugging-port=9222 --user-data-dir="$WORKDIR/chrome" \
  --window-size=1600,1000 about:blank > "$WORKDIR/chrome.log" 2>&1 &
PIDS+=($!)

node "$ROOT/scripts/ui-snapshot.mjs" "http://127.0.0.1:$API_PORT/" "$OUT" "${@:2}"
echo "==> screenshots in $OUT"
