#!/usr/bin/env python3
"""THIRD_PARTY_NOTICES.md: the licenses of everything Alondra distributes that it did not write.

`generate` rebuilds the file from cargo, the npm registry and GitHub (needs the network and a
fetched Cargo registry). `check` reads only the lockfiles in the checkout and fails when the file
was generated from other inputs, so Bazel can run it hermetically.
"""
import hashlib
import json
import pathlib
import re
import subprocess
import sys
import tarfile
import io
import urllib.request

ROOT = pathlib.Path(__file__).absolute().parents[1]
NOTICES = 'THIRD_PARTY_NOTICES.md'
APP_LICENSES = 'apple/Features/Sources/Settings/Licenses/AppLicenses.swift'
RUST_PRODUCTS = ('alondra', 'alondrad')
RUST_TARGET = 'aarch64-apple-darwin'
ANTHROPIC_SDK = '@anthropic-ai/claude-agent-sdk'
LICENSE_FILE = re.compile(r'(?i)^(licen[sc]e|copying|unlicense|notice)([-_.].*)?$')
HIGHLIGHT_JS = ('highlight.js (bundled in highlightswift)', '11.9.0')
MIT_TEXT = """MIT License

Copyright (c) {authors}

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE."""
FONT_LICENSES = ('archivo', 'martian-mono')
INPUT_LINE = re.compile(r'^<!-- input (\S+) ([0-9a-f]{64}) -->$', re.M)


def digest(items):
    return hashlib.sha256('\n'.join(sorted(items)).encode()).hexdigest()


def rust_components(root):
    return locked_registry_packages((root / 'Cargo.lock').read_text())


def locked_registry_packages(lock_text):
    components = []
    for block in lock_text.split('[[package]]')[1:]:
        fields = dict(re.findall(r'^(\w+) = "([^"]*)"$', block, re.M))
        if 'source' in fields:
            components.append(f"{fields['name']} {fields['version']} {fields['source']} {fields.get('checksum', '')}")
    return components


def pnpm_sections(text):
    sections, section, entry, sub = {}, None, None, None
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        indent = len(line) - len(line.lstrip())
        body = line.strip()
        if indent == 0:
            section = body.rstrip(':')
            sections[section] = {}
        elif indent == 2 and section in ('packages', 'snapshots'):
            entry = body.rstrip(':').strip("'")
            sections[section][entry] = {}
            sub = None
        elif indent == 4 and entry is not None:
            key, _, value = body.partition(':')
            sections[section][entry][key] = value.strip()
            sub = key if not value.strip() else None
        elif indent == 6 and entry is not None and sub:
            key, _, value = body.partition(':')
            sections[section][entry].setdefault(sub + '.items', {})[key.strip("'")] = value.strip().strip("'")
    return sections


def split_id(identifier):
    name, _, version = identifier.split('(')[0].rpartition('@')
    return name, version


def host_components(root):
    text = (root / 'agent-host/pnpm-lock.yaml').read_text()
    sections = pnpm_sections(text)
    roots = re.search(r'importers:\n\n  \.:\n    dependencies:\n((?:      .*\n|\n)+?)    devDependencies:', text)
    pending = [f'{m.group(1).strip(chr(39))}@{m.group(2)}' for m in re.finditer(r"^      (\S+):\n        specifier: .*\n        version: (\S+)$", roots.group(1), re.M)]
    seen = {}
    while pending:
        identifier = pending.pop()
        if identifier in seen:
            continue
        name, version = split_id(identifier)
        meta = sections['packages'].get(f'{name}@{version}', {})
        platform_os, platform_cpu = meta.get('os', ''), meta.get('cpu', '')
        if (platform_os and 'darwin' not in platform_os) or (platform_cpu and 'arm64' not in platform_cpu):
            continue
        seen[identifier] = (name, version, meta.get('resolution', ''))
        snapshot = sections['snapshots'].get(identifier, {})
        for kind in ('dependencies.items', 'optionalDependencies.items'):
            for dep, dep_version in snapshot.get(kind, {}).items():
                pending.append(f'{dep}@{dep_version}')
    return sorted({(n, v) for n, v, _ in seen.values()})


