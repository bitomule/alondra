#!/usr/bin/env python3
"""What Alondra and Bazel cost the Mac right now: memory, files and file-system events. Read-only.

- fseventsd's memory, which grows with the paths that change and only comes back when it restarts.
- During SECONDS (0 skips it), every file-system event and distinct path, by where it happens.
- Alondra hosts, split between those a daemon owns and those whose parent is gone.
- The size of Alondra's SDK host folders and files folder, and the live Bazel servers.
"""
import argparse
from collections import defaultdict
from dataclasses import dataclass
import os
from pathlib import Path
import re
import subprocess
import sys

UNITS = {'B': 1, 'K': 1 << 10, 'M': 1 << 20, 'G': 1 << 30, 'T': 1 << 40}
MEMORY = re.compile(r'^(\d+(?:\.\d+)?)([BKMGT])\+?-?$')


def megabytes(text):
    match = MEMORY.match(text.strip())
    if not match:
        return 0.0
    return float(match.group(1)) * UNITS[match.group(2)] / UNITS['M']


@dataclass(frozen=True)
class Process:
    pid: int
    parent: int
    rss_kib: int
    command: str


def processes():
    listing = subprocess.run(['/bin/ps', '-axo', 'pid=,ppid=,rss=,command='], capture_output=True, text=True, check=True).stdout
    table = []
    for line in listing.splitlines():
        fields = line.split(None, 3)
        if len(fields) == 4:
            table.append(Process(int(fields[0]), int(fields[1]), int(fields[2]), fields[3]))
    return table


def footprints(pids):
    """Each process's memory as Activity Monitor counts it, compressed and swapped pages included."""
    if not pids:
        return {}
    arguments = ['/usr/bin/top', '-l', '1', '-stats', 'pid,mem']
    for pid in pids:
        arguments += ['-pid', str(pid)]
    listing = subprocess.run(arguments, capture_output=True, text=True).stdout
    sizes = {}
    for line in listing.splitlines():
        fields = line.split()
        if len(fields) == 2 and fields[0].isdigit():
            sizes[int(fields[0])] = megabytes(fields[1])
    return sizes


def is_host(process):
    return Path(process.command.split()[0]).name in ('alondra-host', 'alondra-codex-host')


def is_bazel_server(process):
    return process.command.startswith('bazel(')


def hosts(table, sizes):
    owned = [process for process in table if is_host(process) and process.parent != 1]
    orphaned = [process for process in table if is_host(process) and process.parent == 1]
    return {
        'owned': (len(owned), sum(sizes.get(process.pid, 0.0) for process in owned)),
        'orphaned': (len(orphaned), sum(sizes.get(process.pid, 0.0) for process in orphaned)),
    }


def category(path, home, alondra_home):
    if path.startswith('/System/Volumes/Data/'):
        path = path[len('/System/Volumes/Data'):]
    if '/.Spotlight-V100/' in path:
        return 'Spotlight index'
    if '/execroot/' in path or '/rustc-incremental' in path or '/bazel-out/' in path or '/external/' in path:
        return 'Bazel output bases'
    if '/disk-cache/' in path or '/repo-cache/' in path:
        return 'Bazel caches'
    if path.startswith('/private/tmp/bt.'):
        return 'test temporary folders'
    if '/.claude/worktrees/' in path:
        return 'task worktrees'
    if alondra_home and path.startswith(alondra_home.rstrip('/') + '/'):
        return 'Alondra home'
    if path.startswith('/private/var/folders/'):
        return 'TMPDIR'
    if path.startswith('/private/tmp/'):
        return '/tmp other'
    if home and path.startswith(home.rstrip('/') + '/'):
        return 'home other'
    return 'other'


def tally(paths, home, alondra_home):
    events, distinct = defaultdict(int), defaultdict(set)
    for path in paths:
        name = category(path, home, alondra_home)
        events[name] += 1
        distinct[name].add(path)
    return {name: (events[name], len(distinct[name])) for name in events}


def sample_events(seconds):
    script = Path(__file__).resolve().with_name('footprint_events.swift')
    run = subprocess.Popen(['/usr/bin/swift', str(script), str(seconds)], stdout=subprocess.PIPE, text=True)
    for line in run.stdout:
        yield line.rstrip('\n')
    run.wait()


def folder_size(path):
    if not path.is_dir():
        return 0, 0
    entries = sum(1 for _ in path.iterdir())
    usage = subprocess.run(['/usr/bin/du', '-sk', str(path)], capture_output=True, text=True).stdout.split()
    return entries, int(usage[0]) / 1024 if usage else 0.0


def report(seconds, alondra_home, out=sys.stdout):
    home = os.path.expanduser('~')
    table = processes()
    watched = [process.pid for process in table if is_host(process) or is_bazel_server(process) or process.command.endswith('/fseventsd')]
    sizes = footprints(watched)
    fseventsd = next((process for process in table if process.command.endswith('/fseventsd')), None)
    if fseventsd:
        print(f'fseventsd: {sizes.get(fseventsd.pid, 0.0):.0f} MB', file=out)
    for kind, (count, total) in hosts(table, sizes).items():
        print(f'Alondra hosts {kind}: {count}, {total:.0f} MB', file=out)
    servers = [process for process in table if is_bazel_server(process)]
    print(f'Bazel servers: {len(servers)}, {sum(sizes.get(process.pid, 0.0) for process in servers):.0f} MB', file=out)
    for name in ('sdk', 'files'):
        entries, size = folder_size(Path(alondra_home) / name)
        print(f'Alondra {name}/: {entries} entries, {size:.0f} MB', file=out)
    if seconds > 0:
        counts = tally(sample_events(seconds), home, alondra_home)
        print(f'File-system events in {seconds} s: {sum(e for e, _ in counts.values())} events, '
              f'{sum(d for _, d in counts.values())} distinct paths', file=out)
        for name, (events, distinct) in sorted(counts.items(), key=lambda item: -item[1][1]):
            print(f'  {distinct:>8} distinct {events:>8} events  {name}', file=out)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--seconds', type=int, default=60, help='How long to count file-system events; 0 skips it')
    parser.add_argument('--alondra-home', default=os.environ.get('ALONDRA_HOME', os.path.expanduser('~/.alondra')))
    arguments = parser.parse_args()
    report(arguments.seconds, arguments.alondra_home)


if __name__ == '__main__':
    main()
