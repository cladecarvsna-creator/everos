#!/usr/bin/env python3
"""A small proxy server for testing EverOS's proxy support.

It speaks HTTP (CONNECT tunnels and plain requests with whole URLs) and
SOCKS5 on the same port, telling them apart by the first byte, and
prints one line per connection. With --user and --password it asks for
that login (Proxy-Authorization for HTTP, RFC 1929 for SOCKS5).

    python3 scripts/test-proxy.py 8124
    python3 scripts/test-proxy.py 8124 --user jack --password secret

From QEMU's user network the guest reaches it at 10.0.2.2:8124.
"""

import argparse
import base64
import select
import socket
import struct
import sys
import threading


def log(text):
    print(text, flush=True)


def pipe(a, b):
    """Copy bytes both ways until one side closes."""
    socks = [a, b]
    try:
        while True:
            ready, _, _ = select.select(socks, [], [], 60)
            if not ready:
                return
            for s in ready:
                data = s.recv(65536)
                if not data:
                    return
                (b if s is a else a).sendall(data)
    except OSError:
        pass


def read_head(conn, first):
    data = first
    while b"\r\n\r\n" not in data:
        more = conn.recv(4096)
        if not more:
            break
        data += more
    head, _, rest = data.partition(b"\r\n\r\n")
    return head.decode("latin-1"), rest


def http(conn, first, login):
    head, rest = read_head(conn, first)
    lines = head.split("\r\n")
    method, target, version = lines[0].split(" ", 2)
    headers = {}
    for line in lines[1:]:
        k, _, v = line.partition(":")
        headers[k.strip().lower()] = v.strip()
    if login:
        want = "Basic " + base64.b64encode(f"{login[0]}:{login[1]}".encode()).decode()
        if headers.get("proxy-authorization") != want:
            log(f"http: {method} {target} -> 407")
            conn.sendall(b"HTTP/1.1 407 Proxy Authentication Required\r\n"
                         b"Proxy-Authenticate: Basic realm=\"test\"\r\nContent-Length: 0\r\n\r\n")
            return
    if method == "CONNECT":
        host, _, port = target.rpartition(":")
        up = socket.create_connection((host, int(port)), timeout=10)
        log(f"http: CONNECT {target}")
        conn.sendall(b"HTTP/1.1 200 Connection established\r\n\r\n")
        if rest:
            up.sendall(rest)
        pipe(conn, up)
        return
    # a plain request with a whole URL: send it on with just the path
    if not target.startswith("http://"):
        conn.sendall(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
        return
    hostport, _, path = target[len("http://"):].partition("/")
    host, _, port = hostport.partition(":")
    log(f"http: {method} {target}")
    up = socket.create_connection((host, int(port or 80)), timeout=10)
    out = [f"{method} /{path} {version}"]
    for line in lines[1:]:
        k = line.partition(":")[0].strip().lower()
        if k not in ("proxy-authorization", "proxy-connection", "connection"):
            out.append(line)
    out.append("Connection: close")
    up.sendall(("\r\n".join(out) + "\r\n\r\n").encode("latin-1") + rest)
    pipe(conn, up)


def recv_exact(conn, n):
    data = b""
    while len(data) < n:
        more = conn.recv(n - len(data))
        if not more:
            raise OSError("closed")
        data += more
    return data


def socks5(conn, first, login):
    n = recv_exact(conn, 1)[0]
    methods = recv_exact(conn, n)
    if login:
        if 2 not in methods:
            conn.sendall(b"\x05\xff")
            log("socks5: client offered no login")
            return
        conn.sendall(b"\x05\x02")
        recv_exact(conn, 1)
        user = recv_exact(conn, recv_exact(conn, 1)[0]).decode()
        password = recv_exact(conn, recv_exact(conn, 1)[0]).decode()
        if (user, password) != login:
            conn.sendall(b"\x01\x01")
            log("socks5: wrong login")
            return
        conn.sendall(b"\x01\x00")
    else:
        conn.sendall(b"\x05\x00")
    ver, cmd, _, kind = recv_exact(conn, 4)
    if kind == 1:
        host = socket.inet_ntoa(recv_exact(conn, 4))
    elif kind == 3:
        host = recv_exact(conn, recv_exact(conn, 1)[0]).decode()
    else:
        conn.sendall(b"\x05\x08\x00\x01" + bytes(6))
        return
    port = struct.unpack(">H", recv_exact(conn, 2))[0]
    try:
        up = socket.create_connection((host, port), timeout=10)
    except OSError:
        log(f"socks5: CONNECT {host}:{port} refused")
        conn.sendall(b"\x05\x05\x00\x01" + bytes(6))
        return
    log(f"socks5: CONNECT {host}:{port}")
    conn.sendall(b"\x05\x00\x00\x01" + bytes(6))
    pipe(conn, up)


def serve(conn, login):
    try:
        first = conn.recv(1)
        if first == b"\x05":
            socks5(conn, first, login)
        elif first:
            http(conn, first, login)
    except Exception as e:  # keep serving others
        log(f"error: {e}")
    finally:
        conn.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("port", type=int)
    ap.add_argument("--user")
    ap.add_argument("--password", default="")
    args = ap.parse_args()
    login = (args.user, args.password) if args.user else None
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", args.port))
    srv.listen(16)
    log(f"proxy listening on {args.port}")
    while True:
        conn, _ = srv.accept()
        threading.Thread(target=serve, args=(conn, login), daemon=True).start()


if __name__ == "__main__":
    sys.exit(main())