def web_components(root):
    lock = json.loads((root / 'web/package-lock.json').read_text())
    result = []
    for path, meta in lock['packages'].items():
        if path and not meta.get('dev') and not meta.get('devOptional'):
            name = path.split('node_modules/')[-1]
            result.append((name, meta['version'], meta.get('license', ''), meta.get('resolved', '')))
    return sorted(result)


def swift_components(root):
    pins = json.loads((root / 'Package.resolved').read_text())['pins']
    return sorted((p['identity'], p['state'].get('version', ''), p['state']['revision'], p['location']) for p in pins)


def node_version(root):
    return re.search(r'node_version = "([^"]+)"', (root / 'MODULE.bazel').read_text()).group(1)


def inputs(root=ROOT):
    return {
        'rust': digest(rust_components(root)),
        'agent-host': digest(f'{n}@{v}' for n, v in host_components(root)),
        'web': digest(f'{n}@{v}' for n, v, _, _ in web_components(root)),
        'swift': digest(f'{n}@{v}@{r}' for n, v, r, _ in swift_components(root)),
        'node': digest([node_version(root), (root / 'deploy/brew/node/LICENSE').read_text()]),
        'fonts': digest((root / f'web/app/public/fonts/OFL-{font}.txt').read_text() for font in FONT_LICENSES),
    }


def fetch(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'alondra-notices'})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read()


def fetch_optional(url):
    try:
        return fetch(url).decode(errors='replace')
    except Exception:
        return None


def license_texts(directory):
    return [path.read_text(errors='replace').strip() for path in sorted(directory.iterdir()) if path.is_file() and LICENSE_FILE.match(path.name)]


def run(*args):
    return subprocess.run(args, cwd=ROOT, check=True, capture_output=True, text=True).stdout


def rust_entries():
    run('cargo', 'fetch', '--locked')
    metadata = json.loads(run('cargo', 'metadata', '--locked', '--format-version', '1', '--filter-platform', RUST_TARGET))
    known = {(p['name'], p['version']): p for p in metadata['packages']}
    entries = {}
    for product in RUST_PRODUCTS:
        tree = run('cargo', 'tree', '--locked', '--edges', 'normal,build', '--target', RUST_TARGET, '-p', product, '--prefix', 'none', '--format', '{p}')
        for line in tree.splitlines():
            found = re.match(r'^(\S+) v(\S+)', line)
            if not found or '(/' in line:
                continue
            package = known[(found.group(1), found.group(2))]
            if package.get('source') is None:
                continue
            directory = pathlib.Path(package['manifest_path']).parent
            entries[(package['name'], package['version'])] = (package.get('license') or 'see crate', license_texts(directory), package.get('authors', []))
    return entries


def npm_texts(name, version, url):
    if not url:
        url = f"https://registry.npmjs.org/{name}/-/{name.split('/')[-1]}-{version}.tgz"
    with tarfile.open(fileobj=io.BytesIO(fetch(url)), mode='r:gz') as archive:
        package, texts = {}, []
        for member in archive.getmembers():
            parts = member.name.split('/')
            if len(parts) == 2 and member.isfile():
                data = archive.extractfile(member).read().decode(errors='replace').strip()
                if parts[1] == 'package.json':
                    package = json.loads(data)
                elif LICENSE_FILE.match(parts[1]):
                    texts.append(data)
    declared = package.get('license')
    if isinstance(declared, dict):
        declared = declared.get('type')
    return declared or 'see package', texts


