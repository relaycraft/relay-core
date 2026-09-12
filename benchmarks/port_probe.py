#!/usr/bin/env python3
"""Exit 0 when 127.0.0.1:<port> accepts a connection, 1 otherwise.

Used by bench_minimal.sh to detect stale or foreign listeners before a run. The readiness
probe alone cannot distinguish RelayCore's own proxy from any other process bound to the
same port, which previously let a dead proxy look healthy while something else answered.
"""

import socket
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: port_probe.py <port>", file=sys.stderr)
        return 2
    try:
        port = int(sys.argv[1])
    except ValueError:
        return 2

    sock = socket.socket()
    sock.settimeout(0.3)
    try:
        sock.connect(("127.0.0.1", port))
    except OSError:
        return 1
    finally:
        sock.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
