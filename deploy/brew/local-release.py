#!/usr/bin/env python3
"""On-demand local release; never exports signing keys or starts hosted jobs."""
import argparse
import fcntl
import json
import os
import pathlib
import platform
import plistlib
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import tomllib

import release

ROOT = pathlib.Path(__file__).resolve().parents[2]
IDENTITY = '137E48D7AF54AAA3DFC56B9820DACC897C20AF15'
TEAM = '658YBSK6MG'
LOW_FREE = 8 * 1024**3
REHEARSAL = 'upgrade-rehearsal.json'
SDK_HANDOVER = 'sdk-handover'
FIRST_SDK_RELEASE = (0, 3, 0)
FRESH_SDK = 'fresh-sdk'
HANDOVER_CRASHES = ('planned', 'bg_stopped', 'sdk_admitted')
SDK_REHEARSAL = {'--sandbox-tool': '//tools/sandbox:alondra-sandbox'}
# What a release ships, by the name it ships under: the binaries `bazelisk test //crates/...` built
# and tested, in the same configuration, so the release never compiles them a second time.
RELEASE_TARGETS = {
    'alondra': '//crates/alondra:alondra',
    'alondrad': '//crates/alondrad:alondrad',
    'alondrad-launcher': '//crates/alondrad:alondrad-launcher',
    'alondra-host': '//agent-host:release_host',
}
HOST_IDENTIFIER = 'com.bitomule.alondra-host'
# V8 compiles JavaScript at run time; the hardened runtime allows it only with this entitlement.
HOST_ENTITLEMENTS = ROOT / 'agent-host/host-entitlements.plist'


def run(*args, capture=False, env=None):
    return subprocess.run(list(map(str, args)), cwd=ROOT, env=env, check=True,
                          text=True, stdout=subprocess.PIPE if capture else None).stdout


def workflow_guard():
    # Tags use workflows from their source commit. Fail closed if automatic
    # workflows return; never infer zero cost from a presumed included allowance.
    workflows = ROOT / '.github/workflows'
    if any(workflows.glob('*.yml')) or any(workflows.glob('*.yaml')):
        raise RuntimeError('Remove automatic GitHub workflows from the release source; this pipeline is local only')


def preflight(ver, retry_source=None):
    release.version(ver)
    data = tomllib.loads((ROOT / 'Cargo.toml').read_text())
    if data['workspace']['package']['version'] != ver:
        raise RuntimeError('Bump Cargo version in a reviewed PR first')
    if run('git', 'status', '--porcelain', capture=True).strip():
        raise RuntimeError('Working tree must be clean')
    run('git', 'fetch', 'origin', 'main')
    source = run('git', 'rev-parse', 'HEAD', capture=True).strip()
    if retry_source is not None and source != retry_source:
        raise RuntimeError('Retry from a clean checkout of the original manifest source commit')
    try:
        run('git', 'merge-base', '--is-ancestor', 'HEAD', 'origin/main')
    except subprocess.CalledProcessError:
        raise RuntimeError('Release source must be a commit already on origin/main') from None
    workflow_guard()
    return source


def tag_state(ver, source, retry):
    tag = f'alondra-v{ver}'
    remote = run('git', 'ls-remote', 'origin', f'refs/tags/{tag}',
                 f'refs/tags/{tag}^{{}}', capture=True)
    local = run('git', 'tag', '--list', tag, capture=True).strip()
    if not retry and (remote.strip() or local):
        raise RuntimeError('Tag exists; use --retry-publish with the original prepared artifacts')
    if retry:
        refs = dict(line.split()[::-1] for line in remote.splitlines())
        remote_source = refs.get(f'refs/tags/{tag}^{{}}', refs.get(f'refs/tags/{tag}'))
        if remote_source and remote_source != source:
            raise RuntimeError('Remote tag points to another source; refusing publication')
        if local and run('git', 'rev-parse', f'{tag}^{{commit}}', capture=True).strip() != source:
            raise RuntimeError('Local tag points to another source; refusing publication')
    return bool(local), bool(remote.strip())


def require_disk_space(path):
    ancestor = path
    while not ancestor.exists():
        ancestor = ancestor.parent
    free = shutil.disk_usage(ancestor).free
    if free < LOW_FREE:
        raise RuntimeError(f'At least 8 GiB must be free on the volume containing {path}; '
                           f'only {free / 1024**3:.1f} GiB is available')


