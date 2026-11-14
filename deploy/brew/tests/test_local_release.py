import importlib.util
import io
import json
import os
import pathlib
import subprocess
import sys
import shutil
import tarfile
import tempfile
import tomllib
import unittest
from unittest.mock import patch

SCRIPT = pathlib.Path(__file__).parents[1] / 'local-release.py'
REPO_ROOT = SCRIPT.parents[2]
sys.path.insert(0, str(SCRIPT.parent))
SPEC = importlib.util.spec_from_file_location('local_release', SCRIPT)
l = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(l)


def archives(directory, ver='0.1.2'):
    for target in l.release.TARGETS:
        with tarfile.open(directory / f'alondra-{ver}-{target}.tar.gz', 'w:gz') as output:
            for name in sorted(l.release.PUBLIC_FILES):
                item = tarfile.TarInfo(name)
                item.size = len(name)
                output.addfile(item, io.BytesIO(name.encode()))


def rehearsed(directory, ver='0.1.2'):
    archive = directory / f'alondra-{ver}-{l.release.TARGETS[0]}.tar.gz'
    record = {'schema': 1, 'from': '0.1.1', 'to': ver, 'archive': {'name': archive.name, 'sha256': l.release.sha(archive)},
              'cases': {l.SDK_HANDOVER: {'crashed_after': list(l.HANDOVER_CRASHES)}, l.FRESH_SDK: {'session': 'fresh-session'},
                        l.PREVIOUS_DOCTOR: {'doctor_exit_code': 0, 'options_sha256': 'a' * 64}}}
    (directory / l.REHEARSAL).write_text(json.dumps(record))


def bazel(args):
    """What Bazel answers about the rehearsal's SDK artifacts."""
    if args[1] == 'info':
        return '/execroot\n'
    if args[1] == 'cquery':
        built = 'bazel-out/' + args[-1].strip('/').replace(':', '/')
        return f'{built}\n{built}.clippy.ok\n'
    return ''


def rehearsal_call(args):
    return args[0] == sys.executable and str(args[1]).endswith('upgrade-rehearsal.py')


