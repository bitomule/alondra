#!/usr/bin/env python3
"""Steps and cache-read tokens of Alondra nodes, read from their Claude Code transcripts.

A node runs in a worktree `<repo>/.claude/worktrees/t-…`, so its transcripts are the files
`~/.claude/projects/*-claude-worktrees-t-*/*.jsonl`, with its subagents' transcripts in
`<session>/subagents/*.jsonl`. A step is one model call (one assistant message id): every step
re-reads the whole context, which is what cache-read tokens count.

  tools/node_token_usage.py --since 2026-10-21T11:00 --until 2026-10-21T17:00
  tools/node_token_usage.py --since 2026-10-14 --split 2026-10-22T10:00 --repo alondra

`--split` compares the tasks that started before that moment with those that started after it.
"""

import argparse
import glob
import json
import os
import re
import statistics
import sys
from datetime import datetime, timezone

LOOKS = {"grep", "rg", "sed", "cat", "head", "tail", "ls", "git", "python3", "python", "find",
         "wc", "awk", "jq", "diff", "stat", "file", "test", "echo", "cd", "pwd", "nl", "tree"}
LOOK_TOOLS = {"Read", "Grep", "Glob", "LS"}
POLL_TOOLS = {"BashOutput", "TaskOutput"}
POLL = re.compile(r"\bsleep\s+\d|\buntil\b|\bwhile\b.*\bdo\b|\btail\s+-[a-zA-Z]*f|\bwatch\s")
SEPARATORS = re.compile(r"&&|\|\||;|\n")


def parse_time(text):
    moment = datetime.fromisoformat(text.replace("Z", "+00:00"))
    return moment if moment.tzinfo else moment.replace(tzinfo=timezone.utc)


def commands(command):
    parts = [part.strip() for part in SEPARATORS.split(command) if part.strip()]
    return [part for part in parts if program(part) != "cd"]


def program(command):
    for word in command.split():
        if "=" not in word:
            return os.path.basename(word)
    return ""


def looks(tool):
    if tool["name"] in LOOK_TOOLS:
        return 1
    if tool["name"] != "Bash":
        return 0
    parts = commands(tool.get("input", {}).get("command", ""))
    return len(parts) if parts and all(program(part.split("|")[0]) in LOOKS for part in parts) else 0


def kind(tools):
    if any(tool["name"] in POLL_TOOLS for tool in tools):
        return "poll"
    for tool in tools:
        if tool["name"] == "Bash" and POLL.search(tool.get("input", {}).get("command", "")):
            return "poll"
    if not tools:
        return "text"
    if len(tools) > 1:
        return "parallel"
    count = looks(tools[0])
    return "look" if count == 1 else "looks" if count > 1 else "other"


def steps(path):
    found = {}
    with open(path, encoding="utf-8") as lines:
        for line in lines:
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            message = record.get("message")
            if record.get("type") != "assistant" or not isinstance(message, dict):
                continue
            key = message.get("id") or record.get("uuid")
            step = found.setdefault(key, {"time": record.get("timestamp"), "usage": message.get("usage") or {},
                                          "tools": []})
            step["tools"] += [block for block in message.get("content") or []
                              if isinstance(block, dict) and block.get("type") == "tool_use"]
    for step in found.values():
        if step["time"] and step["usage"]:
            step["time"] = parse_time(step["time"])
            step["kind"] = kind(step["tools"])
            yield step


def task_of(directory):
    name = os.path.basename(directory.rstrip("/"))
    head, _, task = name.partition("-claude-worktrees-")
    return head.rstrip("-").split("-Projects-")[-1], task


def tasks(projects, since, until, repo):
    for directory in sorted(glob.glob(os.path.join(projects, "*-claude-worktrees-t-*"))):
        owner, task = task_of(directory)
        if repo and owner.lower() != repo.lower():
            continue
        main = [step for path in glob.glob(os.path.join(directory, "*.jsonl")) for step in steps(path)]
        helpers = [step for path in glob.glob(os.path.join(directory, "*", "subagents", "*.jsonl"))
                   for step in steps(path)]
        inside = lambda step: (not since or step["time"] >= since) and (not until or step["time"] < until)
        main = [step for step in main if inside(step)]
        helpers = [step for step in helpers if inside(step)]
        if main:
            yield summary(f"{owner}/{task}", main, helpers)


def usage(steps_, field):
    return sum(step["usage"].get(field, 0) or 0 for step in steps_)


