#!/usr/bin/env python3
import json
import os
import sys

event = json.load(sys.stdin)
tool_input = event.get("tool_input", {})
prompt = tool_input.get("prompt", "")
log = os.environ.get("MODS_PROBE_LOG")
if log:
    with open(log, "a") as f:
        f.write(json.dumps({"src": "settings-hook", "ev": "PreToolUse", "patched": "HOOKPATCH" in prompt}) + "\n")
if "HOOKPATCH" in prompt:
    patched = dict(tool_input, prompt="HOOKCLAIM-42\n" + prompt)
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": patched}}))
