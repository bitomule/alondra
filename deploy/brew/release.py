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
from urllib.parse import urlencode

REPO = 'bitomule/homebrew-tap'
ROOT = pathlib.Path(__file__).resolve().parents[2]
TARGETS = ('aarch64-apple-darwin',)
BINARIES = {'alondra', 'alondrad', 'alondrad-launcher'}
PUBLIC_FILES = {
    'alondra', 'alondrad', 'alondrad-launcher', '.claude-plugin/marketplace.json',
    'plugin/bin/alondra', 'plugin/.mcp.json', 'plugin/hooks/hooks.json',
    'plugin/hooks/register.ts', 'plugin/tests/register.test.ts', 'plugin/tests/holds.test.ts',
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
    for name in sorted(PUBLIC_FILES - BINARIES):
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
    for target, key in zip(TARGETS, ('ARM_SHA256',)):
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


def api(endpoint, payload=None, method=None):
    args = ['api', '--hostname', 'github.com', endpoint]
    if payload is not None:
        args += ['--method', method or 'PUT', '--input', '-']
    elif method:
        args += ['--method', method]
    return json.loads(gh(*args, payload=payload))


def optional_api(endpoint):
    try:
        return api(endpoint)
    except RuntimeError as error:
        if '(HTTP 404)' not in str(error):
            raise
        # A masked auth/repository failure must not look like a missing formula.
        api(f'repos/{REPO}')
        return None


def identifier(value):
    if type(value) is not int or value <= 0:
        raise RuntimeError('REST response has no valid numeric ID')
    return value


def pages(endpoint):
    page = 1
    while True:
        items = api(f'{endpoint}?per_page=100&page={page}')
        if not isinstance(items, list) or any(not isinstance(item, dict) for item in items):
            raise RuntimeError('Invalid REST list response')
        yield from items
        if len(items) < 100:
            return
        page += 1


def release_assets(release_id):
    assets = {}
    for asset in pages(f'repos/{REPO}/releases/{identifier(release_id)}/assets'):
        identifier(asset.get('id'))
        name = asset.get('name')
        if not isinstance(name, str) or name in assets:
            raise RuntimeError('Invalid or duplicate release asset name')
        assets[name] = asset
    return assets


def lookup_release(tag):
    # The authenticated list includes drafts. Never infer absence from a tag
    # endpoint's 404, which may instead hide a draft or missing permissions.
    api(f'repos/{REPO}')
    matches = []
    for item in pages(f'repos/{REPO}/releases'):
        identifier(item.get('id'))
        if not isinstance(item.get('tag_name'), str):
            raise RuntimeError('Invalid REST release listing')
        if item['tag_name'] == tag:
            matches.append(item)
    if len(matches) > 1:
        raise RuntimeError('Multiple releases use the requested tag')
    if not matches:
        return None
    release = api(f'repos/{REPO}/releases/{identifier(matches[0].get("id"))}')
    if release.get('tag_name') != tag or type(release.get('draft')) is not bool:
        raise RuntimeError('Invalid REST release response')
    identifier(release.get('id'))
    return release


def upload_asset(release_id, path):
    # Full URL avoids gh treating uploads.github.com as an Enterprise /api/v3
    # endpoint. gh normalizes *.github.com for authentication; no token in argv.
    endpoint = f'https://uploads.github.com/repos/{REPO}/releases/{identifier(release_id)}/assets?{urlencode({"name": path.name})}'
    asset = json.loads(gh('api', '--hostname', 'uploads.github.com', endpoint,
        '--method', 'POST', '--header', 'Content-Type: application/octet-stream',
        '--input', str(path)))
    identifier(asset.get('id'))
    if asset.get('name') != path.name or asset.get('state') != 'uploaded':
        raise RuntimeError('Upload did not return the expected complete asset')
    return asset


def download_asset(asset_id, destination):
    with destination.open('wb') as output:
        result = subprocess.run(['gh', 'api', '--hostname', 'github.com',
            f'repos/{REPO}/releases/assets/{identifier(asset_id)}',
            '--header', 'Accept: application/octet-stream', '--allow-escape-sequences'],
            stdout=output, stderr=subprocess.PIPE)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors='replace').strip())


def publish(ver, directory):
    formula = render(ver, directory)  # fail closed before any external mutation
    tag = f'alondra-v{ver}'
    formula_endpoint = f'repos/{REPO}/contents/Formula/alondra.rb'
    release = lookup_release(tag)  # successful authenticated listing proves absence
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
    if release is None:
        release = api(f'repos/{REPO}/releases', {
            'tag_name': tag, 'draft': True, 'name': f'Alondra {ver}',
            'body': 'Signed macOS CLI, TUI, daemon and Claude integration. Install: brew install bitomule/tap/alondra',
        }, method='POST')
        if release.get('tag_name') != tag or release.get('draft') is not True:
            raise RuntimeError('Release creation did not return the expected draft')
    release_id = identifier(release.get('id'))
    assets = release_assets(release_id)
    names = set(assets)
    archives = [directory / f'alondra-{ver}-{target}.tar.gz' for target in TARGETS]
    expected = {p.name for p in archives}
    if names - expected:
        raise ValueError('Unexpected existing release assets; inspect manually')
    if not release['draft'] and names != expected:
        raise ValueError('Published release is incomplete; do not mutate it')
    with tempfile.TemporaryDirectory() as temp:
        for path in archives:
            existing = path.name in assets
            asset = assets[path.name] if existing else upload_asset(release_id, path)
            if asset.get('state') != 'uploaded':
                raise ValueError('Existing release asset is incomplete; inspect manually')
            downloaded = pathlib.Path(temp) / path.name
            download_asset(asset['id'], downloaded)
            if sha(downloaded) != sha(path):
                raise ValueError(f'Immutable asset differs: {path.name}. Reuse the original signed archives.')
            assets[path.name] = asset
    # Catch unexpected concurrent uploads before making the release public.
    latest = release_assets(release_id)
    if set(latest) != expected or any(latest[name]['id'] != assets[name]['id'] for name in expected):
        raise ValueError('Release assets changed during verification')
    if release['draft']:
        published = api(f'repos/{REPO}/releases/{release_id}',
            {'draft': False, 'make_latest': 'false'}, method='PATCH')
        if published.get('draft') is not False:
            raise RuntimeError('Release remains a draft; formula was not updated')
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
