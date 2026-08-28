#!/usr/bin/env python3
"""Probe the framing the unix listener expects.

NDJSON works on stdio (measured, s01-s05) but the same frame over a
`--listen unix://` socket got no reply and produced no server log. Rather than
guess, try each plausible framing and report which one answers. A wrong guess
here is exactly the vendor-key failure mode one layer down: silence is
indistinguishable between "wrong framing" and "method refused".
"""

import json
import socket
import sys
import time

SOCK = sys.argv[1] if len(sys.argv) > 1 else "/private/tmp/codex-probe-sock/as.sock"
MSG = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                  "params": {"clientInfo": {"name": "probe", "version": "0.0.1"},
                             "capabilities": {"experimentalApi": True}}})


def attempt(label, payload):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.connect(SOCK)
    except OSError as e:
        print(f"{label}: connect failed {e}")
        return
    s.settimeout(6)
    s.sendall(payload)
    try:
        data = s.recv(8000)
        if data:
            print(f"{label}: REPLIED {data[:500]!r}")
        else:
            print(f"{label}: connection closed with no data")
    except socket.timeout:
        print(f"{label}: timeout, no reply")
    finally:
        s.close()


# The binary carries "codex app-server (WebSockets)" plus /control, /rpc,
# /readyz, /healthz path strings, so the listener wants an HTTP upgrade.
# Try each candidate path -- a 404 on the wrong path and a refused upgrade
# look identical from the client, which is why the path is measured.
def upgrade(path):
    req = (
        f"GET {path} HTTP/1.1\r\n"
        "Host: localhost\r\n"
        "Connection: Upgrade\r\n"
        "Upgrade: websocket\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
        "\r\n"
    ).encode()
    attempt(f"ws upgrade {path}", req)

for p_ in ("/", "/control", "/rpc", "/ws", "/readyz", "/healthz"):
    upgrade(p_)
