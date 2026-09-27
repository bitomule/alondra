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
            good = {'steps': [{'name': n, 'image': {'ok': True}} for n in ('clean-onboarding', 'demo-open', 'demo-reset', 'demo-exit')],
                    'crashes': [], 'invalid_step_count': 0, 'video_status': 'accepted'}
            (run / 'run.json').write_text(json.dumps({'status': 'passed', 'skipped': []}))
            for field, bad in [('video_status', 'invalid'), ('crashes', ['crash.ips']), ('invalid_step_count', 1), ('steps', [])]:
                (run / 'report.json').write_text(json.dumps({**good, field: bad}))
                with self.subTest(field=field), self.assertRaises(m.GateError):
                    m.validate_report(folder)
            (run / 'report.json').write_text(json.dumps(good))
            self.assertEqual(m.validate_report(folder)[0], run / 'report.json')
            (run / 'run.json').write_text(json.dumps({'status': 'passed', 'skipped': ['1:open']}))
            with self.assertRaises(m.GateError):
                m.validate_report(folder)

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

    def test_existing_output_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            (out / 'result.json').write_text('previous evidence')
            gate = m.Gate(argparse.Namespace(output_dir=out, base=m.BASE_NAME, profile='cli', resign_for_demo=False))
            with self.assertRaises(m.GateError):
                gate.execute()
            self.assertEqual((out / 'result.json').read_text(), 'previous evidence')


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
