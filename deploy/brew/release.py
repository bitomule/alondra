#!/usr/bin/env python3
"""Validate public payloads and publish without replacing an existing asset."""
import argparse
import base64
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import tarfile
import tempfile

REPO = 'bitomule/homebrew-tap'
ROOT = pathlib.Path(__file__).resolve().parents[2]
TARGETS = ('aarch64-apple-darwin', 'x86_64-apple-darwin')
PUBLIC_FILES = {
    'alondra', 'alondrad', '.claude-plugin/marketplace.json',
    'plugin/bin/alondra', 'plugin/.mcp.json', 'plugin/hooks/hooks.json',
    'plugin/.claude-plugin/plugin.json', 'plugin/skills/alondra/SKILL.md',
    'plugin-orchestrator/.mcp.json', 'plugin-orchestrator/.claude-plugin/plugin.json',
    'plugin-orchestrator/skills/alondra-orchestrator/SKILL.md',
    'plugin-orchestrator/bin/alondra',
}


def version(value):
    if not re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', value):
        raise ValueError('Release version must be stable MAJOR.MINOR.PATCH')
    return value


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def stage_assets(root, destination):
    """Copy explicit runtime files; materialize only the known orchestrator wrapper."""
    root = root.resolve()
    alias = root / 'plugin-orchestrator/bin'
    if not alias.is_symlink() or alias.readlink() != pathlib.Path('../plugin/bin'):
        raise ValueError('Unexpected orchestrator bin alias; review the packaging mapping')
    for name in sorted(PUBLIC_FILES - {'alondra', 'alondrad'}):
        source_name = 'plugin/bin/alondra' if name == 'plugin-orchestrator/bin/alondra' else name
        source = root / source_name
        if not source.is_file() or source.resolve() != source:
            raise ValueError(f'Runtime source missing or contains an unexpected symlink: {source_name}')
        output = destination / name
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, output)


def validate_archive(path):
    """Exact allowlist: no source trees, links, app bundles, credentials or extras."""
    with tarfile.open(path, 'r:gz') as archive:
        files = set()
        for member in archive.getmembers():
            name = member.name.removeprefix('./').rstrip('/')
            if member.isdir() and any(p.startswith(name + '/') for p in PUBLIC_FILES):
                continue
            if not member.isfile() or name not in PUBLIC_FILES or name in files:
                raise ValueError(f'Unexpected public archive entry: {member.name}')
            files.add(name)
        if files != PUBLIC_FILES:
            raise ValueError(f'Missing public files: {sorted(PUBLIC_FILES - files)}')


def render(ver, directory):
    version(ver)
    values = {'VERSION': ver}
    for target, key in zip(TARGETS, ('ARM_SHA256', 'INTEL_SHA256')):
        archive = directory / f'alondra-{ver}-{target}.tar.gz'
        validate_archive(archive)
        values[key] = sha(archive)
    formula = (ROOT / 'deploy/brew/alondra.rb.in').read_text()
    for key, value in values.items():
        formula = formula.replace(f'@{key}@', value)
    if re.search(r'@[A-Z_0-9]+@', formula):
        raise ValueError('Unresolved formula placeholder')
    return formula


