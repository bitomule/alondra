"""Offline protocol fixture for the standalone SEA: no JavaScript runtime."""
import json
import os
import sys


def send(value):
    print(json.dumps(value), flush=True)


for line in sys.stdin:
    message = json.loads(line)
    if message["type"] == "control_request":
        if message["request"]["subtype"] != "initialize":
            raise RuntimeError("Unsupported fixture control")
        with open(os.environ["FIXTURE_CAPTURE"], "w", encoding="utf-8") as capture:
            json.dump({"args": sys.argv[1:], "home": os.environ["HOME"], "initialize": message["request"]}, capture)
        send({"type": "control_response", "response": {"subtype": "success", "request_id": message["request_id"], "response": {"commands": [], "models": [], "account": {}, "output_style": "default"}}})
        send({"type": "system", "subtype": "init", "session_id": "sea-fixture", "uuid": "fixture-init", "claude_code_version": "2.1.300-fixture", "model": "fixture", "cwd": os.getcwd(), "tools": [], "mcp_servers": [], "permissionMode": "default", "slash_commands": [], "output_style": "default", "skills": [], "plugins": [], "apiKeySource": "none"})
    elif message["type"] == "user":
        send({"type": "result", "subtype": "success", "is_error": False, "result": "offline standalone SEA", "session_id": "sea-fixture", "uuid": "fixture-result", "user_message_uuid": message["uuid"], "duration_ms": 0, "duration_api_ms": 0, "num_turns": 1, "total_cost_usd": 0, "usage": {}, "modelUsage": {}, "permission_denials": []})
    else:
        raise RuntimeError("Unsupported fixture frame")
