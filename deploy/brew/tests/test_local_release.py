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
        values = {('git', 'branch', '--show-current'): 'main',
                  ('git', 'status', '--porcelain'): '',
                  ('git', 'rev-parse', 'HEAD'): 'source',
                  ('git', 'rev-parse', 'origin/main'): 'source'}
        return values.get(args, '')

    def test_preflight_rejects_dirty_branch_diverged_and_wrong_version_before_mutations(self):
        for failing, answer in [(('git', 'branch', '--show-current'), 'feature/change'),
                                (('git', 'status', '--porcelain'), ' M file'),
                                (('git', 'rev-parse', 'origin/main'), 'other')]:
            with self.subTest(failing=failing), patch.object(l, 'run', side_effect=lambda *a, **k: answer if a == failing else self.git(*a, **k)) as runner:
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
        patch.object(l, 'disk_guard').start()
        calls = []
        def fake(*args, **kwargs):
            calls.append(args)
            if args[:2] == ('cargo', 'test'):
                self.assertEqual(kwargs['env']['CARGO_BUILD_JOBS'], '1')
                self.assertEqual(kwargs['env']['CARGO_INCREMENTAL'], '0')
                raise subprocess.CalledProcessError(1, args)
            if args[0] == 'rustup': return '\n'.join(l.release.TARGETS)
            if args[0] == 'security': return f'1) {l.IDENTITY} "local identity"'
            return 'x86_64'
        output = self.root / 'artifacts'
        with patch.object(l, 'run', side_effect=fake), self.assertRaises(subprocess.CalledProcessError):
            l.prepare('0.1.2', 'source', output, self.root / 'target')
        self.assertFalse((output / 'manifest.json').exists())
        self.assertFalse(any(c[0] in ('git', 'codesign') for c in calls))

    def test_low_disk_stops_before_build(self):
        usage = type('Usage', (), {'free': l.MIN_FREE - 1})()
        with patch.object(l.shutil, 'disk_usage', return_value=usage), self.assertRaises(RuntimeError):
            l.disk_guard(self.root / 'not-created')
        self.assertFalse((self.root / 'not-created').exists())

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
            self.assertNotIn(('git', 'branch', '--show-current'), [c.args for c in runner.call_args_list])
        with patch.object(l, 'run', side_effect=self.git), self.assertRaises(RuntimeError):
            l.preflight('0.1.2', retry_source='different-manifest-source')
        def not_merged(*args, **kwargs):
            if args[:2] == ('git', 'merge-base'): raise subprocess.CalledProcessError(1, args)
            return self.git(*args, **kwargs)
        with patch.object(l, 'run', side_effect=not_merged), self.assertRaises(subprocess.CalledProcessError):
            l.preflight('0.1.2', retry_source='source')