def host_entries():
    entries, anthropic = {}, {}
    pnpm = pnpm_sections((ROOT / 'agent-host/pnpm-lock.yaml').read_text())['packages']
    for name, version in host_components(ROOT):
        resolved = pnpm.get(f'{name}@{version}', {}).get('resolution', '')
        found = re.search(r'tarball: (\S+?)[,}]', resolved)
        spdx, texts = npm_texts(name, version, found.group(1) if found else '')
        (anthropic if name.startswith(ANTHROPIC_SDK) else entries)[(name, version)] = (spdx, texts, [])
    return entries, anthropic


def web_entries():
    entries = {}
    for name, version, spdx, url in web_components(ROOT):
        declared, texts = npm_texts(name, version, url)
        entries[(name, version)] = (spdx or declared, texts, [])
    return entries


def swift_entries():
    entries = {}
    for identity, version, revision, location in swift_components(ROOT):
        owner_repo = re.search(r'github\.com/([^/]+/[^/.]+)', location).group(1)
        texts = []
        for name in ('LICENSE', 'LICENSE.md', 'LICENSE.txt'):
            text = fetch_optional(f'https://raw.githubusercontent.com/{owner_repo}/{revision}/{name}')
            if text:
                texts.append(text.strip())
                break
        entries[(identity, version or revision[:12])] = ('see text' if texts else 'see repository', texts, [])
    name, version = HIGHLIGHT_JS
    entries[(name, version)] = ('BSD-3-Clause', [fetch(f'https://raw.githubusercontent.com/highlightjs/highlight.js/{version}/LICENSE').decode().strip()], [])
    return entries


def group(entries):
    groups = {}
    for (name, version), (spdx, texts, authors) in sorted(entries.items()):
        holders = ', '.join(authors) or f'the authors of {name}'
        key = '\n\n'.join(texts) if texts else MIT_TEXT.format(authors=holders) if 'MIT' in spdx else f'(The published package carries no license file. Declared license: {spdx}. Authors: {holders}.)'
        groups.setdefault(key, {'spdx': set(), 'users': []})
        groups[key]['spdx'].add(spdx)
        groups[key]['users'].append(f'{name} {version}')
    return groups


def render_groups(entries):
    out = []
    for text, found in sorted(group(entries).items(), key=lambda item: item[1]['users'][0]):
        out.append(f"#### {' / '.join(sorted(found['spdx']))}\n\nUsed by: {', '.join(found['users'])}\n\n````text\n{text}\n````\n")
    return '\n'.join(out)


HEADER = """# Third-party notices

Alondra is licensed under the Functional Source License, Version 1.1, MIT Future License
(`LICENSE.md`). The components below are not covered by that license: each keeps its own, reproduced
here as its authors require. This file is generated by `tools/third_party_notices.py` and checked by
`//tools:licenses_test`; do not edit it by hand.

## Anthropic Agent SDK: not under FSL

The Agent SDK host (`sdk/alondra-host`) bundles `@anthropic-ai/claude-agent-sdk`. It is not open
source and is not covered by Alondra's license. Anthropic owns it ("All rights reserved") and its
use is subject to Anthropic's legal agreements (https://code.claude.com/docs/en/legal-and-compliance),
including the Commercial Terms of Service.
Alondra's source tree never contains it: the build downloads it from npm. Its own license text:

{anthropic}
## Alondra's name and logo

The Alondra name, logo and app icon are not licensed. FSL grants no trademark rights.

## Fonts

The website serves two fonts under the SIL Open Font License 1.1: Archivo and Martian Mono.
Their license files ship next to them in `web/app/public/fonts`.

{fonts}
## Icons and symbols

The app icon, the logo and the favicon are Alondra's own artwork. The apps draw their symbols from
Apple's SF Symbols, which Apple licenses for use inside apps on its platforms and which are not
redistributed with Alondra. No third-party icon set is bundled.

## Node.js

The Agent SDK host and the Codex host are Node.js single-executable applications, so each one
contains Node.js {node} (MIT license, with the notices for OpenSSL, ICU, V8 and the other components
Node bundles). The complete license text is `deploy/brew/node/LICENSE` and ships in the Homebrew
package as `sdk/NODE_LICENSE` and `codex/NODE_LICENSE`.

## Rust crates

Linked into `alondra`, `alondrad` and `alondrad-launcher` for macOS (Apple Silicon).
`attohttpc` (MPL-2.0, through iroh) is used unmodified; its source is at
https://github.com/sbstp/attohttpc.

{rust}
## Agent SDK host packages

Bundled into the Agent SDK host and the Codex host, besides the Anthropic SDK above.

{host}
## Website packages

Production dependencies of the website (`web`).

{web}
## Swift packages

Linked into the iPhone and Mac apps.

{swift}
"""


