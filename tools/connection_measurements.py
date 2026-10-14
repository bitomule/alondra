#!/usr/bin/env python3
"""Summarizes the connection measurements the iPhone or Mac app keeps (Settings > Connection
measurements > Share): per path, network and radio, how many requests, the median and p90 of the
answered ones, and how many failed. The live-updates wait (`changes`) is summarized apart."""
import argparse
from collections import defaultdict
from datetime import datetime, timezone
import json
import sys


def parse_time(value):
    moment = datetime.fromisoformat(value.replace("Z", "+00:00"))
    return moment if moment.tzinfo else moment.replace(tzinfo=timezone.utc)


def load(paths, since=None, until=None):
    rows = []
    for path in paths:
        with open(path, encoding="utf-8") as lines:
            for line in lines:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    continue
                at = parse_time(row["at"])
                if (since and at < since) or (until and at >= until):
                    continue
                rows.append(row)
    return rows


def percentile(values, percent):
    if not values:
        return None
    rank = (percent * len(values) + 99) // 100
    return values[max(rank, 1) - 1]


def summarize(rows):
    groups = defaultdict(list)
    for row in rows:
        if row.get("outcome") == "cancelled":
            continue
        key = (row.get("kind") == "changes", row.get("route") or "none", row.get("network", "other"), row.get("radio") or "")
        groups[key].append(row)
    summary = []
    for (waits, route, network, radio), group in groups.items():
        answered = sorted(row["ms"] for row in group if row["outcome"] in ("ok", "mac_error"))
        summary.append({
            "waits": waits, "route": route, "network": network, "radio": radio,
            "count": len(group), "failed": len(group) - len(answered),
            "median_ms": percentile(answered, 50), "p90_ms": percentile(answered, 90),
        })
    return sorted(summary, key=lambda g: (g["waits"], -g["count"], g["route"], g["network"], g["radio"]))


def table(summary):
    header = ["", "route", "network", "radio", "requests", "failed", "median ms", "p90 ms"]
    lines = [[("wait" if g["waits"] else ""), g["route"], g["network"], g["radio"], str(g["count"]), str(g["failed"]),
              "-" if g["median_ms"] is None else str(g["median_ms"]), "-" if g["p90_ms"] is None else str(g["p90_ms"])]
             for g in summary]
    widths = [max(len(row[i]) for row in [header] + lines) for i in range(len(header))]
    return "\n".join("  ".join(cell.ljust(widths[i]) for i, cell in enumerate(row)).rstrip() for row in [header] + lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("files", nargs="+", help="connection-measurements.jsonl files")
    parser.add_argument("--since", type=parse_time, help="ISO 8601 date or time, inclusive")
    parser.add_argument("--until", type=parse_time, help="ISO 8601 date or time, exclusive")
    parser.add_argument("--json", action="store_true", help="print the summary as JSON")
    args = parser.parse_args(argv)
    summary = summarize(load(args.files, args.since, args.until))
    print(json.dumps(summary, indent=2) if args.json else table(summary))


if __name__ == "__main__":
    sys.exit(main())
