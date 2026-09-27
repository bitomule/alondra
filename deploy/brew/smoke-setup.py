#!/usr/bin/env python3
"""Exercise installed CLI autodetection without a checkout, login or host mutations.

Claude/launchctl doubles expose only read-only observation commands. This checks
setup's proposed plan, not a real Claude registration or launchd installation.
"""
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile


def check_plan(output, prefix, smoke_home, explicit_profile=None):
    expected_daemon = f'<string>{prefix}/bin/alondrad</string>'
    expected_assets = f'would run: claude plugin marketplace add {prefix}/libexec/alondra'
    if expected_daemon not in output:
        raise RuntimeError('Setup did not discover the daemon through the installed opt path')
    if expected_assets not in output:
        raise RuntimeError('Setup did not discover packaged marketplace assets through opt')
    if '(dry run: nothing was written or run)' not in output:
        raise RuntimeError('Setup did not finish in dry-run mode')
    for key in ('CLAUDE_CONFIG_DIR', 'CLAUDE_HOME'):
        if explicit_profile is None:
            if f'<key>{key}</key>' in output:
                raise RuntimeError(f'Setup forced an implicit Claude profile: {key}')
        elif f'<key>{key}</key><string>{explicit_profile}</string>' not in output:
            raise RuntimeError(f'Setup lost the explicitly requested Claude profile: {key}')
    marker = '<key>ALONDRA_CLAUDE_PROFILE_MODE</key><string>explicit</string>'
    if (explicit_profile is None and '<key>ALONDRA_CLAUDE_PROFILE_MODE</key>' in output) or (explicit_profile is not None and marker not in output):
        raise RuntimeError('Setup did not preserve explicit profile provenance')
    if '/Cellar/' in output or 'make install' in output:
        raise RuntimeError('Setup leaked versioned keg paths or requested a source checkout')


def profile_snapshot(profile):
    # Existing fixture directories and Claude executable are allowed; no new
    # files or modified bytes/modes may appear during the dry run.
    snapshot = {}
    for path in profile.rglob('*'):
        if path.is_symlink():
            raise RuntimeError('Unexpected symlink in isolated setup profile')
        snapshot[str(path.relative_to(profile))] = (
            path.stat().st_mode,
            None if path.is_dir() else hashlib.sha256(path.read_bytes()).hexdigest(),
        )
    return snapshot


def check_observations(entries, smoke_home, explicit_profile=None):
    allowed = {
        ('claude', ('--version',)),
        ('claude', ('plugin', 'list', '--json')),
        ('claude', ('plugin', 'marketplace', 'list', '--json')),
    }
    saw_claude = False
    expected = [str(smoke_home), explicit_profile, explicit_profile]
    for name, args, profile in entries:
        if name == 'claude':
            saw_claude = True
            if profile != expected:
                raise RuntimeError('Claude observation used a different user/profile from the daemon')
        read_only_launchctl = name == 'launchctl' and len(args) == 2 and args[0] == 'print'
        if (name, tuple(args)) not in allowed and not read_only_launchctl:
            raise RuntimeError(f'Dry-run attempted a mutation: {name} {args}')
    if not saw_claude:
        raise RuntimeError('Setup did not observe the installed Claude executable')


def run_case(prefix, profile_name):
    with tempfile.TemporaryDirectory(prefix='alondra-setup-smoke-') as temporary:
        workspace = pathlib.Path(temporary)
        working_directory = workspace / 'outside-repository'
        smoke_home = workspace / 'user'
        observation_bin = workspace / 'observation-bin'
        for directory in (working_directory, smoke_home, observation_bin):
            directory.mkdir()
        calls = workspace / 'observations.jsonl'
        observer = '''#!{python}
import json, os, pathlib, sys
args = sys.argv[1:]
name = pathlib.Path(sys.argv[0]).name
with open({calls!r}, 'a') as log:
    profile = [os.environ.get(key) for key in ('HOME', 'CLAUDE_CONFIG_DIR', 'CLAUDE_HOME')]
    log.write(json.dumps([name, args, profile]) + '\\n')
if name == 'claude' and args == ['--version']:
    print('Claude Code (read-only setup smoke double)')
elif name == 'claude' and args in (['plugin', 'list', '--json'], ['plugin', 'marketplace', 'list', '--json']):
    print('[]')
elif name == 'launchctl' and len(args) == 2 and args[0] == 'print':
    sys.exit(1)  # fresh user: no service loaded
else:
    sys.exit(91)  # Any mutation is forbidden and diagnosed from the log.
'''.format(python=sys.executable, calls=str(calls))
        # PR176 resolves Claude using the daemon's own PATH, not shell PATH.
        # Keep launchctl in shell PATH; expose Claude in the fresh user's local bin.
        claude_bin = smoke_home / '.local/bin'
        claude_bin.mkdir(parents=True)
        for path in (claude_bin / 'claude', observation_bin / 'launchctl'):
            path.write_text(observer)
            path.chmod(0o755)
        # HOME is isolated only in the child environment; the invoking shell and
        # host profile are unchanged. Do not inherit live ALONDRA_* credentials.
        environment = {
            'HOME': str(smoke_home),
            'ALONDRA_HOME': str(smoke_home / '.alondra'),
            'ALONDRA_LANG': 'en',
            'PATH': f'{observation_bin}:/usr/bin:/bin:/usr/sbin:/sbin',
        }
        explicit_profile = str(smoke_home / profile_name) if profile_name is not None else None
        if explicit_profile is not None:
            # Both an explicit custom directory and explicitly chosen ~/.claude
            # must retain their identity; neither is an implicit default.
            environment['CLAUDE_CONFIG_DIR'] = explicit_profile
            environment['CLAUDE_HOME'] = explicit_profile
        before = profile_snapshot(smoke_home)
        result = subprocess.run(
            [str(prefix / 'bin/alondra'), 'setup', '--dry-run'],
            cwd=working_directory, env=environment, stdin=subprocess.DEVNULL,
            text=True, capture_output=True, timeout=30,
        )
        print(result.stdout, end='')
        if result.returncode:
            raise RuntimeError(f'Setup dry-run failed: {result.stderr}')
        check_plan(result.stdout, prefix, smoke_home, explicit_profile)
        if profile_snapshot(smoke_home) != before:
            raise RuntimeError('Setup dry-run wrote files in the isolated user profile')
        check_observations([json.loads(line) for line in calls.read_text().splitlines()], smoke_home, explicit_profile)
        label = 'implicit-unset' if profile_name is None else f'explicit-{profile_name}'
        print(f'Installed setup profile {label} passed read-only observations; no real login, registration or daemon start tested.')


def main():
    prefix = pathlib.Path(sys.argv[1]).absolute()  # Keep opt symlink spelling.
    if prefix.name != 'alondra' or prefix.parent.name != 'opt':
        raise RuntimeError('Pass the Homebrew opt/alondra prefix')
    for profile_name in (None, 'custom-profile', '.claude'):
        run_case(prefix, profile_name)



if __name__ == '__main__':
    main()
