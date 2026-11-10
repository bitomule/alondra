#!/usr/bin/env python3
"""A `claude` without a model that plays the orchestrator in the plan-review end-to-end check.

Run as the orchestrator, it records each prompt in $E2E_ORCHESTRATOR_LOG and acts with the real
`alondra mcp-orchestrator` and the capability Alondra gave it:
  - asked to create the phases after the node's task, it creates phases C and D waiting on it and
    answers the node;
  - told a decision holds them, it settles C with a new plan and D unchanged, but only once
    $E2E_ORCHESTRATOR_LOG.settle exists, so the check sees them held first.
Anything else exits at once.
"""

import json
import os
import re
import subprocess
import sys


def trace(text):
    with open(os.environ["E2E_ORCHESTRATOR_LOG"] + ".errors", "a") as file:
        file.write(text + "\n")


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


def plan(name, mission):
    return {"name": name, "cwd": os.environ["E2E_PROJECT"], "mission": mission, "model": "sonnet", "reason": "A short, clear phase"}


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
    phases = re.search(r"Create the phases after (sandbox-probe-[0-9a-f-]+)", prompt)
    if phases and not os.path.exists(log + ".created"):
        source = phases.group(1)
        call("title_request", {"request_id": source, "title": "Redraft index"})
        call("create_task", {"request_id": source, "task_id": "phase-c", "title": "Redraft index · Phase C", "review_prs": False, "dependencies": [source],
                             "launch": plan("phase-c", "Group with prints of the 360 px copy")})
        call("create_task", {"request_id": source, "task_id": "phase-d", "title": "Redraft index · Phase D", "review_prs": False, "dependencies": ["phase-c"],
                             "launch": plan("phase-d", "Use the index everywhere")})
        call("answer_question", {"interaction_id": "create-phases", "option_id": "done", "inputs": {}})
        open(log + ".created", "w").close()
    review = re.search(r'settle_plan_review\(review_id: "([^"]+)"', prompt)
    if review and os.path.exists(log + ".settle") and not os.path.exists(log + ".settled"):
        call("settle_plan_review", {"task_id": "phase-c", "review_id": review.group(1), "outcome": {
            "kind": "plan_changed", "launch": plan("phase-c", "Group with 512 px prints"), "reason": "The user chose 512 px prints"}})
        call("settle_plan_review", {"task_id": "phase-d", "review_id": review.group(1), "outcome": {
            "kind": "unchanged", "reason": "Phase D does not depend on the print size"}})
        open(log + ".settled", "w").close()
    print(json.dumps({"type": "result", "session_id": session, "subtype": "success", "is_error": False, "result": "Done.",
                      "modelUsage": {"opus": {"contextWindow": 1000000}}}), flush=True)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        import traceback
        with open(os.environ.get("E2E_ORCHESTRATOR_LOG", "/dev/null") + ".errors", "a") as file:
            file.write(traceback.format_exc())
        raise
