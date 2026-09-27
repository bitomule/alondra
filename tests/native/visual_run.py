#!/usr/bin/env python3
"""Opt-in Debug visual acceptance, using real views/reducers and synthetic services."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import signal
import subprocess
import sys
import uuid

from visual import driver

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('native_common', Path(__file__).with_name('run.py'))
common = importlib.util.module_from_spec(spec)
spec.loader.exec_module(common)
BUNDLE = 'com.bitomule.alondra.visualharness'


def build(commands, output, cache, platform):
    target = 'AlondraVisualiOS' if platform == 'ios' else 'AlondraVisualMac'
    label = '//apple:' + target
    flags = ['-c', 'dbg', '--jobs=1', '--local_resources=cpu=1', '--remote_cache=', '--bes_backend=', '--lockfile_mode=error']
    if platform == 'ios':
        flags += ['--ios_multi_cpus=sim_arm64', '--apple_platform_type=ios']
    startup = common.cache_startup(cache)
    commands.run(startup + ['build', label] + flags, timeout=1800)
    suffix = '.ipa' if platform == 'ios' else '.zip'
    files = commands.run(startup + ['cquery', label, '--output=files'] + flags)
    archives = [Path(line) for line in files.splitlines() if line.startswith('bazel-out/') and line.endswith('/' + target + suffix)]
    roots = [Path(line) for line in commands.run(startup + ['info', 'execution_root'] + flags).splitlines() if line.startswith('/')]
    if len(archives) != 1 or len(roots) != 1 or '..' in archives[0].parts:
        raise ValueError('ambiguous visual artifact')
    app = common.extract_app(roots[0] / archives[0], output / 'artifact', platform)
    info = plistlib.loads((app / ('Info.plist' if platform == 'ios' else 'Contents/Info.plist')).read_bytes())
    if info.get('CFBundleIdentifier') != BUNDLE:
        raise ValueError('unexpected visual bundle')
    executable = app / ('' if platform == 'ios' else 'Contents/MacOS') / info['CFBundleExecutable']
    if not executable.is_file() or executable.is_symlink():
        raise ValueError('missing visual executable')
    return app, executable


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true')
    parser.add_argument('--platform', required=True, choices=('ios', 'mac'))
    parser.add_argument('--build-only', action='store_true')
    parser.add_argument('--scenario', choices=(*driver.CHECKS, 'all'), default='all')
    parser.add_argument('--runtime')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--build-cache', type=Path, default=Path.home() / 'Library/Caches/Alondra/visual-harness')
    args = parser.parse_args(argv)
    scenarios = list(driver.CHECKS) if args.scenario == 'all' else [args.scenario]
    if not args.run:
        print(json.dumps({'status': 'planned', 'platform': args.platform, 'scenarios': scenarios, 'build': 'dbg', 'testonly': True, 'starts': False}))
        return 0
    if args.platform == 'mac' and not args.build_only:
        parser.error('Mac visual runtime requires the owned Tart profile; use --build-only to prepare its artifact')
    if args.platform == 'ios' and not args.build_only and not args.runtime:
        parser.error('choose an installed --runtime explicitly')
    if args.output is None or not args.output.is_absolute() or args.output.exists() or not args.build_cache.is_absolute():
        parser.error('choose a new absolute --output and an absolute --build-cache')
    if subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT, text=True):
        parser.error('commit source before execution')
    if min(common.free_bytes(args.output), common.free_bytes(args.build_cache)) < 8 * 1024**3:
        parser.error('8 GiB free required on output and cache volumes')
    os.umask(0o077)
    args.output.mkdir(mode=0o700)
    output = args.output.resolve()
    commands = common.Commands(output)
    run_id = str(uuid.uuid4())
    manifest = {'schema': 1, 'status': 'running', 'scope': 'visual-native-fixture', 'runID': run_id,
                'platform': args.platform, 'scenarioReports': [], 'cleanup': 'pending', 'buildConfiguration': 'dbg'}
    common.atomic_json(output / 'manifest.json', manifest)
    udid = None; reserved_name = None; lock = None
    old_signals = {s: signal.getsignal(s) for s in (signal.SIGINT, signal.SIGTERM)}
    for s in old_signals: signal.signal(s, common.interrupted)
    try:
        manifest['source'] = commands.run(['git', 'rev-parse', 'HEAD'])
        manifest['dirty'] = False
        manifest['mavVersion'] = commands.run(['mav', '--version'])
        lock = common.acquire_cache(args.build_cache)
        app, executable = build(commands, output, args.build_cache, args.platform)
        manifest.update(appPath=str(app), binarySHA256=common.hash_file(executable))
        common.atomic_json(output / 'manifest.json', manifest)
        if not args.build_only:
            reserved_name = 'Alondra-native-' + run_id
            manifest['ownedSimulatorName'] = reserved_name
            common.atomic_json(output / 'manifest.json', manifest)
            udid = commands.run(['xcrun', 'simctl', 'create', reserved_name, 'com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro', args.runtime])
            uuid.UUID(udid)
            manifest['ownedSimulator'] = udid
            common.atomic_json(output / 'manifest.json', manifest)
            commands.run(['xcrun', 'simctl', 'boot', udid])
            commands.run(['xcrun', 'simctl', 'bootstatus', udid, '-b'], timeout=180)
            commands.run(['xcrun', 'simctl', 'install', udid, str(app)])
            container = Path(commands.run(['xcrun', 'simctl', 'get_app_container', udid, BUNDLE, 'data'])) / 'Documents'
            for scenario in scenarios:
                scenario_id = str(uuid.uuid4())
                project = output / scenario
                (project / '.mav').mkdir(parents=True)
                config = {'project_name': 'Alondra visual fixture', 'bundle_id': BUNDLE, 'process_name': 'AlondraVisualHarness',
                          'target_kind': 'simulator', 'simulator_udid': udid,
                          'launch': {'mode': 'custom', 'commands': {'build': 'false', 'app_path': "printf '%s' " + __import__('shlex').quote(str(app)), 'install': '', 'launch': ''}}}
                # JSON is valid YAML, avoids quoting synthetic paths manually.
                (project / '.mav/config.yaml').write_text(json.dumps(config))
                result_file = container / 'visual-result.json'
                control_file = container / 'visual-control.json'
                for path in (result_file, control_file):
                    if path.exists(): path.unlink()
                env = common.base_environment()
                env.update(SIMCTL_CHILD_ALONDRA_VISUAL_RUN_ID=scenario_id, SIMCTL_CHILD_ALONDRA_VISUAL_SCENARIO=scenario,
                           MAV_TARGET_KIND='simulator', MAV_TARGET_UDID=udid, MAV_RUN_DIR=str(project / 'mav-run'))
                commands.run(['xcrun', 'simctl', 'launch', '--terminate-running-process', udid, BUNDLE,
                              '-AppleLanguages', '(en)', '-AppleLocale', 'en_US'], env=env)
                def mav(*arguments):
                    return commands.run(['mav', *arguments], env=env, cwd=project, timeout=60)
                def read():
                    if not result_file.is_file(): return None
                    return json.loads(result_file.read_text())
                report = driver.exercise(mav, read, lambda value: common.atomic_json(control_file, value), project / 'assertions', scenario_id, scenario)
                manifest['scenarioReports'].append({'scenario': scenario, 'runID': scenario_id, 'status': report['status'],
                    'sha256': common.hash_file(project / 'assertions/result.json')})
                common.atomic_json(output / 'manifest.json', manifest)
        manifest['status'] = 'built' if args.build_only else 'passed'
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        manifest.update(status='failed', error=str(error))
    finally:
        for s in old_signals: signal.signal(s, signal.SIG_IGN)
        try:
            errors = common.cleanup_owned(commands, udid, args.build_cache, lock, False, reserved_name)
            manifest['cleanup'] = 'failed' if errors else 'complete'
            if errors: manifest.update(status='failed', cleanupErrors=errors)
            common.atomic_json(output / 'manifest.json', manifest)
        finally:
            for s, old in old_signals.items(): signal.signal(s, old)
    print(json.dumps({'status': manifest['status'], 'manifest': str(output / 'manifest.json')}))
    return 0 if manifest['status'] in ('built', 'passed') else 1


if __name__ == '__main__':
    raise SystemExit(main())
