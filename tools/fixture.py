#!/usr/bin/env python3
"""Cut one node's recorded events out of an alondra.db into a reducer fixture.

Usage: tools/fixture.py <copy-of-alondra.db> <session-id> [--from SEQ] [--to SEQ] > fixture.jsonl

Work on a copy (sqlite3 -readonly ~/.alondra/alondra.db ".backup /tmp/x.db"), never on the live file.
Every free text is replaced by a placeholder: equal texts keep equal placeholders, so the
reducer and the sentinel still see when a text changes or repeats, but nothing of what was said
survives. Only the nudge prefix is kept, because the sentinel recognises its nudges by it.
"""

import argparse
import json
import sqlite3
import sys

NUDGE_PREFIX = "[Alondra] Your turn ended without telling"
USAGE_LIMIT_ERRORS = {None, "rate_limit", "overloaded", "billing_error"}


class Redactor:
    def __init__(self):
        self.seen = {}

    def text(self, value):
        if value is None:
            return None
        if value.startswith(NUDGE_PREFIX):
            return NUDGE_PREFIX + " [nudge]"
        if value.strip() == "":
            return value
        if value not in self.seen:
            self.seen[value] = f"text-{len(self.seen) + 1}"
        return self.seen[value]


def redact_answer(answer, r):
    answer["text"] = r.text(answer["text"])
    answer["option"] = r.text(answer.get("option"))
    answer["by"] = r.text(answer.get("by"))
    answer["device"] = r.text(answer.get("device"))


def redact(event, r):
    kind = event["type"]
    if kind == "hook":
        for key in ("tool_summary", "last_message"):
            event[key] = r.text(event.get(key))
        if event.get("error_type") not in USAGE_LIMIT_ERRORS:
            event["error_type"] = r.text(event.get("error_type"))
        event["cwd"] = None
    elif kind == "daemon_seen":
        for key in ("detail", "needs", "last_text"):
            event[key] = r.text(event.get(key))
        event["cwd"] = None
    elif kind == "question_asked":
        event["title"] = r.text(event["title"])
        event["detail"] = r.text(event.get("detail"))
        event["node_context"] = r.text(event.get("node_context"))
        event["options"] = [{"label": r.text(o["label"]), "description": None} for o in event.get("options", [])]
        event["links"] = []
        event["attachments"] = []
        if event.get("answer"):
            redact_answer(event["answer"], r)
    elif kind == "question_answered":
        redact_answer(event["answer"], r)
    elif kind == "report_posted":
        event["title"] = r.text(event["title"])
        event["body"] = r.text(event.get("body"))
        event["links"] = []
        event["attachments"] = []
    elif kind == "message_queued":
        event["text"] = r.text(event["text"])
    elif kind == "waiting_machine":
        event["reason"] = r.text(event["reason"])
    return event


def main():
    p = argparse.ArgumentParser()
    p.add_argument("db")
    p.add_argument("session")
    p.add_argument("--from", dest="start", type=int, default=0)
    p.add_argument("--to", dest="end", type=int, default=1 << 62)
    a = p.parse_args()

    db = sqlite3.connect(f"file:{a.db}?mode=ro", uri=True)
    rows = db.execute(
        "SELECT seq, at, kind, session, body FROM events WHERE seq BETWEEN ? AND ? ORDER BY seq", (a.start, a.end)
    ).fetchall()
    own = {"question": set(), "report": set(), "message": set()}
    r = Redactor()
    out = []
    for seq, at, kind, session, body in rows:
        event = json.loads(body)
        if session == a.session:
            if kind == "question_asked":
                own["question"].add(event["id"])
            elif kind == "report_posted":
                own["report"].add(event["id"])
            elif kind == "message_queued":
                own["message"].add(event["id"])
        elif session is None:
            table = {
                "question_answered": "question",
                "question_delivered": "question",
                "question_closed": "question",
                "report_read": "report",
                "message_handed": "message",
            }.get(kind)
            if table is None or event.get("id") not in own[table]:
                continue
        else:
            continue
        out.append({"seq": seq, "at": at, "event": redact(event, r)})
    for line in out:
        sys.stdout.write(json.dumps(line, ensure_ascii=False, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
