#!/usr/bin/env python3
"""The time each test target took and the limit it declares, from Bazel's build event files.

A result read from a cache keeps the duration of the run that produced it, so a run that reuses
everything still reports what every target costs. With several files the slowest attempt wins.
Usage: test_durations.py [--over SECONDS] FILE..."""
import argparse
import json
from pathlib import Path


def seconds(text):
    return float(text.rstrip("s")) if text else 0.0


def events(path):
    for line in Path(path).read_text(errors="replace").splitlines():
        try:
            yield json.loads(line)
        except json.JSONDecodeError:
            continue


def durations(paths):
    """Each target's slowest attempt and its timeout, slowest first."""
    took, limit = {}, {}
    for path in paths:
        for event in events(path):
            ident = event.get("id", {})
            if "testResult" in ident:
                label = ident["testResult"]["label"]
                attempt = seconds(event.get("testResult", {}).get("testAttemptDuration"))
                took[label] = max(took.get(label, 0.0), attempt)
            elif "targetCompleted" in ident and "testTimeout" in event.get("completed", {}):
                limit[ident["targetCompleted"]["label"]] = seconds(event["completed"]["testTimeout"])
    return sorted(((took[label], limit.get(label), label) for label in took), key=lambda row: (-row[0], row[2]))


def table(rows):
    lines = ["| Target | Seconds | Limit |", "|---|---:|---:|"]
    for took, limit, label in rows:
        lines.append(f"| `{label}` | {took:.1f} | {f'{limit:.0f}' if limit else '?'} |")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--over", type=float, default=0.0)
    parser.add_argument("files", nargs="+")
    args = parser.parse_args()
    print(table([row for row in durations(args.files) if row[0] >= args.over]))


if __name__ == "__main__":
    main()
