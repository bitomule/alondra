import base64
import importlib.util
import io
import pathlib
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('release', pathlib.Path(__file__).parents[1] / 'release.py')
r = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(r)


def archive(path, extra=None):
    with tarfile.open(path, 'w:gz') as output:
        for name in sorted(r.PUBLIC_FILES):
            item = tarfile.TarInfo(name)
            item.size = len(name)
            output.addfile(item, io.BytesIO(name.encode()))
        if extra:
            output.addfile(extra, io.BytesIO(b''))


class PublicPayloadTests(unittest.TestCase):
    def test_rejects_private_source_and_links_even_under_allowed_names(self):
        with tempfile.TemporaryDirectory() as temp:
            path = pathlib.Path(temp) / 'test.tar.gz'
            for name in ('crates/alondra/src/main.rs', '../secret', '.env', 'Alondra.app/Contents/MacOS/Alondra'):
                archive(path, tarfile.TarInfo(name))
                with self.assertRaises(ValueError):
                    r.validate_archive(path)
            archive(path)
            r.validate_archive(path)
            with tarfile.open(path, 'w:gz') as output:
                link = tarfile.TarInfo('alondra')
                link.type = tarfile.SYMTYPE
                link.linkname = '/etc/passwd'
                output.addfile(link)
            with self.assertRaises(ValueError):
                r.validate_archive(path)

    def test_rejects_missing_and_duplicate_files(self):
        with tempfile.TemporaryDirectory() as temp:
            path = pathlib.Path(temp) / 'test.tar.gz'
            with tarfile.open(path, 'w:gz'):
                pass
            with self.assertRaises(ValueError):
                r.validate_archive(path)
            archive(path, tarfile.TarInfo('alondra'))
            with self.assertRaises(ValueError):
                r.validate_archive(path)

    def test_version_cannot_inject_shell_or_formula_syntax(self):
        for value in ('1.0', '01.0.0', '1.0.0-beta', '1.2.3\n', '$(whoami)', '1.0.0"'):
            with self.assertRaises(ValueError):
                r.version(value)


class PublicationTests(unittest.TestCase):
    """Fake remote state exercises partial failure and retry, with actual payload hashes."""
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = pathlib.Path(self.temp.name)
        self.ver = '0.1.0'
        for target in r.TARGETS:
            archive(self.path / f'alondra-{self.ver}-{target}.tar.gz')
        self.remote = None
        self.assets = {}
        self.formula = None
        self.fail_formula_once = False
        self.uploads = 0
        self.patchers = [patch.object(r, 'optional_api', self.optional_api), patch.object(r, 'api', self.api), patch.object(r, 'gh', self.gh)]
        for p in self.patchers:
            p.start()
            self.addCleanup(p.stop)

    def optional_api(self, endpoint):
        return self.api(endpoint)

    def api(self, endpoint, payload=None):
        if '/contents/' in endpoint:
            if payload:
                self.assertFalse(self.remote['draft'])
                self.assertEqual(len(self.assets), 2)
                if self.fail_formula_once:
                    self.fail_formula_once = False
                    raise RuntimeError('simulated concurrent tap update')
                self.formula = {'content': payload['content'], 'sha': 'new-sha'}
            return self.formula
        if self.remote is None:
            return None
        return {**self.remote, 'assets': [{'name': name} for name in self.assets]}

    def gh(self, *args, **kwargs):
        operation = args[1]
        if operation == 'create':
            self.remote = {'draft': True}
        elif operation == 'upload':
            self.assertTrue(self.remote['draft'])
            path = pathlib.Path(args[3])
            self.assertNotIn(path.name, self.assets)
            self.assets[path.name] = path.read_bytes()
            self.uploads += 1
        elif operation == 'download':
            name = args[args.index('--pattern') + 1]
            directory = args[args.index('--dir') + 1]
            (pathlib.Path(directory) / name).write_bytes(self.assets[name])
        elif operation == 'edit':
            self.remote['draft'] = False
        else:
            self.fail(f'Unexpected gh operation: {args}')
        return ''

    def test_retry_after_formula_failure_reuses_immutable_assets(self):
        self.fail_formula_once = True
        with self.assertRaises(RuntimeError):
            r.publish(self.ver, self.path)
        self.assertEqual(self.uploads, 2)
        self.assertIsNone(self.formula)
        r.publish(self.ver, self.path)
        r.publish(self.ver, self.path)
        self.assertEqual(self.uploads, 2)
        formula = base64.b64decode(self.formula['content']).decode()
        self.assertNotIn('@VERSION@', formula)
        for target in r.TARGETS:
            self.assertIn(r.sha(self.path / f'alondra-{self.ver}-{target}.tar.gz'), formula)

    def test_retry_after_partial_upload_completes_without_replacing(self):
        self.remote = {'draft': True}
        first = self.path / f'alondra-{self.ver}-{r.TARGETS[0]}.tar.gz'
        self.assets[first.name] = first.read_bytes()
        r.publish(self.ver, self.path)
        self.assertEqual(self.uploads, 1)
        self.assertFalse(self.remote['draft'])

    def test_different_existing_asset_fails_before_formula_or_replacement(self):
        self.remote = {'draft': True}
        self.assets[f'alondra-{self.ver}-{r.TARGETS[0]}.tar.gz'] = b'older-signed-build'
        with self.assertRaisesRegex(ValueError, 'Immutable asset differs'):
            r.publish(self.ver, self.path)
        self.assertEqual(self.uploads, 0)
        self.assertIsNone(self.formula)

    def test_older_release_cannot_downgrade_formula(self):
        self.formula = {'content': base64.b64encode(b'  version "1.0.0"\n').decode(), 'sha': 'existing'}
        with self.assertRaisesRegex(ValueError, 'downgrade'):
            r.publish(self.ver, self.path)
        self.assertIsNone(self.remote)

    def test_bad_payload_rejected_before_any_public_mutation(self):
        archive(self.path / f'alondra-{self.ver}-{r.TARGETS[1]}.tar.gz', tarfile.TarInfo('Cargo.toml'))
        with self.assertRaises(ValueError):
            r.publish(self.ver, self.path)
        self.assertIsNone(self.remote)
        self.assertEqual(self.uploads, 0)


if __name__ == '__main__':
    unittest.main()
