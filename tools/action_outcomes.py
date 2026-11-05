"""How the daemon's actions ended, per action and outcome, from the `action result:` lines of
alondrad.log and its rotated copies: how often an action stayed unknown or still running, and how
long it took."""
import re
import statistics
import sys
from collections import defaultdict
from pathlib import Path

RESULT = re.compile(r"action result: (\S+) from (\S+) → (.+), HTTP (\d+), in (\d+) ms")
LOOKUP = re.compile(r"action result lookup → (.+)$")


def main(paths):
    results = defaultdict(list)
    lookups = defaultdict(int)
    for path in paths:
        for candidate in (Path(path), Path(path + ".1")):
            if not candidate.is_file():
                continue
            for line in candidate.read_text(errors="replace").splitlines():
                if match := RESULT.search(line):
                    verb, client, outcome, _, millis = match.groups()
                    results[(verb, outcome)].append((client, int(millis)))
                elif match := LOOKUP.search(line):
                    lookups[match.group(1)] += 1
    if not results and not lookups:
        print("No action results logged yet.")
        return
    print(f"{'action':<24} {'outcome':<34} {'count':>6} {'median ms':>10} {'max ms':>8}  clients")
    for (verb, outcome), seen in sorted(results.items(), key=lambda item: (-len(item[1]), item[0])):
        millis = [ms for _, ms in seen]
        clients = ",".join(sorted({client for client, _ in seen}))
        print(f"{verb:<24} {outcome:<34} {len(seen):>6} {int(statistics.median(millis)):>10} {max(millis):>8}  {clients}")
    total = sum(len(seen) for seen in results.values())
    unclear = sum(len(seen) for (_, outcome), seen in results.items() if outcome.startswith(("unknown", "in_progress")))
    print(f"\n{total} action results, {unclear} still running or unknown.")
    for outcome, count in sorted(lookups.items(), key=lambda item: -item[1]):
        print(f"result lookups answered {outcome}: {count}")


if __name__ == "__main__":
    main(sys.argv[1:])
