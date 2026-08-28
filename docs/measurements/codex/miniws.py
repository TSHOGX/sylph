#!/usr/bin/env python3
"""A minimal RFC6455 client, because the vendor's parser is stricter than a library.

Measured on codex-cli 0.150.1: a hand-written upgrade with exactly
Host/Connection/Upgrade/Sec-WebSocket-Version/Sec-WebSocket-Key is answered
`101 Switching Protocols` on any path, while the `websockets` package's own
handshake -- which adds `Sec-WebSocket-Extensions: permessage-deflate` and a
`User-Agent` -- gets the connection closed with no HTTP response at all. The
server's log line is `httparse error: invalid token`.

That asymmetry is worth recording rather than working around silently: an
adapter built on a mainstream websocket library may fail to connect at all,
and the failure presents as "no valid HTTP response" rather than as a rejected
header. Sylph's envagent will hit this, so it is a real constraint on the
implementation, not a quirk of this probe.

Only text frames and close are handled; that is the whole protocol surface the
app-server uses.
"""

import base64
import json
import os
import socket
import struct
import threading


class WSError(Exception):
    pass


class UnixWebSocket:
    """Client-side websocket over an AF_UNIX stream."""

    def __init__(self, path, resource="/"):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        key = base64.b64encode(os.urandom(16)).decode()
        req = (
            f"GET {resource} HTTP/1.1\r\n"
            "Host: localhost\r\n"
            "Connection: Upgrade\r\n"
            "Upgrade: websocket\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "\r\n"
        ).encode()
        self.sock.sendall(req)
        # Read exactly the response head.
        buf = b""
        while b"\r\n\r\n" not in buf:
            chunk = self.sock.recv(1)
            if not chunk:
                raise WSError(f"closed during handshake; got {buf!r}")
            buf += chunk
        if b"101" not in buf.split(b"\r\n")[0]:
            raise WSError(f"upgrade refused: {buf!r}")
        self.buf = b""
        self.lock = threading.Lock()

    def _recv_exact(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise WSError("closed")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def recv(self):
        """Return the next text message, reassembling continuation frames."""
        parts = []
        while True:
            h = self._recv_exact(2)
            fin = h[0] & 0x80
            opcode = h[0] & 0x0F
            masked = h[1] & 0x80
            ln = h[1] & 0x7F
            if ln == 126:
                ln = struct.unpack("!H", self._recv_exact(2))[0]
            elif ln == 127:
                ln = struct.unpack("!Q", self._recv_exact(8))[0]
            if masked:
                mask = self._recv_exact(4)
                payload = bytes(b ^ mask[i % 4]
                                for i, b in enumerate(self._recv_exact(ln)))
            else:
                payload = self._recv_exact(ln)
            if opcode == 0x8:
                raise WSError("server closed")
            if opcode == 0x9:  # ping -> pong
                self._send_frame(0xA, payload)
                continue
            if opcode == 0xA:
                continue
            parts.append(payload)
            if fin:
                return b"".join(parts).decode("utf-8", "replace")

    def _send_frame(self, opcode, payload):
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        n = len(payload)
        if n < 126:
            head = struct.pack("!BB", 0x80 | opcode, 0x80 | n)
        elif n < (1 << 16):
            head = struct.pack("!BBH", 0x80 | opcode, 0x80 | 126, n)
        else:
            head = struct.pack("!BBQ", 0x80 | opcode, 0x80 | 127, n)
        with self.lock:
            self.sock.sendall(head + mask + masked)

    def send_text(self, s):
        self._send_frame(0x1, s.encode())

    def close(self):
        try:
            self._send_frame(0x8, b"")
        except Exception:
            pass
        self.sock.close()
