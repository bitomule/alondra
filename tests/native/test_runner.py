"""Fast runner contract tests: no build, app launch, simulator, VM or backend."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch
import zipfile

spec = importlib.util.spec_from_file_location("native_runner", Path(__file__).with_name("run.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class RunnerContracts(unittest.TestCase):
    def report(self, scenario):
        return {"schema": 1, "scope": "native-client-with-synthetic-http-peers", "runID": "current",
                "scenario": scenario, "status": "passed", "checks": dict.fromkeys(runner.CHECKS[scenario], True),
                "elapsedMilliseconds": 10}

    def test_only_complete_current_behavioral_evidence_passes(self):
        for scenario in runner.CHECKS:
            positive = self.report(scenario)
            runner.validate_report(positive, "current", scenario)
            for field, value in [("runID", "stale"), ("scenario", "wrong"), ("schema", 2),
                                 ("status", "running"), ("status", "failed"), ("error", "partial"),
                                 ("elapsedMilliseconds", float("nan")), ("elapsedMilliseconds", True)]:
                with self.subTest(scenario=scenario, field=field, value=value):
                    report = copy.deepcopy(positive)
                    report[field] = value
                    with self.assertRaises(ValueError):
                        runner.validate_report(report, "current", scenario)
            for name in runner.CHECKS[scenario]:
                for alteration in ("remove", "false", "truthy"):
                    with self.subTest(scenario=scenario, assertion=name, alteration=alteration):
                        report = copy.deepcopy(positive)
                        if alteration == "remove":
                            del report["checks"][name]
                        else:
                            report["checks"][name] = False if alteration == "false" else 1
                        with self.assertRaises(ValueError):
                            runner.validate_report(report, "current", scenario)
            report = copy.deepcopy(positive)
            report["checks"]["unexpected"] = True
            with self.assertRaises(ValueError):
                runner.validate_report(report, "current", scenario)

    def test_plan_starts_no_external_commands(self):
        with patch.object(runner.subprocess, "run", side_effect=AssertionError("launched")), \
             patch.object(runner.subprocess, "check_output", side_effect=AssertionError("launched")), \
             contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(runner.main(["--platform", "ios"]), 0)
        plan = json.loads(output.getvalue())
        self.assertEqual(plan["mode"], "plan")
        self.assertEqual(plan["scenarios"], list(runner.CHECKS))

    def test_archive_cannot_write_outside_its_owned_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "input.zip"
            with zipfile.ZipFile(archive, "w") as zipped:
                zipped.writestr("../escaped", "untrusted")
            with self.assertRaises(ValueError):
                runner.extract_app(archive, root / "extract")
            self.assertFalse((root / "escaped").exists())

    def test_failed_native_evidence_survives_validation_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = self.report("session-revoked")
            report["checks"]["revoked_read_rejected"] = False
            source, evidence = root / "source.json", root / "evidence.json"
            source.write_text(json.dumps(report))
            with self.assertRaises(ValueError):
                runner.wait_report(source, "current", "session-revoked", 1, evidence)
            self.assertEqual(json.loads(evidence.read_text()), report)

    def test_no_result_is_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(TimeoutError):
                runner.wait_report(Path(directory) / "absent", "current", "lost-response", 0,
                                   Path(directory) / "evidence")

    def test_app_does_not_receive_host_secret_environment(self):
        with patch.dict(runner.os.environ, {"OP_SESSION_SECRET": "secret", "SIMCTL_CHILD_ACCESS_TOKEN": "secret"}):
            env = runner.base_environment()
        self.assertNotIn("OP_SESSION_SECRET", env)
        self.assertNotIn("SIMCTL_CHILD_ACCESS_TOKEN", env)

    def test_cleanup_keeps_going_after_simulator_deletion_failure(self):
        commands = Mock()
        commands.run.side_effect = [RuntimeError("failed delete"), ""]
        lock = Mock()
        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            (cache / "kept").write_text("persistent")
            errors = runner.cleanup_owned(commands, "our-uuid", cache, lock, False)
            self.assertEqual(errors, ["simulator_cleanup_failed"])
            self.assertTrue((cache / "kept").exists())
        self.assertEqual(commands.run.call_args_list[0].args[0], ["xcrun", "simctl", "delete", "our-uuid"])
        self.assertEqual(commands.run.call_args_list[1].args[0][-1], "shutdown")
        lock.close.assert_called_once()

    def test_cleanup_without_ownership_cannot_touch_resources(self):
        commands = Mock()
        self.assertEqual(runner.cleanup_owned(commands, None, Path("/unowned"), None, False), [])
        commands.run.assert_not_called()

    def test_interrupted_create_reconciles_only_reserved_uuid_name(self):
        name = 'Alondra-native-7f8f1667-bae7-4432-9f05-4b9c9db991ea'
        owned = '67D20E42-75C7-4C21-AFA7-986FF4C7FC94'
        devices = [{'name': 'Personal iPhone', 'udid': 'DD7B9A9D-E186-4849-8EF0-798526E07EC9'},
                   {'name': name, 'udid': owned}]
        commands = Mock()
        commands.run.side_effect = [json.dumps({'devices': {'runtime': devices}}), '']
        # The create request reached the service, but timeout lost the returned UUID.
        errors = runner.cleanup_owned(commands, None, Path('/unused'), None, False, name)
        self.assertEqual(errors, [])
        self.assertEqual(commands.run.call_args_list[-1].args[0], ['xcrun', 'simctl', 'delete', owned.lower()])
        self.assertEqual(commands.run.call_count, 2)

    def test_unknown_interrupted_create_is_not_reported_clean(self):
        name = 'Alondra-native-7f8f1667-bae7-4432-9f05-4b9c9db991ea'
        for devices in ([], [{'name': name, 'udid': 'invalid'}],
                        [{'name': name, 'udid': 'one'}, {'name': name, 'udid': 'two'}]):
            commands = Mock()
            commands.run.return_value = json.dumps({'devices': {'runtime': devices}})
            self.assertEqual(runner.cleanup_owned(commands, None, Path('/unused'), None, False, name),
                             ['simulator_creation_unresolved'])
            self.assertEqual(commands.run.call_count, 1)

    def test_main_create_then_timeout_reconciles_persisted_ownership(self):
        for interruption in ('timeout', 'signal'):
            with self.subTest(interruption=interruption), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                output = root / 'result'
                binary = root / 'synthetic-executable'
                binary.write_bytes(b'synthetic test artifact')
                personal = {'name': 'Personal iPhone', 'udid': 'e0fb6c6d-8969-4e0d-86dd-e945b91c5b9f'}
                devices = [personal.copy()]
                reserved_before_create = []
                commands = Mock()
                def command(argv, **kwargs):
                    if argv == ['git', 'rev-parse', 'HEAD']:
                        return 'a' * 40
                    if argv[:3] == ['xcrun', 'simctl', 'create']:
                        persisted = json.loads((output / 'manifest.json').read_text())
                        reserved_before_create.append(persisted.get('ownedSimulatorName') == argv[3])
                        devices.append({'name': argv[3], 'udid': '67d20e42-75c7-4c21-afa7-986ff4c7fc94'})
                        if interruption == 'timeout':
                            raise runner.subprocess.TimeoutExpired(argv, 1)
                        raise InterruptedError('synthetic SIGTERM after creation')
                    if argv == ['xcrun', 'simctl', 'list', 'devices', '--json']:
                        return json.dumps({'devices': {'runtime': devices}})
                    if argv[:3] == ['xcrun', 'simctl', 'delete']:
                        devices[:] = [item for item in devices if item['udid'] != argv[3]]
                    return ''
                commands.run.side_effect = command
                lock = Mock()
                with patch.object(runner.subprocess, 'check_output', return_value=''), \
                     patch.object(runner, 'free_bytes', return_value=10 * 1024**3), \
                     patch.object(runner, 'Commands', return_value=commands), \
                     patch.object(runner, 'acquire_cache', return_value=lock), \
                     patch.object(runner, 'build_app', return_value=root / 'Harness.app'), \
                     patch.object(runner, 'app_executable', return_value=binary), \
                     patch.object(runner.sys, 'platform', 'darwin'), \
                     contextlib.redirect_stdout(io.StringIO()):
                    result = runner.main(['--platform', 'ios', '--run', '--runtime', 'synthetic-runtime',
                                          '--output', str(output), '--build-cache', str(root / 'cache')])
                self.assertEqual(devices, [personal], 'interrupted creation leaked its owned simulator')
                self.assertEqual(reserved_before_create, [True])
                self.assertEqual(result, 1)
                manifest = json.loads((output / 'manifest.json').read_text())
                self.assertEqual(manifest['mode'], 'failed')
                self.assertEqual(manifest['cleanup'], 'complete')
                self.assertTrue((output / 'report.md').is_file())
                lock.close.assert_called_once()

    def test_cache_lock_rejects_concurrent_use_and_foreign_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            foreign = root / "foreign"
            foreign.mkdir()
            with self.assertRaises(ValueError):
                runner.acquire_cache(foreign)
            owned = root / "ours"
            lock = runner.acquire_cache(owned)
            try:
                with self.assertRaises(RuntimeError):
                    runner.acquire_cache(owned)
            finally:
                lock.close()
            second = runner.acquire_cache(owned)
            second.close()

    def test_signal_enters_error_path_instead_of_exiting_without_cleanup(self):
        with self.assertRaises(InterruptedError):
            runner.interrupted(runner.signal.SIGTERM, None)


if __name__ == "__main__":
    unittest.main()
