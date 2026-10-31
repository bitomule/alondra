#!/usr/bin/python3
"""Loopback-only Responses model double for the opt-in native owner/store proof."""
import gzip
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

trace = sys.argv[1]
sequence = 0
journey_step = 0
journey = None
if len(sys.argv) > 2:
    with open(sys.argv[2], encoding="utf-8") as source: journey = json.load(source)


def call_item(request, name, arguments, number):
    selected = None
    namespace = None
    code_exec = None
    advertised = list(request.get("tools", []))
    for message in request.get("input", []):
        if isinstance(message, dict) and message.get("type") == "additional_tools":
            advertised.extend(message.get("tools", []))
    for candidate in advertised:
        children = candidate.get("tools", []) if candidate.get("type") == "namespace" else [candidate]
        for child in children:
            child_namespace = candidate["name"] if candidate.get("type") == "namespace" else None
            if child.get("name") == name or child.get("name", "").endswith("__" + name):
                selected, namespace = child, child_namespace
            if child.get("type") == "custom" and child.get("name") == "exec" and child_namespace == "functions":
                code_exec = child
    if selected is None and code_exec is not None:
        program = (
            "const target = " + json.dumps(name) + ";\n"
            "const matches = ALL_TOOLS.filter(tool => tool.name.toLowerCase().includes('alondra') && "
            "(tool.name.endsWith('__' + target) || tool.name.endsWith('_' + target) || tool.name.endsWith('.' + target)));\n"
            "if (matches.length !== 1) throw new Error('Expected exactly one real Alondra MCP tool for ' + target + '; found ' + matches.length);\n"
            "text(await tools[matches[0].name](" + json.dumps(arguments) + "));"
        )
        return {"type":"custom_tool_call","id":f"native-tool-{number}","call_id":f"native-call-{number}","name":code_exec["name"],"namespace":"functions","input":program,"status":"completed"}
    if selected is None:
        raise ValueError("Required real Alondra MCP tool or its code-mode dispatcher was not advertised: " + name)
    item = {"type":"function_call","id":f"native-tool-{number}","call_id":f"native-call-{number}","name":selected["name"],"arguments":json.dumps(arguments),"status":"completed"}
    if namespace is not None: item["namespace"] = namespace
    return item


def user_input_contains(request, expected):
    return any(
        expected in part.get("text", "")
        for message in request.get("input", [])
        if isinstance(message, dict) and message.get("role") == "user"
        for part in message.get("content", [])
        if isinstance(part, dict) and part.get("type") == "input_text"
    )


def native_plan_instructions(request):
    return any(
        "<collaboration_mode>" in part.get("text", "")
        and "# Plan Mode (Conversational)" in part.get("text", "")
        and "You are in **Plan Mode** until a developer message explicitly ends it." in part.get("text", "")
        for message in request.get("input", [])
        if isinstance(message, dict) and message.get("role") == "developer"
        for part in message.get("content", [])
        if isinstance(part, dict) and part.get("type") == "input_text"
    )


