#!/usr/bin/env python3
"""Tells the probe daemon that message ids were handed to the session."""
import socket
import sys

path, session, ids = sys.argv[1], sys.argv[2], sys.argv[3]
conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
conn.settimeout(5)
conn.connect(path)
conn.sendall(f"ACK {session} {ids}\n".encode())
conn.close()
