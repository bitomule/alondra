import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import macos as m
import macos_recover as recovery


class RecoveryTests(unittest.TestCase):
    def fixture(self, root):
        root = root.resolve()
        out = root / 'original'; out.mkdir()
        gate = m.Gate(argparse.Namespace(output_dir=out, base=m.BASE_NAME, profile='app'))
        gate.tart_home = root / 'tart'
        vm = gate.tart_home / 'vms' / gate.name; vm.mkdir(parents=True)
        (vm / 'config.json').write_text('{}')
        (out / 'ownership.json').write_text(json.dumps({'vm': gate.name, 'owner_token': gate.owner_token,
            'tart_home': str(gate.tart_home), 'source': None}))
        gate.created = True; gate.evidence_pending = True
        calls = []
        def call(*args, **kwargs):
            calls.append(args)
            data = json.dumps([{'Name': gate.name, 'Source': 'local', 'State': 'stopped'}]).encode()
            return subprocess.CompletedProcess(args, 0, data, b'')
        return gate, calls, call

    def test_failed_collection_stops_but_preserves_owned_clone_and_receipt(self):
        with tempfile.TemporaryDirectory() as temp:
            gate, calls, call = self.fixture(Path(temp))
            with patch.object(gate, 'call', call): gate.cleanup()
            self.assertIn(('tart', 'stop', gate.name), calls)
            self.assertFalse(any(c[:2] == ('tart', 'delete') for c in calls))
            value = recovery.receipt(gate.out / 'recovery.json')
            self.assertEqual(value['owner_token'], gate.owner_token)
            self.assertTrue(gate.result['cleanup']['evidence_retained'])
            self.assertEqual(gate.result['status'], 'failed')
            # Altered original ownership cannot redirect recovery to another clone.
            (gate.out / 'ownership.json').write_text('{}')
            with self.assertRaisesRegex(m.GateError, 'ownership receipt changed'):
                recovery.receipt(gate.out / 'recovery.json')

    def test_verified_collection_deletes_only_owned_clone(self):
        with tempfile.TemporaryDirectory() as temp:
            gate, calls, call = self.fixture(Path(temp)); gate.evidence_pending = False
            with patch.object(gate, 'call', call): gate.cleanup()
            self.assertIn(('tart', 'delete', gate.name), calls)
            self.assertTrue(gate.result['cleanup']['owned_clone_deleted'])
            self.assertFalse((gate.out / 'recovery.json').exists())

    def test_recovery_config_drift_rejected_before_boot(self):
        with tempfile.TemporaryDirectory() as temp:
            gate, calls, call = self.fixture(Path(temp))
            with patch.object(gate, 'call', call): gate.cleanup()
            (gate.tart_home / 'vms' / gate.name / 'config.json').write_text('{"changed":true}')
            with patch.dict('os.environ', {'TART_HOME': str(gate.tart_home)}), patch.object(m.subprocess, 'Popen') as boot:
                with self.assertRaisesRegex(m.GateError, 'configuration changed'):
                    recovery.recover(argparse.Namespace(receipt=gate.out / 'recovery.json', output=Path(temp) / 'retry'))
            boot.assert_not_called()

    def test_retry_validates_guest_owner_and_never_promotes_failed_execution(self):
        for mode in ('success', 'wrong-guest', 'failed-guest'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temp:
                gate, calls, call = self.fixture(Path(temp))
                if mode == 'failed-guest': gate.result['guest_failure'] = {'exit_code': 1}
                with patch.object(gate, 'call', call): gate.cleanup()
                def recover_call(instance, *args, **kwargs): return call(*args, **kwargs)
                def guest(instance, *args, **kwargs):
                    return subprocess.CompletedProcess(args, 0, json.dumps({'vm': gate.name,
                        'owner_token': 'bad' if mode == 'wrong-guest' else gate.owner_token}).encode(), b'')
                def collect(instance):
                    (instance.out / 'report.json').write_text('{}')
                with patch.dict('os.environ', {'TART_HOME': str(gate.tart_home)}), \
                     patch.object(m.shutil, 'disk_usage', return_value=argparse.Namespace(free=m.MIN_FREE * 2)), \
                     patch.object(m.Gate, 'call', recover_call), patch.object(m.Gate, 'guest', guest), \
                     patch.object(m.Gate, 'collect_evidence', collect), patch.object(m.subprocess, 'Popen'), \
                     patch.object(m, 'validate_report', return_value=(Path(temp)/'retry/report.json', {})):
                    result = recovery.recover(argparse.Namespace(receipt=gate.out/'recovery.json', output=Path(temp)/'retry'))
                self.assertEqual(result['status'], 'recovered-validated-evidence' if mode == 'success' else 'failed')
                self.assertEqual(result['cleanup']['owned_clone_deleted'], mode != 'wrong-guest')
                self.assertEqual(json.loads((gate.out/'recovery.json').read_text())['guest_failure'],
                    {'exit_code':1} if mode == 'failed-guest' else None)

    def test_two_retries_preserve_original_failure_and_frozen_archive_pin(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            gate, calls, call = self.fixture(root)
            descriptor = {'schema': 1, 'size': 10, 'chunk_bytes': 65536, 'sha256': 'a' * 64}
            gate.result.update(guest_failure={'exit_code': 9}, evidence_descriptor=descriptor)
            with patch.object(gate, 'call', call): gate.cleanup()
            def recover_call(instance, *args, **kwargs): return call(*args, **kwargs)
            def guest(instance, *args, **kwargs):
                return subprocess.CompletedProcess(args, 0, json.dumps({'vm': gate.name, 'owner_token': gate.owner_token}).encode(), b'')
            def failed_collect(instance):
                self.assertEqual(instance.recovery_descriptor, descriptor)
                raise m.GateError('transport failed before prepare')
            def collected(instance):
                self.assertEqual(instance.recovery_descriptor, descriptor)
            with patch.dict('os.environ', {'TART_HOME': str(gate.tart_home)}), \
                 patch.object(m.shutil, 'disk_usage', return_value=argparse.Namespace(free=m.MIN_FREE * 2)), \
                 patch.object(m.Gate, 'call', recover_call), patch.object(m.Gate, 'guest', guest), \
                 patch.object(m.subprocess, 'Popen'):
                with patch.object(m.Gate, 'collect_evidence', failed_collect):
                    first = recovery.recover(argparse.Namespace(receipt=gate.out/'recovery.json', output=root/'retry1'))
                self.assertEqual(first['status'], 'failed')
                next_receipt = root/'retry1/recovery.json'
                persisted = recovery.receipt(next_receipt)
                self.assertEqual(persisted['guest_failure'], {'exit_code': 9})
                self.assertEqual(persisted['evidence_descriptor'], descriptor)
                with patch.object(m.Gate, 'collect_evidence', collected), patch.object(m, 'validate_report') as validate:
                    second = recovery.recover(argparse.Namespace(receipt=next_receipt, output=root/'retry2'))
                self.assertEqual(second['status'], 'failed')
                self.assertEqual(second['guest_failure'], {'exit_code': 9})
                self.assertEqual(second['evidence_descriptor'], descriptor)
                self.assertIn('original guest execution failed', second['failure']['detail'])
                validate.assert_not_called()

    def test_visual_recovery_checks_original_binary_and_cannot_promote_guest_failure(self):
        for mode in ('success', 'wrong-binary', 'invalid-report', 'failed-guest', 'retry-before-collection'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temp:
                root = Path(temp).resolve()
                gate, calls, call = self.fixture(root)
                gate.args.profile = 'visual'
                pin = 'b' * 64
                gate.result['input_executable_sha256'] = pin
                if mode == 'failed-guest': gate.result['guest_failure'] = {'exit_code': 1}
                with patch.object(gate, 'call', call): gate.cleanup()
                def recover_call(instance, *args, **kwargs): return call(*args, **kwargs)
                def guest(instance, *args, **kwargs):
                    return subprocess.CompletedProcess(args, 0, json.dumps({'vm': gate.name, 'owner_token': gate.owner_token}).encode(), b'')
                def collect(instance): pass
                with patch.dict('os.environ', {'TART_HOME': str(gate.tart_home)}), \
                     patch.object(m.shutil, 'disk_usage', return_value=argparse.Namespace(free=m.MIN_FREE * 2)), \
                     patch.object(m.Gate, 'call', recover_call), patch.object(m.Gate, 'guest', guest), \
                     patch.object(m.subprocess, 'Popen'), patch.object(m, 'validate_report') as app_validate, \
                     patch.object(m, 'validate_visual', return_value={'binarySHA256': 'c' * 64 if mode == 'wrong-binary' else pin}) as validate:
                    source_receipt = gate.out / 'recovery.json'
                    if mode == 'retry-before-collection':
                        with patch.object(m.Gate, 'collect_evidence', side_effect=m.GateError('interrupted transfer')):
                            first = recovery.recover(argparse.Namespace(receipt=source_receipt, output=root/'retry1'))
                        self.assertEqual(first['status'], 'failed')
                        source_receipt = root/'retry1/recovery.json'
                        self.assertEqual(recovery.receipt(source_receipt)['input_executable_sha256'], pin)
                        validate.assert_not_called()
                    if mode == 'invalid-report': validate.side_effect = m.GateError('incomplete visual scenarios or cleanup')
                    with patch.object(m.Gate, 'collect_evidence', collect):
                        result = recovery.recover(argparse.Namespace(receipt=source_receipt, output=root/'retry2'))
                self.assertEqual(result['status'], 'recovered-validated-evidence' if mode in ('success', 'retry-before-collection') else 'failed')
                self.assertEqual(result['input_executable_sha256'], pin)
                self.assertTrue(result['cleanup']['owned_clone_deleted'])
                app_validate.assert_not_called()
                if mode == 'failed-guest': validate.assert_not_called()
                else: validate.assert_called_once_with(root/'retry2/evidence')
                if mode == 'wrong-binary': self.assertIn('different executable', result['failure']['detail'])

    def test_lifecycle_recovery_preserves_variant_and_rejects_missing_check(self):
        for missing in (False, True):
            with self.subTest(missing=missing), tempfile.TemporaryDirectory() as temp:
                root = Path(temp).resolve(); gate, calls, call = self.fixture(root)
                gate.args.profile = 'cli'; gate.args.brew_version = '0.1.9'; gate.args.brew_lifecycle = True
                with patch.object(gate, 'call', call): gate.cleanup()
                self.assertIs(recovery.receipt(gate.out/'recovery.json')['brew_lifecycle'], True)
                cli = {'status': 'passed', 'version': '0.1.9', 'cleanup': True,
                       'profile': 'cli-public-package-lifecycle',
                       'checks': m.LIFECYCLE_CHECKS[:-1] if missing else list(m.LIFECYCLE_CHECKS)}
                # The normal path uses this same validator; a PASS label is insufficient.
                if missing:
                    with self.assertRaisesRegex(m.GateError, 'incomplete lifecycle'):
                        m.validate_cli(cli, '0.1.9', True)
                else: self.assertEqual(m.validate_cli(cli, '0.1.9', True), cli)
                with self.assertRaisesRegex(m.GateError, 'profile differs'):
                    m.validate_cli(cli, '0.1.9', False)
                def recover_call(instance, *args, **kwargs): return call(*args, **kwargs)
                def guest(instance, *args, **kwargs):
                    return subprocess.CompletedProcess(args, 0, json.dumps({'vm': gate.name, 'owner_token': gate.owner_token}).encode(), b'')
                def collect(instance):
                    self.assertIs(instance.args.brew_lifecycle, True)
                    path = instance.out/'evidence/project'; path.mkdir(parents=True)
                    (path/'cli-result.json').write_text(json.dumps(cli))
                with patch.dict('os.environ', {'TART_HOME': str(gate.tart_home)}), \
                     patch.object(m.shutil, 'disk_usage', return_value=argparse.Namespace(free=m.MIN_FREE*2)), \
                     patch.object(m.Gate, 'call', recover_call), patch.object(m.Gate, 'guest', guest), \
                     patch.object(m.Gate, 'collect_evidence', collect), patch.object(m.subprocess, 'Popen'):
                    result = recovery.recover(argparse.Namespace(receipt=gate.out/'recovery.json', output=root/'retry'))
                self.assertEqual(result['status'], 'failed' if missing else 'recovered-validated-evidence')
                self.assertTrue(result['cleanup']['owned_clone_deleted'])
                if missing: self.assertIn('incomplete lifecycle', result['failure']['detail'])

    def test_visual_receipt_without_original_hash_fails_before_boot(self):
        with tempfile.TemporaryDirectory() as temp:
            gate, calls, call = self.fixture(Path(temp))
            gate.args.profile = 'visual'
            with patch.object(gate, 'call', call): gate.cleanup()
            with patch.object(m.subprocess, 'Popen') as boot:
                with self.assertRaisesRegex(m.GateError, 'original executable hash'):
                    recovery.recover(argparse.Namespace(receipt=gate.out/'recovery.json', output=Path(temp)/'retry'))
            boot.assert_not_called()
