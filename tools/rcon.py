#!/usr/bin/env python3
"""Send commands to a local test server over RCON.

    python tools/rcon.py "tp Walker ~3 ~ ~" "rotate Walker 90 0"

Needs `enable-rcon=true`, `rcon.password=rapidtest`, `rcon.port=25598` in
the test server's server.properties.
"""

import socket
import struct
import sys


def packet(req_id: int, kind: int, body: str) -> bytes:
    data = struct.pack("<ii", req_id, kind) + body.encode() + b"\x00\x00"
    return struct.pack("<i", len(data)) + data


def read(sock: socket.socket) -> tuple[int, str]:
    (length,) = struct.unpack("<i", sock.recv(4))
    data = b""
    while len(data) < length:
        data += sock.recv(length - len(data))
    req_id, _kind = struct.unpack("<ii", data[:8])
    return req_id, data[8:-2].decode(errors="replace")


def main() -> None:
    with socket.create_connection(("127.0.0.1", 25598), timeout=5) as sock:
        sock.sendall(packet(1, 3, "rapidtest"))
        if read(sock)[0] == -1:
            sys.exit("rcon auth failed")
        for i, command in enumerate(sys.argv[1:], start=2):
            sock.sendall(packet(i, 2, command))
            print(f"> {command}\n{read(sock)[1]}")


if __name__ == "__main__":
    main()