def manifest(ver, source, output):
    return {'schema': 1, 'version': ver, 'source': source,
            'validation': 'ARM native only; Apple Silicon exclusive release',
            'archives': {f'alondra-{ver}-{target}.tar.gz': release.sha(output / f'alondra-{ver}-{target}.tar.gz')
                         for target in release.TARGETS}}


def bazel_file(label, *flags):
    execroot = pathlib.Path(run('bazelisk', 'info', 'execution_root', capture=True).strip())
    files = [line for line in run('bazelisk', 'cquery', *flags, '--output=files', label, capture=True).split()
             if not line.endswith('.ok') and not line.endswith('.manifest.json')]
    if len(files) != 1:
        raise RuntimeError(f'{label} does not build exactly one file: {files}')
    return execroot / files[0]


def release_build(ver):
    """Finds what ships among the outputs the tests just built: with the tests' flags, Bazel runs
    no new compile action for the Rust binaries."""
    run('bazelisk', 'build', *RELEASE_TARGETS.values())
    built = {name: bazel_file(label) for name, label in RELEASE_TARGETS.items()}
    check_versions(ver, built)
    built['alondra-host.json'] = built['alondra-host'].with_name('alondra-host.manifest.json')
    return built


def closed_port():
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


def check_versions(ver, built):
    """Never sign binaries that would report another version than the one being released."""
    with tempfile.TemporaryDirectory(prefix='alondra-release-version-') as home:
        environment = {'HOME': home, 'ALONDRA_HOME': str(pathlib.Path(home) / '.alondra'), 'ALONDRA_LANG': 'en',
                       'ALONDRA_PORT': str(closed_port()), 'PATH': '/usr/bin:/bin'}
        daemon = run(built['alondrad'], '--version', capture=True, env=environment).strip()
        cli = run(built['alondra'], '--version', capture=True, env=environment).strip()
    if daemon != f'alondrad {ver}':
        raise RuntimeError(f'alondrad reports {daemon!r}, not {ver}: bump version.bzl with Cargo.toml')
    if not cli.startswith(f'alondra {ver} '):
        raise RuntimeError(f'alondra reports {cli!r}, not {ver}: bump version.bzl with Cargo.toml')


def rehearse_upgrade(ver, output):
    """The previous release must replace itself with these bytes on copies of this Mac's HOME, and
    its claude --bg nodes move to the Agent SDK with their sessions."""
    archive = output / f'alondra-{ver}-{release.TARGETS[0]}.tar.gz'
    run('bazelisk', 'build', *SDK_REHEARSAL.values())
    artifacts = [value for flag, label in SDK_REHEARSAL.items() for value in (flag, bazel_file(label))]
    run(sys.executable, ROOT / 'deploy/brew/upgrade-rehearsal.py', '--new', archive, *artifacts,
        '--sdk-fixtures', ROOT / 'agent-host', '--output', output / REHEARSAL)


def verify_rehearsal(ver, output):
    path = output / REHEARSAL
    if not path.is_file():
        raise RuntimeError('No upgrade rehearsal passed for these artifacts; prepare them again')
    saved = json.loads(path.read_text())
    archive = f'alondra-{ver}-{release.TARGETS[0]}.tar.gz'
    rehearsed = saved.get('archive') or {}
    if saved.get('to') != ver or rehearsed.get('name') != archive or rehearsed.get('sha256') != release.sha(output / archive):
        raise RuntimeError('The upgrade rehearsal ran on other bytes than the prepared archive; prepare them again')
    handover = saved.get('cases', {}).get(SDK_HANDOVER) or {}
    leaves_claude_bg = tuple(int(part) for part in str(saved.get('from', '0')).split('.')[:3]) < FIRST_SDK_RELEASE
    if leaves_claude_bg and handover.get('crashed_after') != list(HANDOVER_CRASHES):
        raise RuntimeError('The upgrade rehearsal did not move claude --bg nodes to the Agent SDK with a crash after each step; prepare them again')
    if not (saved.get('cases', {}).get(FRESH_SDK) or {}).get('session'):
        raise RuntimeError('The upgrade rehearsal did not run a task of a fresh install through its own Agent SDK host; prepare them again')