def journey_item(request, number):
    global journey_step
    task = journey["task"]
    step = journey_step + 1
    advance = True
    if step == 3 and not user_input_contains(request, journey["expected_answer"]):
        step, advance = 2, False
    if step == 5 and not native_plan_instructions(request):
        raise ValueError("The actual native planning request does not carry plan collaboration instructions")
    if step == 6 and not user_input_contains(request, "Implement the plan the user just approved."):
        raise ValueError("The actual native user input does not authorize implementing the approved plan")
    if advance:
        journey_step = step
    with open(trace + ".stages", "a", encoding="utf-8") as stages:
        stages.write(json.dumps({"request":number,"step":step,"advanced":advance}) + "\n")
    if step == 1:
        return call_item(request, "ask", {"interaction_id":"native-language-decision","scope":{"kind":"task","task_id":task},"recipient":{"kind":"user"},"title":"Report language","need":"Which language should the report use?","why":"The report will be written in the language you choose.","blocking":True,"options":[{"id":"english","label":"English","description":"Write the report in English."},{"id":"spanish","label":"Spanish","description":"Write the report in Spanish."}],"recommended":"english"},number)
    if step == 3:
        return call_item(request,"request_plan_mode",{"task_id":task},number)
    if step == 6:
        return call_item(request,"present_result",{"task_id":task,"interaction_id":"native-final-report","title":"Completed report","need":"Review the completed report","targets":[{"kind":"text","text":"The selected-language report is complete."}],"is_final":True,"content":{"kind":"report","body":"The report is written in English, following the language chosen on the decision card. Its approved plan was completed and the report reached this review card through the native Codex MCP integration."},"checklist":[{"item":str(item),"evidence":[{"kind":"test","value":"Native Codex MCP callback journey","detail":"The real native CLI, Alondra role CLI and authenticated daemon share one task owner and store."}]} for item in [1,2]]},number)
    text = {2:"Waiting for your language decision.",4:"The next turn will prepare the plan.",5:"<proposed_plan>\n# Report plan\n- [ ] Write the report in the selected language.\n- [ ] Verify that the report reaches the Alondra review card.\n</proposed_plan>",7:"The report is available on its Alondra review card."}.get(step,"No additional work.")
    return {"type":"message","id":f"native-owner-item-{number}","role":"assistant","status":"completed","phase":"final_answer","content":[{"type":"output_text","text":text,"annotations":[]}]}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        if "accounts/check" in self.path:
            value={"accounts":[{"id":"fixture-workspace","plan_type":"plus","workspace_backend_origin":"https://chatgpt.com","account_routing_override":"NO_CONSTRAINT"}],"account_ordering":["fixture-workspace"],"default_account_id":"fixture-workspace"}
            status=200
        else:
            value={"error":"No remote catalog in this bounded fixture"}
            status=404
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        global sequence
        size = int(self.headers.get("Content-Length", "0"))
        if size < 1 or size > 4 * 1024 * 1024:
            self.send_error(413)
            return
        body = self.rfile.read(size)
        if self.path.split("?", 1)[0] != "/v1/responses":
            self.send_response(204)
            self.end_headers()
            return
        if self.headers.get("Content-Encoding") == "gzip":
            body = gzip.decompress(body)
        request = json.loads(body)
        with open(trace, "a", encoding="utf-8") as target:
            target.write(json.dumps(request) + "\n")
        sequence += 1
        response_id = f"native-owner-response-{sequence}"
        item = {"type": "message", "id": f"native-owner-item-{sequence}", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": "Native Codex owner/store proof completed.", "annotations": []}]}
        if journey is not None:
            try:
                item = journey_item(request, sequence)
            except Exception as error:
                with open(trace + ".error.json", "w", encoding="utf-8") as diagnostic:
                    json.dump({"request": sequence, "completed_journey_step": journey_step, "error_type": type(error).__name__, "message": str(error)[:2000]}, diagnostic)
                self.send_error(400, "The loopback journey fixture rejected this request; inspect its error artifact")
                return
        response = {"id": response_id, "object": "response", "status": "completed", "output": [item], "usage": {"input_tokens": 16, "output_tokens": 8, "total_tokens": 24, "input_tokens_details": {"cached_tokens": 0}, "output_tokens_details": {"reasoning_tokens": 0}}}
        added = dict(item, status="in_progress")
        if item["type"] == "message":
            added["content"] = []
        elif item["type"] == "function_call":
            added["arguments"] = ""
        elif item["type"] == "custom_tool_call":
            added["input"] = ""
        events = [
            ("response.created", {"response": {"id": response_id, "object": "response", "status": "in_progress", "output": []}}),
            ("response.output_item.added", {"output_index": 0, "item": added}),
        ]
        if item["type"] == "message":
            for index, part in enumerate(item["content"]):
                identity = {"item_id": item["id"], "output_index": 0, "content_index": index}
                events.extend([
                    ("response.content_part.added", {**identity, "part": {**part, "text": ""}}),
                    ("response.output_text.delta", {**identity, "delta": part["text"]}),
                    ("response.output_text.done", {**identity, "text": part["text"]}),
                    ("response.content_part.done", {**identity, "part": part}),
                ])
        elif item["type"] == "function_call":
            identity = {"item_id": item["id"], "output_index": 0}
            events.extend([
                ("response.function_call_arguments.delta", {**identity, "delta": item["arguments"]}),
                ("response.function_call_arguments.done", {**identity, "arguments": item["arguments"]}),
            ])
        elif item["type"] == "custom_tool_call":
            identity = {"item_id": item["id"], "output_index": 0}
            events.extend([
                ("response.custom_tool_call_input.delta", {**identity, "delta": item["input"]}),
                ("response.custom_tool_call_input.done", {**identity, "input": item["input"]}),
            ])
        events.extend([
            ("response.output_item.done", {"output_index": 0, "item": item}),
            ("response.completed", {"response": response}),
        ])
        encoded = "".join(f"event: {kind}\ndata: {json.dumps(dict(type=kind, **payload))}\n\n" for kind, payload in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(encoded)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(encoded)


server = HTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
