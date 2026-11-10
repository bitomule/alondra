#!/usr/bin/env python3
"""A `claude` without a model that plays the orchestrator in the pause-queued end-to-end check.

Run as the orchestrator, it records each prompt in $E2E_ORCHESTRATOR_LOG; asked to queue three
tasks for the node's request, it creates them with their launch plans through the real
`alondra mcp-orchestrator` and answers the node. Anything else exits at once, so a node Alondra
tries to start with this `claude` never comes up.
"""

import json
import os
import re
import subprocess
import sys

TASKS = [("queued-one", "Rebuild the release binaries from Bazel"), ("queued-two", "Redraft index UX"),
         ("queued-three", "Lumo App Store release")]


def call(tool, arguments):
    requests = [{"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}},
                {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": arguments}}]
    run = subprocess.run([os.environ["E2E_ALONDRA"], "mcp-orchestrator"], input="\n".join(map(json.dumps, requests)) + "\n",
                         capture_output=True, text=True, timeout=60)
    reply = [json.loads(line) for line in run.stdout.splitlines() if line.strip()][-1]
    if reply.get("result", {}).get("isError") or "error" in reply:
        with open(os.environ["E2E_ORCHESTRATOR_LOG"] + ".errors", "a") as file:
            file.write(f"{tool} was refused: {reply}\n")
        raise SystemExit(1)


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
    print(json.dumps({"type": "system", "subtype": "init", "session_id": session, "permissionMode": "auto", "model": "opus",
                      "mcp_servers": [{"name": "alondra-orchestrator", "status": "connected"}],
                      "tools": ["Skill", "Read"] + [f"mcp__alondra-orchestrator__{tool}" for tool in
                                                    ("title_request", "create_task", "answer_question", "settle_plan_review", "ask")]}), flush=True)
    queue = re.search(r"Queue three tasks after (sandbox-probe-[0-9a-f-]+)", prompt)
    if queue and not os.path.exists(log + ".created"):
        source = queue.group(1)
        call("title_request", {"request_id": source, "title": "Queued work"})
        for task, title in TASKS:
            call("create_task", {"request_id": source, "task_id": task, "title": title, "review_prs": False, "dependencies": [],
                                 "launch": {"name": task, "cwd": os.environ["E2E_PROJECT"], "mission": title, "model": "sonnet",
                                            "reason": "A short, clear task"}})
        call("answer_question", {"interaction_id": "queue-tasks", "option_id": "done", "inputs": {}})
        open(log + ".created", "w").close()
    print(json.dumps({"type": "result", "session_id": session, "subtype": "success", "is_error": False, "result": "Done.",
                      "modelUsage": {"opus": {"contextWindow": 1000000}}}), flush=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except BaseException as error:
        with open(os.environ.get("E2E_ORCHESTRATOR_LOG", "/dev/null") + ".errors", "a") as file:
            file.write(f"{error!r}\n")
        raise