def gh(*args, payload=None):
    result = subprocess.run(['gh', *args], input=json.dumps(payload) if payload is not None else None,
                            text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(result.stderr.strip())
    return result.stdout


def api(endpoint, payload=None):
    args = ['api', endpoint]
    if payload is not None:
        args += ['--method', 'PUT', '--input', '-']
    return json.loads(gh(*args, payload=payload))


def optional_api(endpoint):
    try:
        return api(endpoint)
    except RuntimeError as error:
        if '(HTTP 404)' not in str(error):
            raise
        return None


def lookup_release(tag):
    # Query GraphQL directly: gh release view combines REST and GraphQL lookups
    # and can mask a GraphQL failure behind the REST 404 of an unpublished draft.
    owner, name = REPO.split('/')
    result = json.loads(gh('api', 'graphql', '--method', 'POST', '--input', '-', payload={
        'query': 'query($owner:String!,$name:String!,$tag:String!){repository(owner:$owner,name:$name){release(tagName:$tag){databaseId}}}',
        'variables': {'owner': owner, 'name': name, 'tag': tag},
    }))
    if result.get('errors'):
        raise RuntimeError('GraphQL release lookup failed: ' + json.dumps(result['errors']))
    repository = (result.get('data') or {}).get('repository')
    if repository is None or 'release' not in repository:
        raise RuntimeError('Release repository inaccessible or incomplete GraphQL response')
    release = repository['release']
    if release is None:
        return None  # Only an error-free, explicit null proves absence.
    release_id = release.get('databaseId')
    if type(release_id) is not int or release_id <= 0:
        raise RuntimeError('GraphQL release has no valid database ID')
    return api(f'repos/{REPO}/releases/{release_id}')


def publish(ver, directory):
    formula = render(ver, directory)  # fail closed before any external mutation
    tag = f'alondra-v{ver}'
    formula_endpoint = f'repos/{REPO}/contents/Formula/alondra.rb'
    current = optional_api(formula_endpoint)
    if current:
        old = base64.b64decode(current['content']).decode()
        match = re.search(r'^  version "([0-9.]+)"$', old, re.M)
        if not match:
            raise ValueError('Cannot determine existing formula version')
        if tuple(map(int, match[1].split('.'))) > tuple(map(int, ver.split('.'))):
            raise ValueError('Refusing to downgrade the public formula')
        if match[1] == ver and old != formula:
            raise ValueError('Same version already has different formula; issue a new version')
    release = lookup_release(tag)
    if release is None:
        gh('release', 'create', tag, '--repo', REPO, '--draft', '--title', f'Alondra {ver}',
           '--notes', 'Signed macOS CLI, TUI, daemon and Claude integration. Install: brew install bitomule/tap/alondra')
        release = lookup_release(tag)
    names = {asset['name'] for asset in release['assets']}
    archives = [directory / f'alondra-{ver}-{target}.tar.gz' for target in TARGETS]
    expected = {p.name for p in archives}
    if names - expected:
        raise ValueError('Unexpected existing release assets; inspect manually')
    if not release['draft'] and names != expected:
        raise ValueError('Published release is incomplete; do not mutate it')
    with tempfile.TemporaryDirectory() as temp:
        for path in archives:
            if path.name in names:
                gh('release', 'download', tag, '--repo', REPO, '--pattern', path.name, '--dir', temp)
                if sha(pathlib.Path(temp) / path.name) != sha(path):
                    raise ValueError(f'Immutable asset differs: {path.name}. Reuse original workflow artifacts.')
            else:
                gh('release', 'upload', tag, str(path), '--repo', REPO)  # never --clobber
                gh('release', 'download', tag, '--repo', REPO, '--pattern', path.name, '--dir', temp)
                if sha(pathlib.Path(temp) / path.name) != sha(path):
                    raise ValueError('Uploaded asset checksum mismatch')
    if release['draft']:
        gh('release', 'edit', tag, '--repo', REPO, '--draft=false', '--latest=false')
    if current and old == formula:
        return
    # GitHub Contents API compares the original blob SHA, preventing lost updates.
    payload = {'message': f'alondra {ver}', 'content': base64.b64encode(formula.encode()).decode()}
    if current:
        payload['sha'] = current['sha']
    api(formula_endpoint, payload)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=('validate', 'render', 'publish', 'stage-assets'))
    parser.add_argument('version')
    parser.add_argument('path', type=pathlib.Path)
    args = parser.parse_args()
    version(args.version)
    if args.mode == 'stage-assets':
        stage_assets(ROOT, args.path)
    elif args.mode == 'validate':
        validate_archive(args.path)
    elif args.mode == 'render':
        print(render(args.version, args.path), end='')
    else:
        publish(args.version, args.path)


if __name__ == '__main__':
    main()
