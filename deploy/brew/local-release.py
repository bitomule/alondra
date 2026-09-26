#!/usr/bin/env python3
"""On-demand local release; never exports signing keys or starts hosted jobs."""
import argparse
import fcntl
import json
import os
import pathlib
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

import release

ROOT = pathlib.Path(__file__).resolve().parents[2]
IDENTITY = '137E48D7AF54AAA3DFC56B9820DACC897C20AF15'
TEAM = '658YBSK6MG'
MIN_FREE = 8 * 1024**3


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
    if retry_source is None and run('git', 'branch', '--show-current', capture=True).strip() != 'main':
        raise RuntimeError('Release from main only')
    if run('git', 'status', '--porcelain', capture=True).strip():
        raise RuntimeError('Working tree must be clean')
    run('git', 'fetch', 'origin', 'main')
    source = run('git', 'rev-parse', 'HEAD', capture=True).strip()
    if retry_source is not None:
        if source != retry_source:
            raise RuntimeError('Retry from a clean checkout of the original manifest source commit')
        run('git', 'merge-base', '--is-ancestor', 'HEAD', 'origin/main')
    elif source != run('git', 'rev-parse', 'origin/main', capture=True).strip():
        raise RuntimeError('main must equal origin/main')
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


def disk_guard(path):
    ancestor = path
    while not ancestor.exists():
        ancestor = ancestor.parent
    if shutil.disk_usage(ancestor).free < MIN_FREE:
        raise RuntimeError(f'At least 8 GiB free is required on the volume containing {path}')


def manifest(ver, source, output):
    return {'schema': 1, 'version': ver, 'source': source,
            'validation': 'ARM native and Intel under Rosetta; no physical Intel verification',
            'archives': {f'alondra-{ver}-{target}.tar.gz': release.sha(output / f'alondra-{ver}-{target}.tar.gz')
                         for target in release.TARGETS}}


def verify_prepared(ver, source, output):
    saved = json.loads((output / 'manifest.json').read_text())
    if saved != manifest(ver, source, output):
        raise RuntimeError('Prepared manifest/source/version/checksums differ; never rebuild a published version')
    release.render(ver, output)  # Same strict archive allowlist as the publisher.


def smoke(ver, target, archive):
    release.validate_archive(archive)
    with tempfile.TemporaryDirectory(prefix='alondra-local-install-') as temp:
        prefix = pathlib.Path(temp)
        unpack = prefix / 'unpack'
        unpack.mkdir()
        with tarfile.open(archive) as tar:
            tar.extractall(unpack, filter='data')
        keg = prefix / 'Cellar/alondra' / ver
        binaries = keg / 'bin'
        assets = keg / 'libexec/alondra'
        binaries.mkdir(parents=True)
        assets.mkdir(parents=True)
        for name in ('alondra', 'alondrad'):
            shutil.move(unpack / name, binaries / name)
        for name in ('.claude-plugin', 'plugin', 'plugin-orchestrator'):
            shutil.move(unpack / name, assets / name)
        opt = prefix / 'opt/alondra'
        opt.parent.mkdir()
        opt.symlink_to(keg, target_is_directory=True)
        arch = 'arm64' if target.startswith('aarch64') else 'x86_64'
        for name in ('alondra', 'alondrad'):
            binary = binaries / name
            if run('lipo', '-archs', binary, capture=True).strip() != arch:
                raise RuntimeError(f'Wrong architecture for {name}: expected {arch}')
            run('codesign', '--verify', '--strict', binary)
            run('/usr/bin/arch', f'-{arch}', binary, '--help')
        # The Intel CLI runs via Rosetta automatically; this is not a physical Intel test.
        run(sys.executable, ROOT / 'deploy/brew/smoke-setup.py', opt)


def sign(binary, identifier):
    run('codesign', '--force', '--options', 'runtime', '--timestamp', '--sign', IDENTITY,
        '--identifier', identifier, binary)
    run('codesign', '--verify', '--strict', binary)
    details = subprocess.run(['codesign', '--display', '--verbose=4', str(binary)],
                             check=True, text=True, capture_output=True).stderr
    if not all(value in details for value in (f'TeamIdentifier={TEAM}',
                'Authority=Developer ID Application:', 'Timestamp=')):
        raise RuntimeError('Unexpected signing identity/team or missing secure timestamp')


