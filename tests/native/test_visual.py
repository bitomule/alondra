"""Lightweight visual acceptance contracts, no app/MAV/simulator processes."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).parent))
from visual import driver
import visual_run


class VisualContracts(unittest.TestCase):
    def report(self, scenario):
        return {'schema': 1, 'scope': 'visual-native-fixture', 'runID': 'current', 'scenario': scenario,
                'status': 'passed', 'checks': dict.fromkeys(driver.CHECKS[scenario], True)}

    def test_exact_current_assertions_required(self):
        for scenario in driver.CHECKS:
            value = self.report(scenario)
            driver.validate_report(value, 'current', scenario)
            for key in driver.CHECKS[scenario]:
                for mode in ('missing', 'false', 'truthy'):
                    changed = copy.deepcopy(value)
                    if mode == 'missing': changed['checks'].pop(key)
                    else: changed['checks'][key] = False if mode == 'false' else 1
                    with self.subTest(scenario=scenario, key=key, mode=mode), self.assertRaises(ValueError):
                        driver.validate_report(changed, 'current', scenario)
            for field, content in [('runID', 'stale'), ('status', 'running'), ('scenario', 'other'), ('scope', 'real-backend')]:
                changed = {**value, field: content}
                with self.assertRaises(ValueError): driver.validate_report(changed, 'current', scenario)

    def test_onboarding_and_stale_mac_are_rejected_independently_of_state_receipt(self):
        driver.assert_tree('Tasks Connecting to the fleet', ['Tasks'], ['pairing.screen'])
        for tree, required, forbidden in [('Tasks pairing.screen', ['Tasks'], ['pairing.screen']),
                                           ('Beta Mac question Alpha Mac question', ['Beta Mac question'], ['Alpha Mac question']),
                                           ('blank', ['Beta Mac question'], [])]:
            with self.assertRaises(ValueError): driver.assert_tree(tree, required, forbidden)

    def test_capture_and_video_required_no_skipped_images(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'report.json'
            value = {'steps': [{'name': 'first', 'image': {'ok': True}}], 'crashes': [],
                     'invalid_step_count': 0, 'video_status': 'accepted'}
            path.write_text(json.dumps(value))
            driver.validate_mav_report(path, ['first'])
            for field, content in [('steps', []), ('steps', value['steps'] * 2),
                                   ('steps', [{'name': 'first', 'image': {'ok': False}}]),
                                   ('video_status', 'missing'), ('crashes', ['crash'])]:
                path.write_text(json.dumps({**value, field: content}))
                with self.assertRaises(ValueError): driver.validate_mav_report(path, ['first'])

    def test_plan_does_not_launch_tools_or_create_directories(self):
        with patch.object(visual_run.subprocess, 'check_output', side_effect=AssertionError('tool launched')), \
             patch.object(visual_run.common.Commands, 'run', side_effect=AssertionError('tool launched')), \
             contextlib.redirect_stdout(io.StringIO()) as result:
            self.assertEqual(visual_run.main(['--platform', 'ios']), 0)
        self.assertFalse(json.loads(result.getvalue())['starts'])

    def test_wait_does_not_accept_stale_receipt(self):
        with self.assertRaisesRegex(ValueError, 'stale'):
            driver.wait_state(lambda: self.report('reconnect'), 'different-run', 'reconnect', lambda _: True)

    def test_mac_host_runtime_refused(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            visual_run.main(['--platform', 'mac', '--run', '--output', '/private/tmp/uncreated-visual'])

    def test_failed_mav_still_stops_owned_run_before_simulator_cleanup(self):
        for stop_fails in (False, True):
            with self.subTest(stop_fails=stop_fails), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                binary = root / 'synthetic-binary'; binary.write_bytes(b'fixture')
                (root / 'container/Documents').mkdir(parents=True)
                events = []
                commands = Mock()
                def command(args, **kwargs):
                    if args == ['git', 'rev-parse', 'HEAD']: return 'a' * 40
                    if args[:3] == ['xcrun', 'simctl', 'create']: return 'AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA'
                    if args[:3] == ['xcrun', 'simctl', 'get_app_container']: return str(root / 'container')
                    if args == ['mav', 'stop']:
                        events.append('mav-stop')
                        if stop_fails: raise RuntimeError('synthetic stop failure')
                    return ''
                commands.run.side_effect = command
                def cleanup(*args):
                    events.append('simulator-cleanup')
                    return []
                with patch.object(visual_run.subprocess, 'check_output', return_value=''), \
                     patch.object(visual_run.common, 'free_bytes', return_value=10 * 1024**3), \
                     patch.object(visual_run.common, 'Commands', return_value=commands), \
                     patch.object(visual_run.common, 'acquire_cache', return_value=Mock()), \
                     patch.object(visual_run.common, 'cleanup_owned', side_effect=cleanup), \
                     patch.object(visual_run, 'build', return_value=(root / 'app', binary)), \
                     patch.object(driver, 'exercise', side_effect=RuntimeError('original visual failure')), \
                     contextlib.redirect_stdout(io.StringIO()):
                    code = visual_run.main(['--platform', 'ios', '--run', '--runtime', 'synthetic',
                        '--output', str(root / 'output'), '--build-cache', str(root / 'cache')])
                self.assertEqual(code, 1)
                self.assertEqual(events, ['mav-stop', 'simulator-cleanup'])
                manifest = json.loads((root / 'output/manifest.json').read_text())
                self.assertEqual(manifest['error'], 'original visual failure')
                self.assertEqual(manifest['cleanup'], 'failed' if stop_fails else 'complete')


if __name__ == '__main__':
    unittest.main()
