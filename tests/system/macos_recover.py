#!/usr/bin/env python3
"""Opt-in recovery of one stopped evidence clone; never reruns its app or tests."""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time
import macos as m


def receipt(path):
    if path.is_symlink() or not path.is_file():
        raise m.GateError('regular recovery receipt required')
    value = json.loads(path.read_text())
    if value.get('schema') != 1 or value.get('scope') != 'alondra-tart-evidence-recovery':
        raise m.GateError('unexpected recovery contract')
    if not re.fullmatch('alondra-gate-[a-f0-9]{32}', value.get('vm', '')) or not re.fullmatch('[a-f0-9]{32}', value.get('owner_token', '')):
        raise m.GateError('invalid owned clone identity')
    original = Path(value['original_output'])
    ownership = original / 'ownership.json'
    if original != path.resolve().parent or m.digest(ownership) != value['ownership_sha256']:
        raise m.GateError('original ownership receipt changed')
    owned = json.loads(ownership.read_text())
    if any(owned.get(k) != value[k] for k in ('vm', 'owner_token', 'tart_home', 'source')):
        raise m.GateError('recovery ownership mismatch')
    if value.get('profile') not in ('app', 'cli', 'visual'):
        raise m.GateError('unexpected recovery profile')
    if value['profile'] == 'visual' and not re.fullmatch('[a-f0-9]{64}', value.get('input_executable_sha256') or ''):
        raise m.GateError('visual recovery requires original executable hash')
    if type(value.get('brew_lifecycle', False)) is not bool or (value.get('brew_lifecycle') and value['profile'] != 'cli'):
        raise m.GateError('invalid lifecycle recovery variant')
    return value


def recover(args):
    saved = receipt(args.receipt)
    home = Path(saved['tart_home']).resolve(strict=True)
    if home != Path(os.environ.get('TART_HOME', str(Path.home() / '.tart'))).resolve(strict=True):
        raise m.GateError('explicit original TART_HOME required')
    config = home / 'vms' / saved['vm'] / 'config.json'
    if config.is_symlink() or m.digest(config) != saved['config_sha256']:
        raise m.GateError('retained VM configuration changed')
    if args.output.exists() or not args.output.is_absolute():
        raise m.GateError('new absolute output required')
    args.output.mkdir(parents=True, mode=0o700)
    gate = m.Gate(argparse.Namespace(output_dir=args.output, base=m.BASE_NAME,
                  profile=saved['profile'], brew_version=saved['brew_version'],
                  brew_lifecycle=saved.get('brew_lifecycle', False)))
    gate.name = saved['vm']; gate.owner_token = saved['owner_token']; gate.tart_home = home
    gate.env['TART_HOME'] = str(home)
    gate.recovery_descriptor = saved.get('evidence_descriptor')
    gate.result.update(source=saved['source'], vm=gate.name, recovery_of=str(args.receipt), status='failed',
                       guest_failure=saved.get('guest_failure'),
                       input_executable_sha256=saved.get('input_executable_sha256'),
                       evidence_descriptor=saved.get('evidence_descriptor'))
    # Receipt ownership remains rooted in the original run, including on a failed retry.
    (gate.out / 'ownership.json').write_text((Path(saved['original_output']) / 'ownership.json').read_text())
    try:
        if min(m.shutil.disk_usage(home).free, m.shutil.disk_usage(gate.out).free) < m.MIN_FREE:
            raise m.GateError('8 GiB required before booting retained clone')
        gate.lock = open(home / '.alondra-gate.lock', 'a')
        m.fcntl.flock(gate.lock, m.fcntl.LOCK_EX | m.fcntl.LOCK_NB)
        entries = json.loads(gate.call('tart', 'list', '--format', 'json').stdout)
        own = [v for v in entries if v.get('Name') == gate.name and v.get('Source') == 'local']
        if len(own) != 1 or own[0].get('State') != 'stopped' or any(v.get('State') == 'running' for v in entries):
            raise m.GateError('only the stopped owned clone may be recovered')
        gate.created = True; gate.evidence_pending = True
        with (gate.out / 'tart-run.log').open('wb') as log:
            gate.process = subprocess.Popen(['tart', 'run', gate.name, '--no-graphics', '--no-audio', '--no-clipboard'], env=gate.env, stdout=log, stderr=log)
        deadline = time.monotonic() + 180
        while True:
            try:
                response = gate.guest('/bin/cat', m.GUEST + '/ownership.json', timeout=5, check=False)
                if response.returncode == 0:
                    if json.loads(response.stdout) != {'vm': gate.name, 'owner_token': gate.owner_token}:
                        raise m.GateError('guest ownership token mismatch')
                    break
            except subprocess.TimeoutExpired:
                pass
            if time.monotonic() >= deadline or gate.process.poll() is not None:
                raise m.GateError('retained guest unavailable')
            time.sleep(2)
        gate.step = 'recover-collection'
        gate.collect_evidence()
        gate.evidence_pending = False
        # Recovery cannot turn failed guest execution into acceptance PASS.
        if saved.get('guest_failure'):
            raise m.GateError('evidence recovered, original guest execution failed')
        if saved['profile'] == 'app':
            path, _ = m.validate_report(gate.out / 'evidence')
            gate.result['mav_report_sha256'] = m.digest(path)
        elif saved['profile'] == 'visual':
            gate.result['visual'] = m.validate_visual(gate.out / 'evidence')
            if gate.result['visual'].get('binarySHA256') != saved['input_executable_sha256']:
                raise m.GateError('visual guest ran a different executable')
        else:
            cli = json.loads((gate.out / 'evidence/project/cli-result.json').read_text())
            m.validate_cli(cli, saved['brew_version'], saved.get('brew_lifecycle', False))
            gate.result['cli'] = cli
        gate.result['status'] = 'recovered-validated-evidence'
    except Exception as error:
        gate.result.update(status='failed', failure={'kind': type(error).__name__, 'detail': str(error)})
    finally:
        for sig in (signal.SIGINT, signal.SIGTERM): signal.signal(sig, signal.SIG_IGN)
        gate.cleanup()
        (gate.out / 'result.json').write_text(json.dumps(gate.result, indent=2) + '\n')
    return gate.result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--receipt', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--run', action='store_true')
    args = parser.parse_args()
    if not args.run:
        print(json.dumps({'status': 'planned', 'starts_vm': False})); raise SystemExit(0)
    def interrupted(signum, frame): raise m.GateError('recovery interrupted')
    for sig in (signal.SIGINT, signal.SIGTERM): signal.signal(sig, interrupted)
    result = recover(args)
    print(json.dumps(result))
    raise SystemExit(0 if result['status'] == 'recovered-validated-evidence' else 1)