def summary(name, main, helpers):
    count = len(main)
    kinds = {k: sum(step["kind"] == k for step in main) for k in ("look", "looks", "poll", "parallel")}
    return {
        "task": name,
        "start": min(step["time"] for step in main),
        "steps": count,
        "tool_calls": sum(len(step["tools"]) for step in main),
        "cache_read": usage(main, "cache_read_input_tokens"),
        "cache_write": usage(main, "cache_creation_input_tokens"),
        "output": usage(main, "output_tokens"),
        "mean_context": (usage(main, "cache_read_input_tokens") + usage(main, "cache_creation_input_tokens")
                         + usage(main, "input_tokens")) // count,
        "look_share": (kinds["look"] + kinds["looks"]) / count,
        "one_look_share": kinds["look"] / count,
        "poll_share": kinds["poll"] / count,
        "batched_share": (kinds["looks"] + kinds["parallel"]) / count,
        "subagent_steps": len(helpers),
        "subagent_cache_read": usage(helpers, "cache_read_input_tokens"),
        "subagent_output": usage(helpers, "output_tokens"),
    }


def cohort(rows):
    if not rows:
        return {"tasks": 0}
    median = lambda field: statistics.median(row[field] for row in rows)
    read = sum(row["cache_read"] + row["subagent_cache_read"] for row in rows)
    output = sum(row["output"] + row["subagent_output"] for row in rows)
    steps_ = sum(row["steps"] for row in rows)
    weighted = lambda field: sum(row[field] * row["steps"] for row in rows) / steps_
    return {
        "tasks": len(rows),
        "steps": steps_,
        "median_steps_per_task": median("steps"),
        "median_cache_read_per_task": median("cache_read"),
        "cache_read_with_subagents": read,
        "output_with_subagents": output,
        "cache_read_per_output_token": round(read / max(output, 1), 1),
        "cache_read_per_step": steps_ and sum(row["cache_read"] for row in rows) // steps_,
        "tool_calls_per_step": round(sum(row["tool_calls"] for row in rows) / steps_, 2),
        "look_step_share": round(weighted("look_share"), 3),
        "one_command_look_share": round(weighted("one_look_share"), 3),
        "poll_share": round(weighted("poll_share"), 3),
        "batched_share": round(weighted("batched_share"), 3),
        "subagent_cache_read_share": round(sum(row["subagent_cache_read"] for row in rows) / max(read, 1), 3),
    }


def millions(value):
    return f"{value / 1e6:,.1f} M"


def table(rows, limit):
    print(f"{'task':58} {'steps':>6} {'cache read':>11} {'output':>8} {'context':>8} {'looks':>6} "
          f"{'poll':>5} {'sub read':>9}")
    for row in sorted(rows, key=lambda row: -row["cache_read"])[:limit]:
        print(f"{row['task'][:58]:58} {row['steps']:>6} {millions(row['cache_read']):>11} "
              f"{row['output'] / 1e3:>7.0f}k {row['mean_context'] / 1e3:>7.0f}k {row['look_share']:>6.0%} "
              f"{row['poll_share']:>5.0%} {millions(row['subagent_cache_read']):>9}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--projects", default=os.path.expanduser("~/.claude/projects"))
    parser.add_argument("--since", type=parse_time)
    parser.add_argument("--until", type=parse_time)
    parser.add_argument("--split", type=parse_time, help="compare tasks started before and after this moment")
    parser.add_argument("--repo", help="only nodes of this repository folder, e.g. alondra")
    parser.add_argument("--min-steps", type=int, default=20, help="leave out tasks with fewer steps")
    parser.add_argument("--top", type=int, default=15)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    rows = [row for row in tasks(args.projects, args.since, args.until, args.repo)
            if row["steps"] >= args.min_steps]
    cohorts = ({"before": cohort([row for row in rows if row["start"] < args.split]),
                "after": cohort([row for row in rows if row["start"] >= args.split])}
               if args.split else {"all": cohort(rows)})
    if args.json:
        json.dump({"cohorts": cohorts, "tasks": rows}, sys.stdout, default=str, indent=2)
        print()
        return 0
    table(rows, args.top)
    for name, values in cohorts.items():
        print(f"\n{name}:")
        for field, value in values.items():
            print(f"  {field:30} {value:,}" if isinstance(value, int) else f"  {field:30} {value}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
