#!/usr/bin/env python3
"""A `claude` without a model for the card end-to-end checks, put first on the sandbox daemon's PATH.

Run as the orchestrator, it records the prompt Alondra handed it in $E2E_ORCHESTRATOR_LOG and, the
first time, asks the user the Decide card in $E2E_ORCHESTRATOR_CARD with the real
`alondra mcp-orchestrator` and the capability Alondra gave it. Anything else exits at once.
"""

import json
import os
import subprocess
import sys


def main():
    args = sys.argv[1:]
    if "--version" in args:
        print("2.1.321 (Claude Code)")
        return 0
    if os.environ.get("ALONDRA_ORCHESTRATOR") != "1":
        return 0
    session = next(args[index + 1] for index, arg in enumerate(args) if arg in ("--session-id", "--resume"))
    prompt = sys.stdin.read()
    log = os.environ["E2E_ORCHESTRATOR_LOG"]
    with open(log, "a") as file:
        file.write(json.dumps({"prompt": prompt}) + "\n")
    prefix = "mcp__alondra-orchestrator"
    print(json.dumps({"type": "system", "subtype": "init", "session_id": session, "permissionMode": "auto", "model": "opus",
                      "mcp_servers": [{"name": "alondra-orchestrator", "status": "connected"}],
                      "tools": ["Skill", "Read", f"{prefix}__create_task", f"{prefix}__ask"]}), flush=True)
    asked = log + ".asked"
    if not os.path.exists(asked):
        card = json.loads(os.environ["E2E_ORCHESTRATOR_CARD"])
        requests = [{"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}},
                    {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "ask", "arguments": card}}]
        run = subprocess.run([os.environ["E2E_ALONDRA"], "mcp-orchestrator"], input="\n".join(map(json.dumps, requests)) + "\n",
                             capture_output=True, text=True, timeout=60)
        reply = [json.loads(line) for line in run.stdout.splitlines() if line.strip()][-1]
        if reply.get("result", {}).get("isError") or "error" in reply:
            print(f"ask was refused: {reply}", file=sys.stderr)
            return 1
        open(asked, "w").close()
    print(json.dumps({"type": "result", "session_id": session, "subtype": "success", "is_error": False, "result": "Done.",
                      "modelUsage": {"opus": {"contextWindow": 1000000}}}), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
