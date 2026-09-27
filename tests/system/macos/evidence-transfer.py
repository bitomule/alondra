#!/usr/bin/env python3
"""Frozen, bounded evidence export from the owned disposable guest only."""
import base64
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tarfile

ROOT = Path('/Users/admin/alondra-gate')
CHUNK = 64 * 1024
MAX_ARCHIVE = 544 * 1024**2
MAX_CONTENT = 512 * 1024**2


def regular(path):
    if path.is_symlink() or not stat.S_ISREG(path.stat().st_mode):
        raise ValueError('expected regular evidence file')


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def identity(path):
    s = path.stat()
    return [s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns]


def descriptor(root):
    path = root / 'evidence.tar'
    regular(path)
    before = identity(path)
    if not 0 < before[2] <= MAX_ARCHIVE:
        raise ValueError('archive size out of bounds')
    value = {'schema': 1, 'size': before[2], 'sha256': sha(path), 'chunk_bytes': CHUNK}
    if identity(path) != before:
        raise ValueError('archive changed while hashing')
    return value


def prepare(root):
    project = root / 'project'
    if project.is_symlink() or not project.is_dir():
        raise ValueError('invalid project evidence directory')
    entries = [project]
    total = 0
    # Do not follow symlinks, including directory links. Stop before unbounded inventory.
    for parent, dirs, files in os.walk(project, followlinks=False):
        for name in dirs + files:
            path = Path(parent) / name
            s = path.lstat()
            if not (stat.S_ISREG(s.st_mode) or stat.S_ISDIR(s.st_mode)):
                raise ValueError('unsupported evidence entry')
            entries.append(path)
            total += s.st_size if stat.S_ISREG(s.st_mode) else 0
            if total > MAX_CONTENT or len(entries) > 10000:
                raise ValueError('evidence inventory exceeds limit')
    archive = root / 'evidence.tar'
    # A failed freeze never becomes a reusable archive. No regeneration during retries.
    partial = root / 'evidence.freeze.partial'
    with tarfile.open(partial, 'x:gz', compresslevel=1) as out:
        for path in entries:
            out.add(path, arcname=str(path.relative_to(root)), recursive=False)
            if partial.stat().st_size > MAX_ARCHIVE:
                raise ValueError('archive exceeds limit')
    if archive.exists():
        raise ValueError('archive already exists')
    os.link(partial, archive)  # exclusive publication within the private guest directory
    partial.unlink()
    value = descriptor(root)
    with (root / 'evidence-index.json').open('x') as stream:
        json.dump({'descriptor': value, 'identity': identity(archive)}, stream)
    return value


def chunk(root, offset):
    index = json.loads((root / 'evidence-index.json').read_text())
    path = root / 'evidence.tar'
    regular(path)
    if identity(path) != index['identity']:
        raise ValueError('frozen archive changed')
    size = index['descriptor']['size']
    if offset < 0 or offset >= size or offset % CHUNK:
        raise ValueError('invalid chunk offset')
    with path.open('rb') as stream:
        stream.seek(offset)
        data = stream.read(CHUNK)
    if identity(path) != index['identity']:
        raise ValueError('frozen archive changed during read')
    return {'schema': 1, 'offset': offset, 'total': size, 'archive_sha256': index['descriptor']['sha256'],
            'data': base64.b64encode(data).decode('ascii'), 'sha256': hashlib.sha256(data).hexdigest()}


def diagnostics(root):
    files = [root / 'project' / n for n in ('flow.txt', 'flow-lint.txt', 'doctor.txt', 'stop.txt')]
    runs = sorted((root / 'project/.mav/runs').glob('*'))[:4]
    for run in runs:
        if run.is_symlink():
            continue
        files.extend(run / n for n in ('commands.jsonl', 'run.json', 'report.json'))
    result = {}
    for path in files:
        if not path.exists() or path.is_symlink():
            continue
        regular(path)
        with path.open('rb') as stream:
            size = path.stat().st_size
            stream.seek(max(0, size - 3072))
            result[str(path.relative_to(root))] = {'size': size, 'truncated': size > 3072,
                                                 'tail': stream.read(3072).decode('utf-8', errors='replace')}
    return {'schema': 1, 'files': result}


def main():
    if subprocess.check_output(['/usr/sbin/sysctl', '-n', 'hw.model'], text=True).strip().startswith('VirtualMac') is False:
        raise ValueError('refusing non-virtual Mac')
    if not Path('/Users/admin/alondra-test-tools/base.json').is_file() or ROOT.is_symlink():
        raise ValueError('missing owned fixture')
    action = sys.argv[1]
    if action == 'prepare': result = prepare(ROOT)
    elif action == 'describe': result = descriptor(ROOT)
    elif action == 'chunk': result = chunk(ROOT, int(sys.argv[2]))
    elif action == 'diagnostics': result = diagnostics(ROOT)
    else: raise ValueError('unknown action')
    print(json.dumps(result, separators=(',', ':')))


if __name__ == '__main__':
    main()