class LocalReleaseTests(unittest.TestCase):
    def test_the_daemon_signature_must_bind_its_info_plist(self):
        def details(identifier, plist):
            lines = [f'Identifier={identifier}', 'Format=Mach-O thin (arm64)',
                     'Authority=Developer ID Application: Someone (TEAM)', 'Timestamp=15 Oct 2026',
                     'Info.plist entries=6' if plist else 'Info.plist=not bound', f'TeamIdentifier={l.TEAM}']
            return '\n'.join(lines) + '\n'
        l.check_signature(details('com.bitomule.alondrad', True), 'com.bitomule.alondrad')
        l.check_signature(details('com.bitomule.alondra', False), 'com.bitomule.alondra')
        with self.assertRaisesRegex(RuntimeError, 'Local Network'):
            l.check_signature(details('com.bitomule.alondrad', False), 'com.bitomule.alondrad')
        with self.assertRaisesRegex(RuntimeError, 'another identifier'):
            l.check_signature(details('dev.alondra.alondrad', True), 'com.bitomule.alondrad')
        with self.assertRaisesRegex(RuntimeError, 'identity'):
            l.check_signature(details('com.bitomule.alondrad', True).replace('Timestamp=', 'Signed='), 'com.bitomule.alondrad')

    def test_each_signed_host_is_smoked_with_the_refusal_its_own_entry_prints(self):
        entries = {l.release.HOST: 'agent-host/persistent-main.ts', l.release.CODEX_HOST: 'agent-host/codex-main.ts'}
        self.assertEqual(set(l.HOST_REFUSALS), set(entries))
        with tempfile.TemporaryDirectory() as directory, patch.object(l, 'run'):
            for name, refusal in l.HOST_REFUSALS.items():
                self.assertIn(f"'{refusal}\\n'", (REPO_ROOT / entries[name]).read_text())
                host = pathlib.Path(directory) / pathlib.Path(name).name
                host.write_text(f"#!/bin/sh\necho '{refusal}' >&2\nexit 1\n")
                host.chmod(0o755)
                l.host_smoke(host, refusal)
            with self.assertRaisesRegex(RuntimeError, 'did not start under its signature'):
                l.host_smoke(host, l.HOST_REFUSALS[l.release.HOST] + ' elsewhere')

    def test_the_daemon_info_plist_names_the_identifier_it_is_signed_with(self):
        import plistlib
        plist = plistlib.loads((REPO_ROOT / 'crates/alondrad/Info.plist').read_bytes())
        self.assertIn(plist['CFBundleIdentifier'], l.WITH_INFO_PLIST)
        self.assertTrue(plist['NSLocalNetworkUsageDescription'])

    def test_workspace_crate_versions_match_lockfile(self):
        workspace = tomllib.loads((REPO_ROOT / 'Cargo.toml').read_text())['workspace']
        expected_version = workspace['package']['version']
        package_versions = {}
        for member in workspace['members']:
            manifest = tomllib.loads((REPO_ROOT / member / 'Cargo.toml').read_text())
            package = manifest['package']
            self.assertEqual(package['version'], {'workspace': True}, package['name'])
            package_versions[package['name']] = expected_version

        lockfile = tomllib.loads((REPO_ROOT / 'Cargo.lock').read_text())
        locked_versions = {
            package['name']: package['version']
            for package in lockfile['package']
            if package['name'] in package_versions
        }
        self.assertEqual(locked_versions, package_versions)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name).resolve()
        (self.root / 'Cargo.toml').write_text('[workspace.package]\nversion="0.1.2"\n')
        self.addCleanup(patch.stopall)
        patch.object(l, 'ROOT', self.root).start()

    def git(self, *args, **kwargs):
        values = {('git', 'status', '--porcelain'): '',
                  ('git', 'rev-parse', 'HEAD'): 'source'}
        return values.get(args, '')

    def not_on_main(self, *args, **kwargs):
        if args[:2] == ('git', 'merge-base'):
            raise subprocess.CalledProcessError(1, args)
        return self.git(*args, **kwargs)

    def test_preflight_accepts_clean_detached_commit_already_on_origin_main(self):
        with patch.object(l, 'run', side_effect=self.git) as runner:
            self.assertEqual(l.preflight('0.1.2'), 'source')
            runner.assert_any_call('git', 'merge-base', '--is-ancestor', 'HEAD', 'origin/main')
            self.assertFalse(any(c.args[:2] == ('git', 'branch') for c in runner.call_args_list))

    def test_preflight_rejects_dirty_unmerged_and_wrong_version_before_mutations(self):
        dirty = lambda *a, **k: ' M file' if a == ('git', 'status', '--porcelain') else self.git(*a, **k)
        for name, fake in [('dirty', dirty), ('not on origin/main', self.not_on_main)]:
            with self.subTest(name), patch.object(l, 'run', side_effect=fake) as runner:
                with self.assertRaises(RuntimeError): l.preflight('0.1.2')
                self.assertFalse(any(c.args[:2] in [('git', 'tag'), ('git', 'push')] for c in runner.call_args_list))
        with patch.object(l, 'run') as runner, self.assertRaises(RuntimeError):
            l.preflight('0.1.3')
        runner.assert_not_called()

    def test_any_returning_automatic_workflow_blocks_release_without_api_or_build(self):
        directory = self.root / '.github/workflows'
        directory.mkdir(parents=True)
        for name in ('brew-release.yml', 'brew-release-check.yml', 'new-check.yaml'):
            path = directory / name
            path.write_text('name: unexpected automation')
            with patch.object(l, 'run') as runner, self.assertRaises(RuntimeError): l.workflow_guard()
            runner.assert_not_called()
            path.unlink()
        l.workflow_guard()

    def test_existing_tags_never_rebuild_and_retry_checks_source(self):
        with patch.object(l, 'run', return_value='tag'):
            with self.assertRaises(RuntimeError): l.tag_state('0.1.2', 'source', False)
        def remote(*args, **kwargs):
            return 'wrong refs/tags/alondra-v0.1.2^{}\n' if args[1] == 'ls-remote' else ''
        with patch.object(l, 'run', side_effect=remote), self.assertRaises(RuntimeError):
            l.tag_state('0.1.2', 'source', True)

    def test_manifest_detects_changed_bytes_source_version_and_private_payload(self):
        output = self.root / 'artifacts'
        output.mkdir()
        archives(output)
        rehearsed(output)
        record = l.manifest('0.1.2', 'source', output)
        (output / 'manifest.json').write_text(json.dumps(record))
        l.verify_prepared('0.1.2', 'source', output)
        with self.assertRaises(RuntimeError): l.verify_prepared('0.1.2', 'other', output)
        record['version'] = '0.1.1'
        (output / 'manifest.json').write_text(json.dumps(record))
        with self.assertRaises(RuntimeError): l.verify_prepared('0.1.2', 'source', output)
        record['version'] = '0.1.2'
        (output / 'manifest.json').write_text(json.dumps(record))
        archive = next(output.glob('*.tar.gz'))
        archive.write_bytes(archive.read_bytes() + b'changed')
        with self.assertRaises(RuntimeError): l.verify_prepared('0.1.2', 'source', output)
        # Even a matching manifest cannot bypass the publisher's public-file allowlist.
        with tarfile.open(archive, 'w:gz') as tar:
            tar.addfile(tarfile.TarInfo('private-source.rs'), io.BytesIO(b''))
        (output / 'manifest.json').write_text(json.dumps(l.manifest('0.1.2', 'source', output)))
        with self.assertRaises(ValueError): l.verify_prepared('0.1.2', 'source', output)

    def test_failed_checks_leave_no_manifest_tag_or_published_artifacts(self):
        patch.object(l.platform, 'system', return_value='Darwin').start()
        patch.object(l.platform, 'machine', return_value='arm64').start()
        patch.object(l, 'require_disk_space').start()
        calls = []
        def fake(*args, **kwargs):
            calls.append(args)
            if args[:2] == ('bazelisk', 'test'):
                raise subprocess.CalledProcessError(1, args)
            if args[0] == 'security': return f'1) {l.IDENTITY} "local identity"'
            return 'x86_64'
        output = self.root / 'artifacts'
        with patch.object(l, 'run', side_effect=fake), self.assertRaises(subprocess.CalledProcessError):
            l.prepare('0.1.2', 'source', output)
        self.assertFalse((output / 'manifest.json').exists())
        self.assertFalse(any(c[0] in ('git', 'codesign') for c in calls))

    def test_prepare_runs_every_check_through_bazel_before_signing(self):
        patch.object(l.platform, 'system', return_value='Darwin').start()
        patch.object(l.platform, 'machine', return_value='arm64').start()
        patch.object(l, 'require_disk_space').start()
        calls = []
        def fake(*args, **kwargs):
            calls.append(args)
            if args[:2] == ('bazelisk', 'test'):
                raise subprocess.CalledProcessError(1, args)
            if args[0] == 'security':
                return f'1) {l.IDENTITY} "local identity"'
            self.fail(f'Unexpected command before release validation: {args}')
        with patch.object(l, 'run', side_effect=fake), self.assertRaises(subprocess.CalledProcessError):
            l.prepare('0.1.2', 'source', self.root / 'artifacts')
        self.assertEqual(calls[-1], ('bazelisk', 'test', '//crates/...', '//deploy/brew/...'))

    def test_disk_guard_accepts_threshold_and_rejects_below_it(self):
        sufficient = type('Usage', (), {'free': l.LOW_FREE})()
        insufficient = type('Usage', (), {'free': l.LOW_FREE - 1})()
        path = self.root / 'not-created'
        with patch.object(l.shutil, 'disk_usage', return_value=sufficient):
            l.require_disk_space(path)
        with patch.object(l.shutil, 'disk_usage', return_value=insufficient), \
             self.assertRaisesRegex(RuntimeError, 'At least 8 GiB must be free'):
            l.require_disk_space(path)
        self.assertFalse((self.root / 'not-created').exists())

    def test_prepare_rejects_low_disk_before_any_build_or_artifact(self):
        patch.object(l.platform, 'system', return_value='Darwin').start()
        patch.object(l.platform, 'machine', return_value='arm64').start()
        insufficient = type('Usage', (), {'free': l.LOW_FREE - 1})()
        output = self.root / 'artifacts'
        with patch.object(l.shutil, 'disk_usage', return_value=insufficient), \
             patch.object(l, 'run') as runner, \
             self.assertRaisesRegex(RuntimeError, 'At least 8 GiB must be free'):
            l.prepare('0.1.2', 'source', output)
        runner.assert_not_called()
        self.assertFalse(output.exists())

    def test_retry_only_publishes_verified_original_artifacts(self):
        output = self.root / 'artifacts'
        output.mkdir()
        archives(output)
        rehearsed(output)
        (output / 'manifest.json').write_text(json.dumps(l.manifest('0.1.2', 'source', output)))
        with patch.dict(os.environ, {'ALONDRA_RELEASE_DIR': str(self.root / 'artifacts')}), \
             patch.object(sys, 'argv', ['local-release.py', '0.1.2', '--retry-publish']), \
             patch.object(l, 'preflight', return_value='source'), patch.object(l, 'tag_state') as tags, \
             patch.object(l, 'prepare') as prepare, patch.object(l, 'publish') as publish:
            l.main()
            prepare.assert_not_called()
            tags.assert_called_once_with('0.1.2', 'source', True)
            publish.assert_called_once_with('0.1.2', 'source', self.root / 'artifacts', retry=True)

    def test_publish_validates_before_tag_and_keeps_bytes_after_failure(self):
        output = self.root / 'artifacts'
        output.mkdir()
        archives(output)
        rehearsed(output)
        (output / 'manifest.json').write_text(json.dumps(l.manifest('0.1.2', 'source', output)))
        before = {p.name: p.read_bytes() for p in output.iterdir()}
        with patch.object(l, 'preflight', return_value='source'), patch.object(l, 'tag_state', return_value=(False, False)), \
             patch.object(l, 'run') as runner, patch.object(l.release, 'publish', side_effect=RuntimeError('network')):
            with self.assertRaises(RuntimeError): l.publish('0.1.2', 'source', output)
            self.assertEqual(runner.call_args_list[0].args[:3], ('git', 'tag', '-a'))
        self.assertEqual(before, {p.name: p.read_bytes() for p in output.iterdir()})
        (output / 'manifest.json').write_text('{}')
        with patch.object(l, 'run') as runner, self.assertRaises(RuntimeError): l.publish('0.1.2', 'source', output)
        runner.assert_not_called()

    def test_retry_accepts_original_clean_commit_when_main_advanced_but_requires_ancestry(self):
        with patch.object(l, 'run', side_effect=self.git) as runner:
            self.assertEqual(l.preflight('0.1.2', retry_source='source'), 'source')
            runner.assert_any_call('git', 'merge-base', '--is-ancestor', 'HEAD', 'origin/main')
        with patch.object(l, 'run', side_effect=self.git), self.assertRaises(RuntimeError):
            l.preflight('0.1.2', retry_source='different-manifest-source')
        with patch.object(l, 'run', side_effect=self.not_on_main), self.assertRaises(RuntimeError):
            l.preflight('0.1.2', retry_source='source')


    def release_fixture(self, *, failure=None, reported='0.1.2'):
        """A prepare whose Bazel, signing and packaging are fakes: Bazel answers with built files in a
        fake execution root, and each signature appends who signed it."""
        execroot = self.root / 'execroot'
        output = self.root / 'artifacts'
        built = {name: execroot / 'bazel-out/release' / label.split(':')[-1] for name, label in l.RELEASE_TARGETS.items()}
        signed, archive_bytes, built_with = [], {}, []
        def fake(*args, **kwargs):
            if args[0] == 'bazelisk':
                self.assertFalse(any(str(arg).startswith('--config') for arg in args), 'one configuration: the tests\'')
            if args[:2] == ('bazelisk', 'info'):
                return f'{execroot}\n'
            if args[:2] == ('bazelisk', 'build') and args[2:] == tuple(l.RELEASE_TARGETS.values()):
                built_with.append(args[2:])
                for name, path in built.items():
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(name.encode())
                    path.chmod(0o555)
                built['alondra-host'].with_name('alondra-host.manifest.json').write_text(json.dumps(
                    {'laboratory': False, 'sdk': '0.3.300', 'node': 'v24.19.0', 'bundle_sha256': 'b' * 64}))
                if failure == 'build':
                    raise subprocess.CalledProcessError(1, args)
                return ''
            if args[:2] == ('bazelisk', 'cquery') and args[-1] in l.RELEASE_TARGETS.values():
                name = next(name for name, label in l.RELEASE_TARGETS.items() if label == args[-1])
                relative = built[name].relative_to(execroot)
                return f'{relative}\n{relative}.clippy.ok\n' + (f'{relative.with_name("alondra-host.manifest.json")}\n' if name == 'alondra-host' else '')
            if args[0] == 'bazelisk':
                return bazel(args)
            if args[1:] == ('--version',):
                self.assertNotEqual(kwargs['env']['HOME'], str(pathlib.Path.home()))
                self.assertEqual(kwargs['env']['PATH'], '/usr/bin:/bin')
                name = pathlib.Path(args[0]).name
                return f'alondrad {reported}\n' if name == 'alondrad' else f'alondra {reported} · service not running\n'
            if rehearsal_call(args):
                self.assertFalse((output / 'manifest.json').exists(), 'the rehearsal runs before the manifest exists')
                if failure == 'rehearsal':
                    raise subprocess.CalledProcessError(1, args)
                rehearsed(output)
            elif args[0] == 'security':
                return f'1) {l.IDENTITY} "local identity"'
            elif args[0] == 'bash':
                self.assertEqual(args[1], 'deploy/brew/package-cli.sh')
                arch, payload, manifest = args[3], pathlib.Path(args[4]).parent, pathlib.Path(args[8])
                self.assertEqual(manifest, built['alondra-host'].with_name('alondra-host.manifest.json'))
                path = output / f'alondra-0.1.2-{arch}.tar.gz'
                self.assertFalse(path.exists())
                with tarfile.open(path, 'w:gz') as archive:
                    for name in sorted(l.release.PUBLIC_FILES):
                        source = payload / ('alondra-host' if name == l.release.HOST else name)
                        data = source.read_bytes() if name in l.SIGNED or name == l.release.HOST else name.encode()
                        item = tarfile.TarInfo(name)
                        item.size = len(data)
                        archive.addfile(item, io.BytesIO(data))
            return ''
        def sign(binary, identifier, entitlements=None):
            signed.append((binary.name, identifier, entitlements))
            binary.write_bytes(binary.read_bytes() + b':signed')
        def smoke(ver, arch, archive):
            self.assertFalse((output / 'manifest.json').exists())
            l.release.validate_archive(archive)
            archive_bytes[archive.name] = archive.read_bytes()
            with tarfile.open(archive) as package:
                for name in [*l.SIGNED, l.release.HOST]:
                    self.assertTrue(package.extractfile(name).read().endswith(b':signed'), name)
            if failure == 'smoke':
                raise RuntimeError('smoke failed')
        patch.object(l.platform, 'system', return_value='Darwin').start()
        patch.object(l.platform, 'machine', return_value='arm64').start()
        patch.object(l, 'run', side_effect=fake).start()
        patch.object(l, 'sign', side_effect=sign).start()
        patch.object(l, 'smoke', side_effect=smoke).start()
        patch.object(l, 'require_disk_space').start()
        return output, built, signed, archive_bytes, built_with

    def test_a_release_signs_copies_of_the_binaries_the_tests_built(self):
        output, built, signed, originals, built_with = self.release_fixture()
        l.prepare('0.1.2', 'source', output)
        self.assertEqual(built_with, [tuple(l.RELEASE_TARGETS.values())])
        self.assertEqual(sorted(signed), sorted([(name, identifier, None) for name, identifier in l.SIGNED.items()]
                                               + [('alondra-host', l.HOST_IDENTIFIER, l.HOST_ENTITLEMENTS), ('alondra-codex-host', 'com.bitomule.alondra-codex-host', l.HOST_ENTITLEMENTS)]))
        self.assertTrue(all(path.read_bytes() == name.encode() for name, path in built.items()), 'Bazel outputs stay as built')
        for name, original in originals.items():
            self.assertEqual((output / name).read_bytes(), original)
        l.verify_prepared('0.1.2', 'source', output)

    def test_binaries_that_report_another_version_never_reach_a_signature(self):
        output, _, signed, _, _ = self.release_fixture(reported='0.1.0')
        with self.assertRaisesRegex(RuntimeError, "reports 'alondrad 0.1.0', not 0.1.2"):
            l.prepare('0.1.2', 'source', output)
        self.assertEqual(signed, [])
        self.assertFalse((output / 'manifest.json').exists())

    def test_the_host_entitlements_grant_only_what_v8_needs(self):
        import plistlib
        self.assertEqual(plistlib.loads(l.HOST_ENTITLEMENTS.read_bytes()), {'com.apple.security.cs.allow-jit': True})

    def test_a_failed_build_or_smoke_leaves_nothing_publishable(self):
        for failure in ('build', 'smoke'):
            output, _, signed, originals, _ = self.release_fixture(failure=failure)
            with self.assertRaises((subprocess.CalledProcessError, RuntimeError)):
                l.prepare('0.1.2', 'source', output)
            self.assertFalse((output / 'manifest.json').exists())
            if failure == 'build':
                self.assertEqual(signed, [])
            else:
                for name, original in originals.items():
                    self.assertEqual((output / name).read_bytes(), original, 'the signed archive survives as it was')
            patch.stopall()
            for child in self.root.iterdir():
                shutil.rmtree(child) if child.is_dir() else child.unlink()

    def test_a_failed_upgrade_rehearsal_leaves_nothing_publishable(self):
        output, *_ = self.release_fixture(failure='rehearsal')
        with self.assertRaises(subprocess.CalledProcessError):
            l.prepare('0.1.2', 'source', output)
        self.assertFalse((output / 'manifest.json').exists())
        self.assertFalse((output / l.REHEARSAL).exists())

    def test_publishing_requires_a_rehearsal_of_the_exact_prepared_bytes(self):
        output = self.root / 'artifacts'
        output.mkdir()
        archives(output)
        (output / 'manifest.json').write_text(json.dumps(l.manifest('0.1.2', 'source', output)))
        with patch.object(l, 'run') as runner, self.assertRaisesRegex(RuntimeError, 'No upgrade rehearsal passed'):
            l.publish('0.1.2', 'source', output)
        runner.assert_not_called()
        rehearsed(output)
        archive = next(output.glob('*.tar.gz'))
        record = json.loads((output / l.REHEARSAL).read_text())
        record['archive']['sha256'] = '0' * 64
        (output / l.REHEARSAL).write_text(json.dumps(record))
        with self.assertRaisesRegex(RuntimeError, 'other bytes'):
            l.verify_prepared('0.1.2', 'source', output)
        rehearsed(output)
        record = json.loads((output / l.REHEARSAL).read_text())
        record['to'] = '0.1.1'
        (output / l.REHEARSAL).write_text(json.dumps(record))
        with self.assertRaisesRegex(RuntimeError, 'other bytes'):
            l.verify_prepared('0.1.2', 'source', output)
        for crashes in (None, ['planned', 'bg_stopped']):
            rehearsed(output)
            record = json.loads((output / l.REHEARSAL).read_text())
            if crashes is None:
                del record['cases']
            else:
                record['cases'][l.SDK_HANDOVER]['crashed_after'] = crashes
            (output / l.REHEARSAL).write_text(json.dumps(record))
            with self.assertRaisesRegex(RuntimeError, 'Agent SDK'):
                l.verify_prepared('0.1.2', 'source', output)
        rehearsed(output)
        record = json.loads((output / l.REHEARSAL).read_text())
        del record['cases'][l.FRESH_SDK]
        (output / l.REHEARSAL).write_text(json.dumps(record))
        with self.assertRaisesRegex(RuntimeError, 'fresh install'):
            l.verify_prepared('0.1.2', 'source', output)
        rehearsed(output)
        record = json.loads((output / l.REHEARSAL).read_text())
        record['from'] = '0.3.0'
        del record['cases'][l.SDK_HANDOVER]
        (output / l.REHEARSAL).write_text(json.dumps(record))
        l.verify_prepared('0.1.2', 'source', output)  # 0.3.0 leaves no claude --bg node to move
        for reader in (None, {'doctor_exit_code': 1, 'options_sha256': 'a' * 64}, {'doctor_exit_code': 0}):
            rehearsed(output)
            record = json.loads((output / l.REHEARSAL).read_text())
            record['cases'][l.PREVIOUS_DOCTOR] = reader
            (output / l.REHEARSAL).write_text(json.dumps(record))
            with self.assertRaisesRegex(RuntimeError, 'previous doctor'):
                l.verify_prepared('0.1.2', 'source', output)
        rehearsed(output)
        l.verify_prepared('0.1.2', 'source', output)
        self.assertTrue(archive.exists())

