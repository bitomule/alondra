#!/usr/bin/env python3
"""Stand-in for alondrad in the push probe.

Serves a Unix socket. A node's helper connects with `HELLO <session>` and stays connected;
every line typed here is pushed to it at once as {"id": n, "text": line}. The mod acks on a
second short connection with `ACK <session> <ids>`. Everything is printed with a timestamp."""
import json
import os
import socket
import sys
import threading
import time

path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/alondra-push-probe.sock"
nodes = {}
lock = threading.Lock()
next_id = 1


def say(text):
    print(time.strftime("%H:%M:%S"), text, flush=True)


def serve(conn):
    first = b""
    while not first.endswith(b"\n"):
        chunk = conn.recv(1024)
        if not chunk:
            conn.close()
            return
        first += chunk
    words = first.decode().split()
    if words[0] == "ACK":
        say(f"ACK from {words[1][:8]}: {words[2]}")
        conn.close()
        return
    session = words[1]
    with lock:
        nodes[session] = conn
    say(f"node {session[:8]} connected ({len(nodes)} connected)")
    while conn.recv(1024):
        pass
    with lock:
        if nodes.get(session) is conn:
            del nodes[session]
    say(f"node {session[:8]} disconnected")


def accept(server):
    while True:
        conn, _ = server.accept()
        threading.Thread(target=serve, args=(conn,), daemon=True).start()


if os.path.exists(path):
    os.unlink(path)
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(path)
server.listen()
threading.Thread(target=accept, args=(server,), daemon=True).start()
say(f"listening on {path}; type a message and Enter to push it to every connected node")
for line in sys.stdin:
    text = line.strip()
    if not text:
        continue
    with lock:
        targets = list(nodes.items())
        message = json.dumps({"id": next_id, "text": text}) + "\n"
        next_id += 1
    for session, conn in targets:
        conn.sendall(message.encode())
        say(f"pushed to {session[:8]}: {message.strip()}")
    if not targets:
        say("no node connected; nothing pushed")