def prepare(ver, source, output, target_dir):
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        raise RuntimeError('Preparation requires this Apple Silicon Mac with Rosetta')
    if run('/usr/bin/arch', '-x86_64', 'uname', '-m', capture=True).strip() != 'x86_64':
        raise RuntimeError('Rosetta must be installed before preparing the Intel package')
    installed = run('rustup', 'target', 'list', '--installed', capture=True).splitlines()
    if not set(release.TARGETS).issubset(installed):
        raise RuntimeError('Install both Rust Apple targets before release: rustup target add aarch64-apple-darwin x86_64-apple-darwin')
    identities = run('security', 'find-identity', '-v', '-p', 'codesigning', capture=True)
    if not any(len(parts := line.split()) > 1 and parts[1] == IDENTITY for line in identities.splitlines()):
        raise RuntimeError('The approved Developer ID identity is not available in the local keychain')
    disk_guard(target_dir)
    disk_guard(output)
    # Never overwrite prior archives, including partially prepared artifacts.
    output.mkdir(parents=True, exist_ok=False)
    output.chmod(0o700)
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('ALONDRA_', 'CLAUDE_'))}
    env.update(CARGO_TARGET_DIR=str(target_dir), CARGO_BUILD_JOBS='1', CARGO_INCREMENTAL='0',
               CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0', CARGO_PROFILE_RELEASE_DEBUG='0',
               MACOSX_DEPLOYMENT_TARGET='14.0')
    run(sys.executable, '-m', 'unittest', 'discover', '-s', 'deploy/brew/tests', '-v')
    for target in release.TARGETS:
        disk_guard(target_dir)
        with tempfile.TemporaryDirectory(prefix='alondra-release-test-home-') as home:
            test_env = dict(env, HOME=home,
                            CARGO_HOME=os.environ.get('CARGO_HOME', str(pathlib.Path.home() / '.cargo')),
                            RUSTUP_HOME=os.environ.get('RUSTUP_HOME', str(pathlib.Path.home() / '.rustup')))
            run('cargo', 'test', '--locked', '--target', target, '--jobs', '1',
                '-p', 'alondra', '-p', 'alondrad', '-p', 'alondra-e2e', env=test_env)
        disk_guard(target_dir)
        run('cargo', 'build', '--locked', '--release', '--target', target, '--jobs', '1',
            '-p', 'alondra', '-p', 'alondrad', env=env)
        with tempfile.TemporaryDirectory(prefix='alondra-local-sign-') as temp:
            payload = pathlib.Path(temp)
            for name in ('alondra', 'alondrad'):
                shutil.copy2(target_dir / target / 'release' / name, payload / name)
                sign(payload / name, f'com.bitomule.{name}')
            run('bash', 'deploy/brew/package-cli.sh', ver, target, payload / 'alondra', payload / 'alondrad', output)
        smoke(ver, target, output / f'alondra-{ver}-{target}.tar.gz')
    (output / 'manifest.json').write_text(json.dumps(manifest(ver, source, output), indent=2) + '\n')
    verify_prepared(ver, source, output)
    print(f'Prepared immutable release artifacts: {output}')
    print('Intel validation ran under Rosetta, not on physical Intel hardware.')


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
    target = pathlib.Path(os.environ.get('CARGO_TARGET_DIR', '/private/tmp/alondra-service-crypto/target')).expanduser().resolve()
    target.parent.mkdir(parents=True, exist_ok=True)
    with open(target.parent / (target.name + '.release.lock'), 'a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('Another local release is using this target directory') from None
        retry_source = None
        if args.retry_publish:
            retry_source = json.loads((output / 'manifest.json').read_text())['source']
            verify_prepared(ver, retry_source, output)
        source = preflight(ver, retry_source=retry_source)
        tag_state(ver, source, args.retry_publish)
        if not args.retry_publish:
            prepare(ver, source, output, target)
        if not args.prepare:
            publish(ver, source, output, retry=args.retry_publish)


if __name__ == '__main__':
    try:
        main()
    except (RuntimeError, OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f'Local release stopped: {error}', file=sys.stderr)
        sys.exit(1)
