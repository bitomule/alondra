"""Lightweight visual acceptance contracts, no app/MAV/simulator processes."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import subprocess
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).parent))
from visual import driver
import visual_run
mac_spec = importlib.util.spec_from_file_location('visual_tart', Path(__file__).parents[1] / 'system/macos.py')
visual_tart = importlib.util.module_from_spec(mac_spec)
mac_spec.loader.exec_module(visual_tart)
guest_spec = importlib.util.spec_from_file_location('visual_mac_guest', Path(__file__).parent / 'visual/mac_guest.py')
visual_mac_guest = importlib.util.module_from_spec(guest_spec)
with patch.dict(sys.modules, {'driver': driver}):
    guest_spec.loader.exec_module(visual_mac_guest)



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

    def test_mav_report_uses_open_receipt_not_ignored_environment_path(self):
        with tempfile.TemporaryDirectory() as directory:
            project = Path(directory).resolve()
            run = '96be7621'
            receipt = f'ok cmd=open dir={project}/.mav/runs/{run} run={run} target_kind=simulator'
            actual_run, report = driver.resolve_mav_run(receipt, project, set())
            self.assertEqual(actual_run, run)
            self.assertEqual(report, project / '.mav/runs' / run / 'report.json')
            for changed, old in [(receipt, {run}), (receipt.replace(run, '../outside'), set()),
                                 (receipt.replace(str(project), '/tmp/other'), set()),
                                 (receipt + ' run=second', set()), (receipt + '\n' + receipt, set())]:
                with self.assertRaises(ValueError): driver.resolve_mav_run(changed, project, old)
            report.parent.mkdir(parents=True)
            report.write_text(json.dumps({'run_id': 'stale'}))
            with self.assertRaisesRegex(ValueError, 'stale MAV report'):
                driver.validate_mav_report(report, [], run)

    def test_driver_keeps_original_failure_when_receipt_and_stop_also_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            calls = []
            def mav(*args):
                calls.append(args)
                if args[0] == 'open': raise RuntimeError('original launch failure')
                if args[0] == 'stop': raise TimeoutError('cleanup timeout')
                return ''
            with self.assertRaisesRegex(RuntimeError, 'original launch failure') as result:
                driver.exercise(mav, Mock(side_effect=ValueError('bad JSON')), Mock(),
                                Path(directory) / 'assertions', 'current', 'reconnect')
            self.assertEqual(calls[-1], ('stop',))
            self.assertEqual(json.loads((Path(directory) / 'assertions/failure-diagnostics.json').read_text()), [
                'Failure receipt unavailable: ValueError', 'MAV cleanup failed: TimeoutError'])

    def test_wait_failure_preserves_direct_ax_and_capture_without_replacing_cause(self):
        for tree_fails in (False, True):
            with self.subTest(tree_fails=tree_fails), tempfile.TemporaryDirectory() as directory:
                project = Path(directory).resolve()
                calls = []
                def mav(*args):
                    calls.append(args)
                    if args[0] == 'open':
                        return f'ok cmd=open dir={project}/.mav/runs/current run=current'
                    if args[:2] == ('ui', 'wait'): raise RuntimeError('original wait failure')
                    if args[:2] == ('ui', 'tree'):
                        if tree_fails: raise ValueError('underlying AX failure')
                        return 'actual accessibility tree'
                    return 'recorded'
                state = {'runID': 'current', 'scenario': 'reconnect', 'status': 'running',
                         'savedConfiguration': True, 'restoring': True, 'ready': False}
                with self.assertRaisesRegex(RuntimeError, 'original wait failure'):
                    driver.exercise(mav, lambda: state, Mock(), project / 'assertions', 'current', 'reconnect', project)
                self.assertIn(('evidence', 'step', '--name', 'failure', '--note', 'Failed synthetic visual scenario'), calls)
                self.assertEqual(calls[-1], ('stop',))
                self.assertTrue((project / 'assertions/failed-capture.txt').is_file())
                if tree_fails:
                    self.assertIn('failed-tree.txt unavailable: ValueError',
                                  json.loads((project / 'assertions/failure-diagnostics.json').read_text()))
                else:
                    self.assertEqual((project / 'assertions/failed-tree.txt').read_text(), 'actual accessibility tree')

    def test_full_ax_requires_owned_pid_window_and_real_tree(self):
        windows = {'windows': [{'pid': 71, 'window_id': 3, 'is_on_screen': True,
                               'bounds': {'width': 600, 'height': 400}}]}
        self.assertEqual(visual_mac_guest.select_window(windows, 71), 3)
        for changed in ({'windows': []}, {'windows': windows['windows'] * 2},
                        {'windows': [{**windows['windows'][0], 'pid': 72}]},
                        {'windows': [{**windows['windows'][0], 'is_on_screen': False}]}):
            with self.assertRaises(ValueError): visual_mac_guest.select_window(changed, 71)
        value = {'structuredContent': {'pid': 71, 'window_id': 3, 'snapshot_id': 'fresh',
                                      'elements': [], 'tree_markdown': '- AXStaticText = "Connecting to the fleet…"'}}
        tree, snapshot = visual_mac_guest.validate_tree(value, 71, 3)
        self.assertIn('Connecting to the fleet…', tree)
        self.assertEqual(snapshot, 'fresh')
        for field, bad in [('pid', 72), ('window_id', 4), ('tree_markdown', ''),
                           ('tree_markdown', None), ('snapshot_id', ''), ('degraded', True), ('degraded_reason', 'ax_window_unresolved'), ('truncated', True),
                           ('nodes_pending', 1), ('tree_markdown', 'AX tree truncated at 2000 nodes'),
                           ('tree_markdown', ' ' * 80 + 'AXStaticText hidden')]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                visual_mac_guest.validate_tree({'structuredContent': {**value['structuredContent'], field: bad}}, 71, 3)
        with self.assertRaises(ValueError): visual_mac_guest.validate_tree({'isError': True}, 71, 3)
        with self.assertRaises(ValueError): visual_mac_guest.validate_tree([], 71, 3)

    def test_full_ax_wait_sees_static_text_not_only_addressable_elements(self):
        read = Mock(side_effect=['- AXWindow "Loading"', '- AXStaticText = "Connecting to the fleet…"'])
        with patch.object(driver.time, 'sleep'):
            driver.wait_tree_text(read, 'Connecting to the fleet…')
        self.assertEqual(read.call_count, 2)
        with patch.object(driver.time, 'monotonic', side_effect=[0, 0, 2]), patch.object(driver.time, 'sleep'):
            with self.assertRaises(TimeoutError):
                driver.wait_tree_text(lambda: '- AXWindow "Loading"', 'Connecting to the fleet…', timeout=1)

    def test_full_ax_timeout_stops_only_command_and_preserves_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Mock(pid=71); app.poll.return_value = None
            command = Mock(); command.wait.side_effect = subprocess.TimeoutExpired('cua-driver', 10)
            reader = visual_mac_guest.OwnedTreeReader(app, Path(directory) / 'full-ax', {}, 'synthetic')
            with patch.object(visual_mac_guest.subprocess, 'Popen', return_value=command) as spawn, \
                 patch.object(visual_mac_guest, 'stop_owned') as stop:
                with self.assertRaises(subprocess.TimeoutExpired): reader.call('list_windows', {'pid': 71})
            stop.assert_called_once_with(command)
            self.assertEqual(spawn.call_args.args[0][:3],
                             ['/Applications/CuaDriver.app/Contents/MacOS/cua-driver', 'call', 'list_windows'])
            self.assertEqual(json.loads(spawn.call_args.args[0][3])['pid'], 71)
            self.assertTrue((Path(directory) / 'full-ax/001-list_windows.json').exists())
            app.poll.return_value = 0
            with patch.object(visual_mac_guest.subprocess, 'Popen') as spawn:
                with self.assertRaisesRegex(ValueError, 'exited'): reader.call('list_windows', {'pid': 71})
                spawn.assert_not_called()

    def test_full_ax_closes_its_named_session_even_after_app_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Mock(pid=71); app.poll.return_value = 0
            reader = visual_mac_guest.OwnedTreeReader(app, Path(directory) / 'full-ax', {}, 'owned-run')
            def spawn(args, **kwargs):
                self.assertEqual(args[2], 'end_session')
                self.assertEqual(json.loads(args[3]), {'session': 'alondra-visual-owned-run'})
                kwargs['stdout'].write(b'{}'); kwargs['stdout'].flush()
                command = Mock(returncode=0); command.poll.return_value = 0
                return command
            with patch.object(visual_mac_guest.subprocess, 'Popen', side_effect=spawn) as create:
                reader.close()
            self.assertEqual(create.call_count, 1)

    def test_mac_host_runtime_refused(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            visual_run.main(['--platform', 'mac', '--run', '--output', '/private/tmp/uncreated-visual'])

    def test_tart_host_rechecks_each_scenario_and_required_captures(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            summary = {'status': 'passed', 'cleanup': True, 'scenarios': []}
            for index, scenario in enumerate(driver.CHECKS, 1):
                run = '00000000-0000-0000-0000-%012d' % index
                summary['scenarios'].append({'scenario': scenario, 'runID': run})
                assertions = root / 'project' / scenario / 'assertions'
                assertions.mkdir(parents=True)
                native = {**self.report(scenario), 'runID': run}
                (assertions / 'result.json').write_text(json.dumps(native))
                mav_run = '%08x' % index
                receipt = {'status': 'passed', 'runID': run, 'mavRunID': mav_run, 'scenario': scenario, 'steps': driver.STEPS[scenario]}
                (assertions / 'driver.json').write_text(json.dumps(receipt))
                report = root / 'project/.mav/runs' / mav_run / 'report.json'
                report.parent.mkdir(parents=True)
                report.write_text(json.dumps({'run_id': mav_run, 'steps': [{'name': name, 'image': {'ok': True}} for name in driver.STEPS[scenario]],
                                              'crashes': [], 'invalid_step_count': 0, 'video_status': 'accepted'}))
            path = root / 'project/visual-summary.json'
            path.write_text(json.dumps(summary))
            visual_tart.validate_visual(root)
            # A passing wrapper cannot hide a missing actual identity assertion.
            last = root / 'project/mac-selection/assertions/result.json'
            original = last.read_text()
            altered = json.loads(original); altered['checks']['late_alpha_rejected'] = False
            last.write_text(json.dumps(altered))
            with self.assertRaisesRegex(ValueError, 'failed visual fixture checks'):
                visual_tart.validate_visual(root)
            last.write_text(original)
            receipt = root / 'project/mac-selection/assertions/driver.json'
            altered = json.loads(receipt.read_text()); altered['steps'] = []
            receipt.write_text(json.dumps(altered))
            with self.assertRaisesRegex(visual_tart.GateError, 'wrong visual driver receipt'):
                visual_tart.validate_visual(root)

    def test_visual_app_profile_rejects_shipping_bundle(self):
        import plistlib
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / 'Alondra.app'
            (app / 'Contents').mkdir(parents=True)
            path = app / 'Contents/Info.plist'
            info = {'CFBundleIdentifier': 'com.bitomule.alondra.mac', 'CFBundleExecutable': 'Alondra', 'CFBundlePackageType': 'APPL'}
            path.write_bytes(plistlib.dumps(info))
            with self.assertRaisesRegex(visual_tart.GateError, 'unexpected app identity'):
                visual_tart.validate_app(app, visual=True)
            info['CFBundleIdentifier'] = 'com.bitomule.alondra.visualharness'
            path.write_bytes(plistlib.dumps(info))
            visual_tart.validate_app(app, visual=True)
            (app / 'Contents/embedded.provisionprofile').write_bytes(b'synthetic')
            with self.assertRaisesRegex(visual_tart.GateError, 'Debug app'):
                visual_tart.validate_app(app, visual=True)

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
