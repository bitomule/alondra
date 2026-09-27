import argparse
import io
import importlib.util
import os
import sys
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import macos as m


class MacGateTests(unittest.TestCase):
    def test_default_is_plan_only_without_tooling(self):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / 'output'
            with patch('sys.argv', ['macos.py', '--output-dir', str(out)]), patch('subprocess.run') as run, patch('subprocess.Popen') as popen:
                self.assertEqual(m.main(), 0)
            run.assert_not_called()
            popen.assert_not_called()
            self.assertFalse(out.exists())

    def test_personal_and_registry_bases_are_rejected(self):
        for name in ('mav-macos', 'quill-screenshots-tahoe', '../alondra-test', 'ghcr.io/test', 'alondra-foo;touch bad'):
            with self.subTest(name=name), self.assertRaises(m.GateError):
                m.checked_name(name)

    def test_evidence_cannot_escape_or_create_symlinks(self):
        for kind in ('escape', 'link', 'oversized'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                archive = root / 'evidence.tar'
                with tarfile.open(archive, 'w') as tar:
                    member = tarfile.TarInfo('../outside' if kind == 'escape' else 'artifact')
                    if kind == 'link':
                        member.type = tarfile.SYMTYPE
                        member.linkname = '/etc/passwd'
                    elif kind == 'oversized':
                        member.type = tarfile.DIRTYPE
                        member.size = 513 * 1024**2
                    tar.addfile(member)
                with self.assertRaises(m.GateError):
                    m.extract_evidence(archive, root / 'output')
                self.assertFalse((root / 'outside').exists())

    def test_report_rejects_false_green_and_accepts_complete_evidence(self):
        with tempfile.TemporaryDirectory() as temp:
            folder = Path(temp)
            run = folder / 'project/.mav/runs/one'
            run.mkdir(parents=True)
            good = {'steps': [{'name': n, 'image': {'ok': True}, 'created_at': f'2026-09-27T10:00:{i * 10:02d}Z'} for i,n in enumerate(('clean-onboarding', 'demo-open', 'demo-reset', 'demo-exit'))],
                    'crashes': [], 'invalid_step_count': 0, 'video_status': 'accepted'}
            (run / 'run.json').write_text(json.dumps({'status': 'passed', 'skipped': []}))
            for i, identifier in enumerate(('demo.enter', 'demo.reset')):
                turn = run / f'video/turn-{i:05d}'
                turn.mkdir(parents=True)
                timestamp = m.datetime.fromisoformat(f'2026-09-27T10:00:{i * 10 + 5:02d}+00:00').timestamp()
                (turn / 'action.json').write_text(json.dumps({'tool': 'click', 'result_error': False,
                    'timestamp': str(timestamp), 'arguments': {'element_token': 'session:1'}}))
                (turn / 'before_state.json').write_text(json.dumps({'tree_markdown': f'- [1] AXButton (Demo) [id={identifier} actions=[press]]'}))
                (turn / 'after_state.json').write_text(json.dumps({'tree_markdown': 'AXStaticText = "Demo · Sample data"\n[1] AXButton (Reset demo)\n[2] AXButton (Exit demo)'}))
            raw = run / 'video/turn-00001/after_state.json'  # Only reset loses its banner below.
            good['steps'].append({'name': 'final', 'image': {'ok': True}})
            for field, bad in [('video_status', 'invalid'), ('crashes', ['crash.ips']), ('invalid_step_count', 1), ('steps', [])]:
                (run / 'report.json').write_text(json.dumps({**good, field: bad}))
                with self.subTest(field=field), self.assertRaises(m.GateError):
                    m.validate_report(folder)
            for bad_steps in (good['steps'][1:], good['steps'] + [good['steps'][0]],
                              good['steps'] + [{'name': 'unexpected', 'image': {'ok': True}}]):
                (run / 'report.json').write_text(json.dumps({**good, 'steps': bad_steps}))
                with self.assertRaises(m.GateError): m.validate_report(folder)
            (run / 'report.json').write_text(json.dumps({**good, 'steps': good['steps'][:-1]}))
            self.assertEqual(m.validate_report(folder)[0], run / 'report.json')
            (run / 'report.json').write_text(json.dumps(good))
            self.assertEqual(m.validate_report(folder)[0], run / 'report.json')
            original = raw.read_text()
            raw.write_text(json.dumps({'tree_markdown': '[1] AXButton (Reset demo)\n[2] AXButton (Exit demo)'}))
            with self.assertRaisesRegex(m.GateError, 'banner'): m.validate_report(folder)
            raw.write_text(original)
            (run / 'run.json').write_text(json.dumps({'status': 'passed', 'skipped': ['1:open']}))
            with self.assertRaises(m.GateError):
                m.validate_report(folder)

    def test_ax_evidence_survives_mav_cleanup_and_rejects_links(self):
        spec = importlib.util.spec_from_file_location('cua_session', Path(__file__).parent / 'macos/cua-session.py')
        adapter = importlib.util.module_from_spec(spec); spec.loader.exec_module(adapter)
        with tempfile.TemporaryDirectory() as temp:
            project = Path(temp)
            turn = project / '.mav/runs/run/video/turn-00001'
            turn.mkdir(parents=True)
            (turn / 'after_state.json').write_text('{"tree_markdown":"actual driver state"}')
            adapter.preserve_ax(project)
            import shutil
            shutil.rmtree(project / '.mav')
            saved = project / 'driver-ax/run/turn-00001/after_state.json'
            self.assertEqual(json.loads(saved.read_text())['tree_markdown'], 'actual driver state')
            turn.mkdir(parents=True)
            (turn / 'action.json').symlink_to(saved)
            with self.assertRaises(ValueError): adapter.preserve_ax(project)

    def test_ax_adapter_keeps_driver_error_when_preservation_fails(self):
        spec = importlib.util.spec_from_file_location('cua_session', Path(__file__).parent / 'macos/cua-session.py')
        adapter = importlib.util.module_from_spec(spec); spec.loader.exec_module(adapter)
        with patch('sys.argv', ['cua-driver', 'call', 'click', '{"element_token":"a:1"}']), \
             patch.object(adapter.subprocess, 'check_output', return_value='VirtualMac2,1'), \
             patch.object(adapter.Path, 'is_file', return_value=True), \
             patch.object(adapter.subprocess, 'run', return_value=subprocess.CompletedProcess([], 7)) as run, \
             patch.object(adapter, 'preserve_ax', side_effect=OSError('no space')):
            self.assertEqual(adapter.main(), 7)
            self.assertEqual(json.loads(run.call_args.args[0][-1])['element_token'], 'a:1')
            self.assertNotIn('stdout', run.call_args.kwargs)
            self.assertNotIn('stderr', run.call_args.kwargs)

    def test_partial_clone_failure_cleans_only_reserved_name(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            base = root / 'tart/vms' / m.BASE_NAME
            base.mkdir(parents=True)
            (base / 'config.json').write_text('{}')
            args = argparse.Namespace(output_dir=root / 'result', base=m.BASE_NAME, profile='cli', resign_for_demo=False)
            gate = m.Gate(args)
            calls = []
            def call(*argv, **kwargs):
                calls.append(argv)
                if argv[:2] == ('git', 'rev-parse'):
                    out = b'a'*40
                elif argv[:2] == ('tart', 'list'):
                    out = json.dumps([{'Name': m.BASE_NAME, 'Source': 'local', 'State': 'stopped'}]).encode()
                elif argv[:2] == ('tart', 'clone'):
                    raise subprocess.TimeoutExpired(argv, 90)
                else:
                    out = b''
                return subprocess.CompletedProcess(argv, 0, out, b'')
            with patch.dict('os.environ', {'TART_HOME': str(root / 'tart')}), patch.object(gate, 'call', call), patch.object(m.shutil, 'disk_usage', return_value=argparse.Namespace(free=m.MIN_FREE + 1)):
                result = gate.execute()
            self.assertEqual(result['status'], 'failed')
            self.assertTrue(result['cleanup']['owned_clone_deleted'])
            self.assertEqual(json.loads((root / 'result/ownership.json').read_text())['vm'], gate.name)
            self.assertIn(('tart', 'delete', gate.name), calls)
            self.assertNotIn(('tart', 'delete', m.BASE_NAME), calls)
            self.assertEqual(json.loads((root / 'result/result.json').read_text())['failure']['step'], 'clone')

    def test_app_store_bundle_is_rejected_before_execution(self):
        with tempfile.TemporaryDirectory() as temp:
            app = Path(temp) / 'Alondra.app'
            (app / 'Contents').mkdir(parents=True)
            (app / 'Contents/embedded.provisionprofile').write_text('synthetic profile')
            with self.assertRaisesRegex(m.GateError, 'Debug app'):
                m.validate_app(app)

    def transfer_fixture(self, root, mutate=None):
        guest_root = root / 'guest'
        project = guest_root / 'project'
        project.mkdir(parents=True)
        # Incompressible multi-chunk fixture exercises offsets and retry, not just a tiny TAR.
        body = os.urandom(170000)
        (project / 'payload.bin').write_bytes(body)
        out = root / 'output'
        out.mkdir()
        gate = m.Gate(argparse.Namespace(output_dir=out, base=m.BASE_NAME, profile='cli'))
        calls = []
        def guest(*args, **kwargs):
            action = args[2]
            offset = int(args[3]) if len(args) == 4 else None
            calls.append((action, offset))
            if action == 'prepare': value = transfer.prepare(guest_root)
            elif action == 'describe': value = transfer.descriptor(guest_root)
            elif action == 'chunk': value = transfer.chunk(guest_root, offset)
            else: value = transfer.diagnostics(guest_root)
            if mutate:
                value = mutate(action, offset, value, calls)
            return subprocess.CompletedProcess(args, 0, json.dumps(value).encode(), b'')
        return gate, guest, calls, body

    def test_chunk_retry_is_same_range_and_preserves_exact_archive(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            def drop(action, offset, value, calls):
                if action == 'chunk' and offset == 65536 and calls.count((action, offset)) == 1:
                    raise subprocess.TimeoutExpired('fixture transfer', 1)
                return value
            gate, guest, calls, body = self.transfer_fixture(root, drop)
            with patch.object(gate, 'guest', guest): gate.collect_evidence()
            self.assertEqual(calls.count(('chunk', 65536)), 2)
            self.assertEqual((gate.out / 'evidence/project/payload.bin').read_bytes(), body)
            self.assertFalse((gate.out / 'evidence.partial').exists())
            self.assertTrue((gate.out / 'guest-diagnostics.json').exists())

    def test_corruption_and_descriptor_mixing_never_publish(self):
        for kind in ('chunk_hash', 'global_hash', 'offset', 'oversize', 'changed_final'):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                def mutate(action, offset, value, calls):
                    if kind == 'oversize' and action == 'prepare': value['size'] = 545 * 1024**2
                    if kind == 'changed_final' and action == 'describe': value['sha256'] = '0' * 64
                    if action == 'chunk':
                        if kind == 'chunk_hash': value['sha256'] = '0' * 64
                        if kind == 'offset': value['offset'] += 1
                        if kind == 'global_hash':
                            data = m.base64.b64decode(value['data']);data = bytes([data[0] ^ 1]) + data[1:]
                            value['data'] = m.base64.b64encode(data).decode()
                            value['sha256'] = m.hashlib.sha256(data).hexdigest()
                    return value
                gate, guest, calls, body = self.transfer_fixture(root, mutate)
                with patch.object(gate, 'guest', guest), self.assertRaises(m.GateError): gate.collect_evidence()
                self.assertFalse((gate.out / 'evidence.tar').exists())
                self.assertFalse((gate.out / 'evidence').exists())
                if kind != 'oversize': self.assertTrue((gate.out / 'evidence.partial').exists())
                self.assertLessEqual(calls.count(('chunk', 0)), 3)

    def test_collection_deadline_and_frozen_file_change_fail_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            gate, guest, _, _ = self.transfer_fixture(root)
            with patch.object(gate, 'guest', guest), patch.object(m.time, 'monotonic', side_effect=[0, 301, 302]):
                with self.assertRaisesRegex(m.GateError, '300 seconds'): gate.collect_evidence()
            self.assertFalse((gate.out / 'evidence.tar').exists())
            transfer.prepare(root / 'guest')
            with (root / 'guest/evidence.tar').open('ab') as stream: stream.write(b'mutation')
            with self.assertRaisesRegex(ValueError, 'changed'): transfer.chunk(root / 'guest', 0)

    def test_guest_freeze_rejects_links_and_diagnostics_are_bounded(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp);(root/'project').mkdir()
            (root/'project/flow.txt').write_text('X' * 100000)
            value=transfer.diagnostics(root)['files']['project/flow.txt']
            self.assertTrue(value['truncated']);self.assertEqual(len(value['tail']),3072)
            (root/'project/leak').symlink_to(root/'project/flow.txt')
            with self.assertRaisesRegex(ValueError, 'unsupported'): transfer.prepare(root)

    def test_guest_timeout_and_stop_timeout_still_attempt_collection(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            base = root / 'tart/vms' / m.BASE_NAME
            base.mkdir(parents=True)
            (base / 'config.json').write_text('{}')
            gate = m.Gate(argparse.Namespace(output_dir=root / 'output', base=m.BASE_NAME,
                                            profile='cli', brew_version='0.1.8'))
            sequence = []
            def call(*args, **kwargs):
                data = b''
                if args[:2] == ('git', 'rev-parse'): data = b'a' * 40
                if args[:2] == ('tart', 'list'):
                    data = json.dumps([{'Name': m.BASE_NAME, 'Source': 'local', 'State': 'stopped'}]).encode()
                if args[:2] == ('/usr/bin/tar', '-cf'): Path(args[2]).write_bytes(b'fixture')
                return subprocess.CompletedProcess(args, 0, data, b'')
            def guest(*args, **kwargs):
                if args[0] == '/bin/cat':
                    return subprocess.CompletedProcess(args, 0, json.dumps({'origin': m.BASE_ORIGIN, 'synthetic': True}).encode(), b'')
                if args[:2] == ('/bin/bash', m.GUEST + '/guest.sh'):
                    sequence.append('guest-timeout'); raise subprocess.TimeoutExpired(args, 600)
                if args[:2] == ('/bin/bash', '-c'):
                    sequence.append('stop-timeout'); raise subprocess.TimeoutExpired(args, 30)
                return subprocess.CompletedProcess(args, 0, b'', b'')
            fake_process = unittest.mock.Mock()
            fake_process.poll.return_value = None
            with patch.dict(os.environ, {'TART_HOME': str(root / 'tart')}), \
                    patch.object(gate, 'call', call), patch.object(gate, 'guest', guest), \
                    patch.object(gate, 'collect_evidence', side_effect=lambda: sequence.append('collect')), \
                    patch.object(m.subprocess, 'Popen', return_value=fake_process), \
                    patch.object(m.signal, 'signal'), \
                    patch.object(m.shutil, 'disk_usage', return_value=argparse.Namespace(free=m.MIN_FREE + 1)):
                result = gate.execute()
            self.assertEqual(sequence, ['guest-timeout', 'stop-timeout', 'collect'])
            self.assertEqual(result['guest_failure'], {'timeout_seconds': 600})
            self.assertEqual(result['timeout_mav_stop_error']['kind'], 'TimeoutExpired')
            self.assertEqual(result['status'], 'failed')
            self.assertTrue(result['cleanup']['owned_clone_deleted'])

    def test_existing_output_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            (out / 'result.json').write_text('previous evidence')
            gate = m.Gate(argparse.Namespace(output_dir=out, base=m.BASE_NAME, profile='cli', resign_for_demo=False))
            with self.assertRaises(m.GateError):
                gate.execute()
            self.assertEqual((out / 'result.json').read_text(), 'previous evidence')


transfer_spec = importlib.util.spec_from_file_location('evidence_transfer', Path(m.__file__).parent / 'macos/evidence-transfer.py')
transfer = importlib.util.module_from_spec(transfer_spec)
transfer_spec.loader.exec_module(transfer)

spec = importlib.util.spec_from_file_location('cli_gate', Path(m.__file__).parent / 'macos/cli.py')
cli_gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cli_gate)

class CLIGuestTests(unittest.TestCase):
 def test_plan_requires_installed_opt_daemon(self):
  prefix=Path('/opt/homebrew/opt/alondra')
  valid=f'<string>{prefix}/bin/alondrad</string>\n(dry run: nothing was written or run)'
  cli_gate.check_plan(valid,prefix)
  for bad in [valid.replace(str(prefix),'/Users/admin/.local'),valid+'\nmake install',valid+'\n/opt/homebrew/Cellar/alondra']:
   with self.assertRaises(RuntimeError):cli_gate.check_plan(bad,prefix)
 def test_guest_network_requires_explicit_down(self):
  cli_gate.check_guest_network_disabled('en0: flags=8862<BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500')
  for bad in ['en0: flags=8863<UP,BROADCAST,RUNNING> mtu 1500','lo0: flags=8049<UP,LOOPBACK> mtu 16384','']:
   with self.assertRaises(RuntimeError):cli_gate.check_guest_network_disabled(bad)
 def test_tree_sees_empty_directories_and_rejects_symlinks(self):
  with tempfile.TemporaryDirectory() as raw:
   root=Path(raw); first=cli_gate.tree(root);(root/'empty').mkdir()
   self.assertNotEqual(first,cli_gate.tree(root));(root/'link').symlink_to(root/'empty')
   with self.assertRaises(RuntimeError):cli_gate.tree(root)
 def test_command_success_and_failure(self):
  with tempfile.TemporaryDirectory() as raw:
   root=Path(raw)
   self.assertEqual(cli_gate.command([sys.executable,'-c','print("fixture")'],dict(os.environ),root/'ok.log'), 'fixture\n')
   with self.assertRaisesRegex(RuntimeError,'command failed'):
    cli_gate.command([sys.executable,'-c','raise SystemExit(2)'],dict(os.environ),root/'bad.log')
 def test_command_timeout_cleans_child(self):
  with tempfile.TemporaryDirectory() as raw:
   with self.assertRaisesRegex(RuntimeError,'timeout'):
    cli_gate.command([sys.executable,'-c','import time;time.sleep(20)'],dict(os.environ),Path(raw)/'timeout.log',timeout=.05)
 def test_output_bound(self):
  with tempfile.TemporaryDirectory() as raw:
   with self.assertRaisesRegex(RuntimeError,'output bound'):
    cli_gate.command([sys.executable,'-c','import sys;sys.stdout.write("a"*(5*1024*1024))'],dict(os.environ),Path(raw)/'huge.log')
   self.assertLessEqual((Path(raw)/'huge.log').stat().st_size,cli_gate.MAX_LOG)
 def test_latest_screen_replaces_transient_offline(self):
  frame=cli_gate.final_screen(b'\x1b[2J\x1b[Hno responde\x1b[2J\x1b[H0 trabajando\x1b[45;1HLocal')
  self.assertNotIn('no responde',frame);self.assertIn('0 trabajando',frame);self.assertIn('Local',frame)

if __name__ == '__main__':
    unittest.main()
