import importlib.util
import io
import json
import os
import pathlib
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = pathlib.Path(__file__).parents[1] / 'local-release.py'
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


class LocalReleaseTests(unittest.TestCase):
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
        patch.object(l, 'rust_toolchain', return_value=('stable', {'cargo': '/pinned/cargo', 'rustc': '/pinned/rustc', 'rustdoc': '/pinned/rustdoc'})).start()
        calls = []
        def fake(*args, **kwargs):
            calls.append(args)
            if args[:2] == ('bazelisk', 'test'):
                raise subprocess.CalledProcessError(1, args)
            if args[0] == 'rustup': return '\n'.join(l.release.TARGETS)
            if args[0] == 'security': return f'1) {l.IDENTITY} "local identity"'
            return 'x86_64'
        output = self.root / 'artifacts'
        with patch.object(l, 'run', side_effect=fake), self.assertRaises(subprocess.CalledProcessError):
            l.prepare('0.1.2', 'source', output, self.root / 'target')
        self.assertFalse((output / 'manifest.json').exists())
        self.assertFalse(any(c[0] in ('git', 'codesign') for c in calls))

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

    def test_prepare_rejects_low_disk_before_toolchain_or_artifact_creation(self):
        patch.object(l.platform, 'system', return_value='Darwin').start()
        patch.object(l.platform, 'machine', return_value='arm64').start()
        insufficient = type('Usage', (), {'free': l.LOW_FREE - 1})()
        output = self.root / 'artifacts'
        with patch.object(l.shutil, 'disk_usage', return_value=insufficient), \
             patch.object(l, 'rust_toolchain') as toolchain, \
             self.assertRaisesRegex(RuntimeError, 'At least 8 GiB must be free'):
            l.prepare('0.1.2', 'source', output, self.root / 'target')
        toolchain.assert_not_called()
        self.assertFalse(output.exists())

    def test_retry_only_publishes_verified_original_artifacts(self):
        output = self.root / 'artifacts'
        output.mkdir()
        archives(output)
        (output / 'manifest.json').write_text(json.dumps(l.manifest('0.1.2', 'source', output)))
        with patch.dict(os.environ, {'ALONDRA_RELEASE_DIR': str(self.root / 'artifacts'), 'CARGO_TARGET_DIR': str(self.root / 'target')}), \
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


    def toolchain_fixture(self):
        tools = {}
        for name in ('cargo', 'rustc', 'rustdoc'):
            path = self.root / 'pinned/bin' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('#!/bin/sh\nexit 0\n')
            path.chmod(0o755)
            tools[name] = str(path)
        for target in l.release.TARGETS:
            library = self.root / 'pinned/lib/rustlib' / target / 'lib'
            library.mkdir(parents=True)
            for crate in ('std', 'core'): (library / f'lib{crate}-fixture.rlib').touch()
        def resolve(*args, **kwargs):
            if args == ('rustup', 'show', 'active-toolchain'): return 'stable-aarch64-apple-darwin (default)'
            if args[:3] == ('rustup', 'which', '--toolchain'):
                self.assertEqual(args[3], 'stable-aarch64-apple-darwin')
                return tools[args[4]]
            if args[:2] == (tools['rustc'], '--print'):
                return str(self.root / 'pinned/lib/rustlib' / args[-1] / 'lib')
            if args == (tools['rustc'], '--version'): return 'rustc fixture'
            raise AssertionError(f'Unexpected command: {args}')
        return tools, resolve

    def test_rustup_selection_ignores_conflicting_path_and_checks_actual_compiler_targets(self):
        tools, resolve = self.toolchain_fixture()
        with patch.dict(os.environ, {'PATH': '/opt/homebrew/bin', 'RUSTC': '/opt/homebrew/bin/rustc'}), patch.object(l, 'run', side_effect=resolve):
            name, selected = l.rust_toolchain()
        self.assertEqual(selected, tools)
        self.assertEqual(name, 'stable-aarch64-apple-darwin')
        (self.root / 'pinned/lib/rustlib/aarch64-apple-darwin/lib/libcore-fixture.rlib').unlink()
        with patch.object(l, 'run', side_effect=resolve), self.assertRaisesRegex(RuntimeError, 'Missing target libraries for aarch64'):
            l.rust_toolchain()

    def test_build_and_isolated_home_tests_use_same_pinned_tools(self):
        tools, _ = self.toolchain_fixture()
        output, target_dir = self.root / 'artifacts', self.root / 'target'
        rust_calls = []
        def fake(*args, **kwargs):
            if args[0] == tools['cargo']:
                rust_calls.append((args, kwargs['env']))
                if args[1] == 'build':
                    target = args[args.index('--target') + 1]
                    directory = target_dir / target / 'release'
                    directory.mkdir(parents=True)
                    for name in ('alondra', 'alondrad'): (directory / name).touch()
            elif args[0] == 'security': return f'1) {l.IDENTITY} "local identity"'
            elif args[0] == 'bash': archives(output)
            elif args[0] == '/usr/bin/arch': return 'x86_64'
            return ''
        with patch.dict(os.environ, {'PATH': '/opt/homebrew/bin:/usr/bin', 'RUSTC': '/opt/homebrew/bin/rustc', 'RUSTDOC': '/opt/homebrew/bin/rustdoc', 'RUSTC_WRAPPER': 'unwanted-wrapper'}), \
             patch.object(l.platform, 'system', return_value='Darwin'), patch.object(l.platform, 'machine', return_value='arm64'), \
             patch.object(l, 'rust_toolchain', return_value=('pinned', tools)), patch.object(l, 'require_disk_space'), \
             patch.object(l, 'sign'), patch.object(l, 'smoke'), patch.object(l, 'run', side_effect=fake):
            l.prepare('0.1.2', 'source', output, target_dir)
        self.assertEqual(len(rust_calls), len(l.release.TARGETS))
        self.assertEqual({args[args.index('--target') + 1] for args, _ in rust_calls}, set(l.release.TARGETS))
        for args, environment in rust_calls:
            self.assertEqual(args[0], tools['cargo'])
            self.assertEqual(environment['RUSTC'], tools['rustc'])
            self.assertEqual(environment['RUSTDOC'], tools['rustdoc'])
            self.assertEqual(environment['CARGO'], tools['cargo'])
            self.assertEqual(environment['RUSTUP_TOOLCHAIN'], 'pinned')
            self.assertEqual(environment['RUSTC_WRAPPER'], '')
            self.assertEqual(environment['RUSTC_WORKSPACE_WRAPPER'], '')
            self.assertEqual(environment['PATH'].split(os.pathsep)[0], str(self.root / 'pinned/bin'))
            if args[1] == 'test':
                self.assertNotEqual(environment['HOME'], str(pathlib.Path.home()))
                self.assertNotEqual(environment['CARGO_HOME'], str(pathlib.Path(environment['HOME']) / '.cargo'))
                self.assertNotEqual(environment['RUSTUP_HOME'], str(pathlib.Path(environment['HOME']) / '.rustup'))

    def ephemeral_fixture(self, *, failure=None):
        tools, _ = self.toolchain_fixture()
        target = self.root / 'shared-target'
        target.mkdir()
        sentinel = target / 'owner-cache'
        sentinel.write_bytes(b'preserve user cache')
        output = self.root / 'artifacts'
        caches, stages, archive_bytes = [], [], {}
        def fake(*args, **kwargs):
            if args[0] == tools['cargo']:
                phase = args[1]
                cache = pathlib.Path(kwargs['env']['CARGO_TARGET_DIR'])
                self.assertNotEqual(cache, target)
                self.assertEqual(cache.parent, target.parent)
                self.assertTrue(cache.is_dir())
                self.assertTrue(all(not prior.exists() for prior in caches))
                caches.append(cache)
                stages.append(phase)
                (cache / 'owned-cache').write_bytes(b'cache bytes')
                (cache / 'outside-link').symlink_to(sentinel)
                self.assertEqual(kwargs['env']['CARGO_INCREMENTAL'], '0')
                self.assertIn('--locked', args)
                if phase == failure:
                    raise subprocess.CalledProcessError(1, args)
                if phase == 'build':
                    arch = args[args.index('--target') + 1]
                    directory = cache / arch / 'release'
                    directory.mkdir(parents=True)
                    for name in ('alondra', 'alondrad'):
                        (directory / name).write_bytes(f'{arch}:{name}'.encode())
            elif args[0] == 'security':
                return f'1) {l.IDENTITY} "local identity"'
            elif args[0] == '/usr/bin/arch':
                return 'x86_64'
            elif args[0] == 'bash':
                arch = args[3]
                path = output / f'alondra-0.1.2-{arch}.tar.gz'
                self.assertFalse(path.exists())
                with tarfile.open(path, 'w:gz') as archive:
                    for name in sorted(l.release.PUBLIC_FILES):
                        payload = pathlib.Path(args[4]).parent / name
                        data = payload.read_bytes() if name in ('alondra', 'alondrad') else name.encode()
                        item = tarfile.TarInfo(name)
                        item.size = len(data)
                        archive.addfile(item, io.BytesIO(data))
            return ''
        def sign(binary, identifier):
            binary.write_bytes(binary.read_bytes() + b':signed')
        def smoke(ver, arch, archive):
            self.assertTrue(caches[-1].exists(), 'build cache survives through archive smoke')
            self.assertFalse((output / 'manifest.json').exists())
            l.release.validate_archive(archive)
            for name, data in archive_bytes.items():
                self.assertEqual((output / name).read_bytes(), data)
            archive_bytes[archive.name] = archive.read_bytes()
            with tarfile.open(archive) as package:
                for name in ('alondra', 'alondrad'):
                    self.assertTrue(package.extractfile(name).read().endswith(b':signed'))
            if failure == 'smoke':
                raise RuntimeError('smoke failed')
        guards = []
        def guard(path):
            guards.append(path)
        patch.object(l.platform, 'system', return_value='Darwin').start()
        patch.object(l.platform, 'machine', return_value='arm64').start()
        patch.object(l, 'rust_toolchain', return_value=('pinned', tools)).start()
        patch.object(l, 'run', side_effect=fake).start()
        patch.object(l, 'sign', side_effect=sign).start()
        patch.object(l, 'smoke', side_effect=smoke).start()
        patch.object(l, 'require_disk_space', side_effect=guard).start()
        return output, target, sentinel, caches, stages, archive_bytes, guards

    def test_ephemeral_caches_are_freed_per_stage_and_exact_signed_archives_survive(self):
        output, target, sentinel, caches, stages, originals, guards = self.ephemeral_fixture()
        l.prepare('0.1.2', 'source', output, target, ephemeral_build_cache=True)
        self.assertEqual(stages, ['build'] * len(l.release.TARGETS))
        self.assertEqual(len(set(caches)), len(l.release.TARGETS))
        self.assertTrue(all(not cache.exists() for cache in caches))
        self.assertEqual(sentinel.read_bytes(), b'preserve user cache')
        self.assertEqual(list(target.iterdir()), [sentinel])
        self.assertEqual(len(guards), 2 + len(l.release.TARGETS), 'initial target/output plus every build stage')
        self.assertEqual(len(originals), len(l.release.TARGETS))
        for name, original in originals.items():
            self.assertEqual((output / name).read_bytes(), original)
        l.verify_prepared('0.1.2', 'source', output)

    def test_failed_ephemeral_build_cleans_only_its_cache(self):
        self.check_ephemeral_failure('build')

    def test_failed_ephemeral_smoke_keeps_original_archive_without_manifest(self):
        self.check_ephemeral_failure('smoke')

    def check_ephemeral_failure(self, phase):
        output, target, sentinel, caches, stages, originals, _ = self.ephemeral_fixture(failure=phase)
        with self.assertRaises((subprocess.CalledProcessError, RuntimeError)):
            l.prepare('0.1.2', 'source', output, target, ephemeral_build_cache=True)
        self.assertTrue(caches)
        self.assertTrue(all(not cache.exists() for cache in caches))
        self.assertEqual(sentinel.read_bytes(), b'preserve user cache')
        self.assertEqual(list(target.iterdir()), [sentinel])
        self.assertFalse((output / 'manifest.json').exists())
        if phase == 'smoke':
            self.assertEqual(len(originals), 1)
            for name, original in originals.items():
                self.assertEqual((output / name).read_bytes(), original)

    def test_retry_rejects_ephemeral_flag_before_any_files_or_checks(self):
        from contextlib import redirect_stderr
        untouched = self.root / 'not-created' / 'target'
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': str(untouched)}), \
             patch.object(sys, 'argv', ['local-release.py', '0.1.2', '--retry-publish', '--ephemeral-build-cache']), \
             patch.object(l, 'preflight') as preflight, patch.object(l, 'build_cache') as cache, \
             redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as stopped:
            l.main()
        self.assertEqual(stopped.exception.code, 2)
        preflight.assert_not_called()
        cache.assert_not_called()
        self.assertFalse(untouched.parent.exists())


    def test_ephemeral_target_mountpoint_is_rejected_before_creating_cache(self):
        target = self.root / 'mounted-target'
        target.mkdir()
        sentinel = target / 'owner-file'
        sentinel.write_bytes(b'unchanged')
        original_stat = pathlib.Path.stat
        def mounted_stat(path, *args, **kwargs):
            result = original_stat(path, *args, **kwargs)
            if path == target:
                fields = list(result)
                fields[2] = result.st_dev + 1
                return os.stat_result(fields)
            return result
        with patch.object(pathlib.Path, 'stat', mounted_stat), \
             patch.object(l.tempfile, 'TemporaryDirectory') as temporary:
            with self.assertRaisesRegex(RuntimeError, 'mount point'):
                with l.build_cache(target, True, 'test'):
                    self.fail('cross-volume cache must not be created')
            temporary.assert_not_called()
        self.assertEqual(sentinel.read_bytes(), b'unchanged')
        self.assertEqual(list(target.iterdir()), [sentinel])