def verify_prepared(ver, source, output):
    saved = json.loads((output / 'manifest.json').read_text())
    if saved != manifest(ver, source, output):
        raise RuntimeError('Prepared manifest/source/version/checksums differ; never rebuild a published version')
    release.render(ver, output)  # Same strict archive allowlist as the publisher.
    verify_rehearsal(ver, output)


def smoke(ver, target, archive):
    release.validate_archive(archive)
    with tempfile.TemporaryDirectory(prefix='alondra-local-install-') as temp:
        prefix = pathlib.Path(temp).resolve()
        unpack = prefix / 'unpack'
        unpack.mkdir()
        with tarfile.open(archive) as tar:
            tar.extractall(unpack, filter='data')
        keg = prefix / 'Cellar/alondra' / ver
        binaries = keg / 'bin'
        assets = keg / 'libexec/alondra'
        binaries.mkdir(parents=True)
        assets.mkdir(parents=True)
        for name in SIGNED:
            shutil.move(unpack / name, binaries / name)
        for name in ('.claude-plugin', 'plugin', 'plugin-orchestrator', 'sdk'):
            shutil.move(unpack / name, assets / name)
        opt = prefix / 'opt/alondra'
        opt.parent.mkdir()
        opt.symlink_to(keg, target_is_directory=True)
        if target != 'aarch64-apple-darwin':
            raise RuntimeError(f'Unsupported release target: {target}; Alondra ships Apple Silicon only')
        arch = 'arm64'
        for name in SIGNED:
            binary = binaries / name
            if run('lipo', '-archs', binary, capture=True).strip() != arch:
                raise RuntimeError(f'Wrong architecture for {name}: expected {arch}')
            run('codesign', '--verify', '--strict', binary)
            run('/usr/bin/arch', f'-{arch}', binary, '--version' if name == 'alondrad-launcher' else '--help')
        host_smoke(assets / release.HOST)
        run(sys.executable, ROOT / 'deploy/brew/smoke-setup.py', opt)


def host_smoke(host):
    """The signed host runs under the hardened runtime with no Node on PATH: it starts, compiles its
    JavaScript and refuses a start that brings no bootstrap."""
    run('codesign', '--verify', '--strict', host)
    result = subprocess.run([str(host)], stdin=subprocess.DEVNULL, capture_output=True, text=True,
                            env={'PATH': '/usr/bin:/bin'}, timeout=60)
    if result.returncode != 1 or 'Alondra host startup refused' not in result.stdout + result.stderr:
        raise RuntimeError(f'The SDK host did not start under its signature: exit {result.returncode}, {(result.stdout + result.stderr).strip()[-300:]}')


# alondrad is a launchd agent: macOS shows its Local Network reason only from the Info.plist its
# build embeds and binds into the signature (crates/alondrad/build.rs, TN3179).
WITH_INFO_PLIST = {'com.bitomule.alondrad'}
# The launcher launchd runs carries alondrad's identity: macOS keeps the Local Network permission
# for that identity at the launcher's fixed path, and the privacy settings keep naming alondrad.
SIGNED = {'alondra': 'com.bitomule.alondra', 'alondrad': 'com.bitomule.alondrad', 'alondrad-launcher': 'com.bitomule.alondrad'}


def check_signature(details, identifier):
    if not all(value in details for value in (f'TeamIdentifier={TEAM}',
                'Authority=Developer ID Application:', 'Timestamp=')):
        raise RuntimeError('Unexpected signing identity/team or missing secure timestamp')
    if f'Identifier={identifier}\n' not in details + '\n':
        raise RuntimeError(f'Signed with another identifier than {identifier}')
    if identifier in WITH_INFO_PLIST and 'Info.plist entries=' not in details:
        raise RuntimeError(f'{identifier} lost its embedded Info.plist: macOS would not show why it needs Local Network')


