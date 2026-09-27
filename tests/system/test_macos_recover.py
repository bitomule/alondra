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
