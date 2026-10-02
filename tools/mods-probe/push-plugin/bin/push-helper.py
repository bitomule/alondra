#!/usr/bin/env python3
"""Connects to the probe daemon's socket, says who it is, then copies every line the daemon
pushes to stdout. Blocks in recv while nothing arrives; exits when the daemon goes away."""
import socket
import sys

path, session = sys.argv[1], sys.argv[2]
conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
conn.connect(path)
conn.sendall(f"HELLO {session}\n".encode())
print("connected", flush=True)
while True:
    data = conn.recv(65536)
    if not data:
        sys.exit(3)
    sys.stdout.write(data.decode())
    sys.stdout.flush()
