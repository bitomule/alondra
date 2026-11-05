#!/usr/bin/env python3
"""Ends the Alondra hosts that cut tests left running.

Only a host no process owns any more (parent 1), leading its own group, whose binary a test ran:
from a Bazel output base, or the copy a test made in its temporary folder. A production host and the
children of a daemon never match. Ends each with its whole group, then removes its fixture folder.
"""
import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess

BAZEL_HOST = re.compile(r'/[0-9a-f]{32}/execroot/[^/]+/bazel-out/[^/]+/bin/agent-host/alondra-host$')
TEST_COPY = re.compile(r'/private/tmp/bt\.[^/]+/at-[^/]+/work/alondra-host-before$')
FIXTURE_FOLDER = re.compile(r'(/private/tmp/as-[^/]+)/work/h/[^/]+$')


@dataclass(frozen=True)
class Process:
    pid: int
    parent: int
    group: int
    binary: str


def table():
    listing = subprocess.run(['/bin/ps', '-axo', 'pid=,ppid=,pgid=,comm='], capture_output=True, text=True, check=True).stdout
    processes = []
    for line in listing.splitlines():
        pid, parent, group, binary = line.split(None, 3)
        processes.append(Process(int(pid), int(parent), int(group), binary))
    return processes


def left_by_tests(processes):
    return [process for process in processes if process.parent == 1 and process.group == process.pid
            and (BAZEL_HOST.search(process.binary) or TEST_COPY.fullmatch(process.binary))]


def fixture_folder(pid):
    listing = subprocess.run(['/usr/sbin/lsof', '-a', '-p', str(pid), '-d', 'cwd', '-Fn'], capture_output=True, text=True).stdout
    for line in listing.splitlines():
        match = FIXTURE_FOLDER.fullmatch(line[1:])
        if line.startswith('n') and match:
            return Path(match.group(1))
    return None


def end(hosts, current=table):
    """Ends those that still are what they were when listed; a pid can have gone to another process."""
    ended, folders = 0, set()
    still = {process.pid: process for process in left_by_tests(current())}
    for host in hosts:
        if still.get(host.pid) != host:
            continue
        folder = fixture_folder(host.pid)
        os.killpg(host.pid, signal.SIGKILL)
        ended += 1
        if folder:
            folders.add(folder)
    for folder in folders:
        shutil.rmtree(folder, ignore_errors=True)
    return ended, len(folders)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--dry-run', action='store_true', help='List the hosts and end nothing')
    arguments = parser.parse_args()
    hosts = left_by_tests(table())
    if arguments.dry_run:
        for host in hosts:
            print(f'{host.pid} {host.binary}')
        print(f'{len(hosts)} hosts left by tests')
        return
    ended, folders = end(hosts)
    print(f'Ended {ended} of {len(hosts)} hosts left by tests and removed {folders} fixture folders')


if __name__ == '__main__':
    main()