def generate():
    host, anthropic = host_entries()
    fonts = '\n'.join(f"````text\n{(ROOT / f'web/app/public/fonts/OFL-{font}.txt').read_text().strip()}\n````\n" for font in FONT_LICENSES)
    marks = ''.join(f'<!-- input {name} {value} -->\n' for name, value in inputs().items())
    body = HEADER.format(
        anthropic='\n'.join(f"````text\n{t}\n````\n" for t in sorted({texts[0] for _, texts, _ in anthropic.values() if texts})),
        fonts=fonts, node=node_version(ROOT),
        rust=render_groups(rust_entries()), host=render_groups(host), web=render_groups(web_entries()), swift=render_groups(swift_entries()),
    )
    (ROOT / NOTICES).write_text(marks + '\n' + body)


def stale(root=ROOT):
    """The input names whose lockfiles changed since the notices were generated."""
    written = dict(INPUT_LINE.findall((root / NOTICES).read_text()))
    current = inputs(root)
    return sorted(name for name in current.keys() | written.keys() if current.get(name) != written.get(name))


def app_packages(notices):
    """The licenses of the Swift packages linked into the apps: (names, text) per distinct license."""
    section = notices.split('\n## Swift packages\n', 1)[1]
    found = []
    for block in section.split('\n#### ')[1:]:
        users = re.search(r'^Used by: (.+)$', block, re.M).group(1)
        text = re.search(r'````text\n(.*?)\n````', block, re.S).group(1)
        found.append((users, text))
    return found


def swift_literal(text):
    hashes = '#' * (max((len(m) for m in re.findall(r'"""#*', text)), default=0) + 1)
    return f'{hashes}"""\n{text}\n"""{hashes}'


def app_licenses_source(root=ROOT):
    alondra = (root / 'LICENSE.md').read_text().strip()
    packages = ',\n'.join(
        f'        AppLicense(usedBy: {swift_literal(users)}, text: {swift_literal(text)})'
        for users, text in app_packages((root / NOTICES).read_text()))
    return (
        '// Generated by tools/third_party_notices.py from LICENSE.md and THIRD_PARTY_NOTICES.md; do not edit.\n\n'
        'enum AppLicenses {\n'
        f'    static let alondra = AppLicense(usedBy: "Alondra", text: {swift_literal(alondra)})\n\n'
        f'    static let packages: [AppLicense] = [\n{packages},\n    ]\n'
        '}\n')


if __name__ == '__main__':
    mode = sys.argv[1] if len(sys.argv) > 1 else 'check'
    if mode == 'generate':
        generate()
        (ROOT / APP_LICENSES).write_text(app_licenses_source())
    elif mode == 'check':
        changed = stale()
        if changed:
            sys.exit(f'{NOTICES} is out of date for: {", ".join(changed)}. Run `python3 tools/third_party_notices.py generate`.')
        if (ROOT / APP_LICENSES).read_text() != app_licenses_source():
            sys.exit(f'{APP_LICENSES} does not match LICENSE.md and {NOTICES}. Run `python3 tools/third_party_notices.py apps`.')
    elif mode == 'apps':
        (ROOT / APP_LICENSES).write_text(app_licenses_source())
    else:
        sys.exit('usage: third_party_notices.py generate|check')
