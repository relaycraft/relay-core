#!/usr/bin/env python3
"""Minimal HTTP echo server used as upstream target for proxy benchmarks.

Supported endpoints:
  - /             -> default payload size (PAYLOAD_KB_DEFAULT, default 1KB)
  - /payload/<kb> -> payload size in KB (1..2048)
"""
import http.server
import json
import os

DEFAULT_PAYLOAD_KB = int(os.environ.get("PAYLOAD_KB_DEFAULT", "1"))
MAX_PAYLOAD_KB = int(os.environ.get("PAYLOAD_KB_MAX", "2048"))


def make_payload(kb: int) -> bytes:
    kb = max(1, min(MAX_PAYLOAD_KB, kb))
    target_bytes = kb * 1024
    body = {"message": "ok", "size_kb": kb, "data": "x" * max(0, target_bytes - 80)}
    encoded = json.dumps(body).encode()
    if len(encoded) < target_bytes:
        encoded += b"x" * (target_bytes - len(encoded))
    return encoded[:target_bytes]


class EchoHandler(http.server.BaseHTTPRequestHandler):
    def _resolve_payload_kb(self) -> int:
        parts = [p for p in self.path.strip("/").split("/") if p]
        if len(parts) == 2 and parts[0] == "payload":
            try:
                return int(parts[1])
            except ValueError:
                return DEFAULT_PAYLOAD_KB
        return DEFAULT_PAYLOAD_KB

    def do_GET(self):
        payload = make_payload(self._resolve_payload_kb())
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_POST(self):
        self.do_GET()

    def log_message(self, *args):
        pass  # suppress per-request log noise


if __name__ == "__main__":
    port = int(os.environ.get("PORT", "9000"))
    server = http.server.HTTPServer(("127.0.0.1", port), EchoHandler)
    print(
        f"echo server listening on 127.0.0.1:{port} "
        f"(default={DEFAULT_PAYLOAD_KB}KB, max={MAX_PAYLOAD_KB}KB)",
        flush=True,
    )
    server.serve_forever()