def sign(binary, identifier, entitlements=None):
    run('codesign', '--force', '--options', 'runtime', '--timestamp', '--sign', IDENTITY,
        '--identifier', identifier, *(('--entitlements', entitlements) if entitlements else ()), binary)
    run('codesign', '--verify', '--strict', binary)
    details = subprocess.run(['codesign', '--display', '--verbose=4', str(binary)],
                             check=True, text=True, capture_output=True).stderr
    check_signature(details, identifier)
    granted = subprocess.run(['codesign', '--display', '--entitlements', ':-', str(binary)],
                             check=True, capture_output=True).stdout
    expected = plistlib.loads(entitlements.read_bytes()) if entitlements else {}
    if (plistlib.loads(granted) if granted.strip() else {}) != expected:
        raise RuntimeError(f'{identifier} carries other entitlements than {entitlements or "none"}')



def prepare(ver, source, output):
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise RuntimeError('Preparation requires this Apple Silicon Mac')
    # Check the destination volume before creating artifacts or starting tests/builds.
    require_disk_space(output)
    identities = run('security', 'find-identity', '-v', '-p', 'codesigning', capture=True)
    if not any(len(parts := line.split()) > 1 and parts[1] == IDENTITY for line in identities.splitlines()):
        raise RuntimeError('The approved Developer ID identity is not available in the local keychain')
    # Never overwrite prior archives, including partially prepared artifacts.
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    run(sys.executable, '-m', 'unittest', 'discover', '-s', 'deploy/brew/tests', '-v')
    run('bazelisk', 'test', '//crates/...')
    built = release_build(ver)
    for target in release.TARGETS:
        with tempfile.TemporaryDirectory(prefix='alondra-local-sign-') as temp:
            payload = pathlib.Path(temp)
            for name, identifier in SIGNED.items():
                shutil.copyfile(built[name], payload / name)
                (payload / name).chmod(0o755)
                sign(payload / name, identifier)
            shutil.copyfile(built['alondra-host'], payload / 'alondra-host')
            (payload / 'alondra-host').chmod(0o755)
            sign(payload / 'alondra-host', HOST_IDENTIFIER, HOST_ENTITLEMENTS)
            run('bash', 'deploy/brew/package-cli.sh', ver, target, payload / 'alondra', payload / 'alondrad',
                payload / 'alondrad-launcher', payload / 'alondra-host', built['alondra-host.json'], output)
        smoke(ver, target, output / f'alondra-{ver}-{target}.tar.gz')
    rehearse_upgrade(ver, output)
    (output / 'manifest.json').write_text(json.dumps(manifest(ver, source, output), indent=2) + '\n')
    verify_prepared(ver, source, output)
    print(f'Prepared immutable release artifacts: {output}')


def publish(ver, source, output, retry=False):
    verify_prepared(ver, source, output)
    # Repeat immediately before tag creation: the source or workflow may have changed during builds.
    if preflight(ver, retry_source=source if retry else None) != source:
        raise RuntimeError('Source changed during preparation')
    local, remote = tag_state(ver, source, retry=retry)
    tag = f'alondra-v{ver}'
    if not local:
        run('git', 'tag', '-a', tag, source, '-m', f'Alondra {ver}')
    if not remote:
        run('git', 'push', 'origin', f'refs/tags/{tag}')
    release.publish(ver, output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('version')
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--prepare', action='store_true', help='Build, verify and preserve artifacts without tagging or publishing')
    modes.add_argument('--retry-publish', action='store_true', help='Publish only the original prepared bytes; never rebuild or sign')
    args = parser.parse_args()
    ver = release.version(args.version)
    output = pathlib.Path(os.environ.get('ALONDRA_RELEASE_DIR', str(pathlib.Path.home() / 'Library/Caches/Alondra/releases' / ver))).expanduser().resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    with open(output.parent / (output.name + '.release.lock'), 'a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('Another local release of this version is running') from None
        retry_source = None
        if args.retry_publish:
            retry_source = json.loads((output / 'manifest.json').read_text())['source']
            verify_prepared(ver, retry_source, output)
        source = preflight(ver, retry_source=retry_source)
        tag_state(ver, source, args.retry_publish)
        if not args.retry_publish:
            prepare(ver, source, output)
        if not args.prepare:
            publish(ver, source, output, retry=args.retry_publish)


if __name__ == '__main__':
    try:
        main()
    except (RuntimeError, OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f'Local release stopped: {error}', file=sys.stderr)
        sys.exit(1)
