import base64
import importlib.util
import io
import json
import pathlib
import re
import tarfile
import tempfile
import tomllib
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
    def test_real_tracked_assets_materialize_known_wrapper_without_links(self):
        with tempfile.TemporaryDirectory() as temp:
            stage = pathlib.Path(temp)
            r.stage_assets(r.ROOT, stage)
            files = {str(p.relative_to(stage)) for p in stage.rglob('*') if p.is_file()}
            self.assertEqual(files, r.PUBLIC_FILES - r.BINARIES - r.HOST_MANIFESTS)
            self.assertFalse(any(p.is_symlink() for p in stage.rglob('*')))
            wrapper = stage / 'plugin-orchestrator/bin/alondra'
            self.assertEqual(wrapper.read_bytes(), (r.ROOT / 'plugin/bin/alondra').read_bytes())
            self.assertTrue(wrapper.stat().st_mode & 0o111)
            # Validate a package with the actual tracked runtime assets, rather
            # than only generated fixture names that missed the original link.
            (stage / 'alondra').write_bytes(b'fake binary')
            (stage / 'alondrad').write_bytes(b'fake daemon')
            (stage / 'alondrad-launcher').write_bytes(b'fake launcher')
            built = stage / 'built.json'
            built.write_text(json.dumps({'laboratory': False, 'sdk': '0.3.300', 'node': 'v24.19.0', 'bundle_sha256': 'b' * 64}))
            host = pathlib.Path(temp) / 'alondra-host'
            host.write_bytes(b'fake host')
            r.stage_host(host, built, stage)
            r.stage_host(host, built, stage, provider='codex')
            codex = json.loads((stage / r.CODEX_MANIFEST).read_text())
            self.assertEqual(codex['sha256'],r.sha(stage / r.CODEX_HOST))
            self.assertEqual((stage / r.CODEX_HOST).read_bytes(),host.read_bytes())
            self.assertNotIn('codex',r.PRE_CODEX_FILES)
            built.unlink()
            manifest = json.loads((stage / r.HOST_MANIFEST).read_text())
            self.assertEqual(manifest, {'sdk': '0.3.300', 'node': 'v24.19.0', 'bundle_sha256': 'b' * 64, 'sha256': r.sha(host)})
            package = stage / 'package.tar.gz'
            with tarfile.open(package, 'w:gz') as output:
                for name in sorted(r.PUBLIC_FILES):
                    output.add(stage / name, arcname=name)
            r.validate_archive(package)

    def test_a_laboratory_host_is_never_staged(self):
        with tempfile.TemporaryDirectory() as temp:
            built = pathlib.Path(temp) / 'built.json'
            built.write_text(json.dumps({'laboratory': True, 'sdk': '0.3.300', 'node': 'v24.19.0', 'bundle_sha256': 'b' * 64}))
            (pathlib.Path(temp) / 'host').write_bytes(b'host')
            with self.assertRaisesRegex(ValueError, 'release host'):
                r.stage_host(pathlib.Path(temp) / 'host', built, pathlib.Path(temp) / 'stage')

    def test_a_published_release_without_the_sdk_host_passes_only_as_an_older_set(self):
        with tempfile.TemporaryDirectory() as temp:
            path = pathlib.Path(temp) / 'test.tar.gz'
            with tarfile.open(path, 'w:gz') as output:
                for name in sorted(r.PRE_SDK_FILES):
                    item = tarfile.TarInfo(name)
                    item.size = len(name)
                    output.addfile(item, io.BytesIO(name.encode()))
            with self.assertRaisesRegex(ValueError, 'alondra-host'):
                r.validate_archive(path)
            r.validate_archive(path, r.PUBLISHED_FILES)

    def test_bazel_stamps_the_version_cargo_ships(self):
        cargo = tomllib.loads((r.ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
        stamped = re.search(r'^VERSION = "([^"]+)"$', (r.ROOT / 'version.bzl').read_text(), re.M)[1]
        self.assertEqual(stamped, cargo, 'bump version.bzl with the workspace version')

    def test_a_release_without_the_launcher_passes_only_when_an_older_set_is_allowed(self):
        with tempfile.TemporaryDirectory() as temp:
            path = pathlib.Path(temp) / 'test.tar.gz'
            with tarfile.open(path, 'w:gz') as output:
                for name in sorted(r.PRE_LAUNCHER_FILES):
                    item = tarfile.TarInfo(name)
                    item.size = len(name)
                    output.addfile(item, io.BytesIO(name.encode()))
            with self.assertRaisesRegex(ValueError, 'alondrad-launcher'):
                r.validate_archive(path)
            r.validate_archive(path, (r.PUBLIC_FILES, r.PRE_LAUNCHER_FILES))
            archive(path, tarfile.TarInfo('notes.txt'))
            with self.assertRaises(ValueError):
                r.validate_archive(path, (r.PUBLIC_FILES, r.PRE_LAUNCHER_FILES))

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
        self.patchers = [patch.object(r, 'optional_api', self.optional_api), patch.object(r, 'api', self.api), patch.object(r, 'gh', self.gh), patch.object(r, 'download_asset', self.download)]
        for p in self.patchers:
            p.start()
            self.addCleanup(p.stop)

    def optional_api(self, endpoint):
        return self.api(endpoint)

    def asset_records(self):
        return [{'id': i + 100, 'name': name, 'state': 'uploaded'} for i, name in enumerate(self.assets)]

    def api(self, endpoint, payload=None, method=None):
        if '/releases/tags/' in endpoint:
            raise RuntimeError('gh: Not Found (HTTP 404): draft has no published tag')
        if endpoint == f'repos/{r.REPO}':
            return {'default_branch': 'master'}
        if '/contents/' in endpoint:
            if payload:
                self.assertFalse(self.remote['draft'])
                self.assertEqual(len(self.assets), len(r.TARGETS))
                if self.formula:
                    self.assertEqual(payload.get('sha'), self.formula['sha'], 'formula updates must use original blob CAS')
                if self.fail_formula_once:
                    self.fail_formula_once = False
                    raise RuntimeError('simulated concurrent tap update')
                self.formula = {'content': payload['content'], 'sha': 'new-sha'}
            return self.formula
        if endpoint == f'repos/{r.REPO}/releases' and method == 'POST':
            self.assertIsNone(self.remote)
            self.remote = {'id': 42, 'tag_name': payload['tag_name'], 'draft': True}
            return dict(self.remote)
        if endpoint.startswith(f'repos/{r.REPO}/releases?'):
            return [dict(self.remote, id=42, tag_name='alondra-v0.1.0')] if self.remote else []
        if endpoint.startswith(f'repos/{r.REPO}/releases/42/assets?'):
            return self.asset_records()
        if endpoint == f'repos/{r.REPO}/releases/42':
            if method == 'PATCH':
                self.assertEqual(payload, {'draft': False, 'make_latest': 'false'})
                self.assertEqual(len(self.assets), len(r.TARGETS))
                self.remote['draft'] = False
            return dict(self.remote, id=42, tag_name='alondra-v0.1.0')
        self.fail(f'Unexpected REST call: {endpoint} {method}')

    def gh(self, *args, **kwargs):
        self.assertEqual(args[:3], ('api', '--hostname', 'uploads.github.com'))
        self.assertEqual(args[3].split('?')[0], f'https://uploads.github.com/repos/{r.REPO}/releases/42/assets')
        self.assertIn('Content-Type: application/octet-stream', args)
        self.assertEqual(args[args.index('--method') + 1], 'POST')
        self.assertTrue(self.remote['draft'])
        path = pathlib.Path(args[args.index('--input') + 1])
        self.assertNotIn(path.name, self.assets)
        self.assets[path.name] = path.read_bytes()
        self.uploads += 1
        return json.dumps(self.asset_records()[-1])

    def download(self, asset_id, destination):
        name = next(asset['name'] for asset in self.asset_records() if asset['id'] == asset_id)
        destination.write_bytes(self.assets[name])

    def test_release_lookup_uses_database_id_for_unpublished_draft(self):
        self.remote = {'draft': True}
        self.assertTrue(r.lookup_release('alondra-v0.1.0')['draft'])
        with self.assertRaisesRegex(RuntimeError, 'HTTP 404'):
            self.api(f'repos/{r.REPO}/releases/tags/alondra-v0.1.0')

    def test_release_lookup_does_not_treat_auth_or_network_errors_as_absence(self):
        for message in ('HTTP 401: Bad credentials', 'network timeout', 'HTTP 403: forbidden', '(HTTP 404)'):
            with patch.object(r, 'api', side_effect=RuntimeError(message)):
                with self.assertRaises(RuntimeError):
                    r.publish(self.ver, self.path)
        self.assertIsNone(self.remote)
        self.assertEqual(self.uploads, 0)

    def test_rest_list_errors_and_invalid_draft_ids_never_create_release(self):
        for response in ({'message': 'Bad credentials'}, None, [{'tag_name': 'alondra-v0.1.0', 'id': None}]):
            def remote(endpoint, payload=None, method=None):
                if endpoint == f'repos/{r.REPO}': return {}
                self.assertIsNone(payload, 'failed discovery cannot mutate remote state')
                return response
            with patch.object(r, 'api', side_effect=remote):
                with self.assertRaises(RuntimeError):
                    r.publish(self.ver, self.path)
        self.assertIsNone(self.remote)
        self.assertEqual(self.uploads, 0)

    def test_draft_lookup_paginates_before_proving_absence(self):
        pages_seen = []
        def remote(endpoint, payload=None, method=None):
            if endpoint == f'repos/{r.REPO}': return {}
            if endpoint.endswith('/42'): return {'id':42, 'tag_name':'alondra-v0.1.0', 'draft':True}
            pages_seen.append(endpoint)
            if endpoint.endswith('&page=1'): return [{'id': i+1000, 'tag_name': f'other-{i}'} for i in range(100)]
            return [{'id':42, 'tag_name':'alondra-v0.1.0', 'draft':True}]
        with patch.object(r, 'api', side_effect=remote):
            self.assertTrue(r.lookup_release('alondra-v0.1.0')['draft'])
        self.assertEqual(len(pages_seen), 2)

    def test_retry_after_formula_failure_reuses_immutable_assets(self):
        self.fail_formula_once = True
        with self.assertRaises(RuntimeError):
            r.publish(self.ver, self.path)
        self.assertEqual(self.uploads, len(r.TARGETS))
        self.assertIsNone(self.formula)
        r.publish(self.ver, self.path)
        r.publish(self.ver, self.path)
        self.assertEqual(self.uploads, len(r.TARGETS))
        formula = base64.b64decode(self.formula['content']).decode()
        self.assertNotIn('@VERSION@', formula)
        for target in r.TARGETS:
            self.assertIn(r.sha(self.path / f'alondra-{self.ver}-{target}.tar.gz'), formula)

    def test_retry_after_uploaded_asset_completes_without_reuploading(self):
        self.remote = {'draft': True}
        first = self.path / f'alondra-{self.ver}-{r.TARGETS[0]}.tar.gz'
        self.assets[first.name] = first.read_bytes()
        r.publish(self.ver, self.path)
        self.assertEqual(self.uploads, 0)
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

    def test_existing_formula_update_uses_blob_compare_and_swap(self):
        self.formula = {'content': base64.b64encode(b'  version "0.0.9"\n').decode(), 'sha': 'original-blob'}
        r.publish(self.ver, self.path)
        self.assertEqual(self.formula['sha'], 'new-sha')

    def test_error_on_later_release_page_does_not_prove_absence(self):
        def remote(endpoint, payload=None, method=None):
            if endpoint == f'repos/{r.REPO}': return {}
            if endpoint.endswith('&page=1'):
                return [{'id': i+1000, 'tag_name': f'other-{i}'} for i in range(100)]
            raise RuntimeError('upstream timeout page two')
        with patch.object(r, 'api', side_effect=remote):
            with self.assertRaisesRegex(RuntimeError, 'page two'):
                r.publish(self.ver, self.path)
        self.assertIsNone(self.remote)
        self.assertEqual(self.uploads, 0)

    def test_published_incomplete_or_unexpected_assets_are_never_modified(self):
        for draft, name in [(False, f'alondra-{self.ver}-{r.TARGETS[0]}.tar.gz'), (True, 'unexpected-source.zip')]:
            self.remote = {'draft': draft}
            self.assets = {name: b'existing'}
            with self.assertRaises(ValueError): r.publish(self.ver, self.path)
            self.assertEqual(self.uploads, 0)
            self.assertIsNone(self.formula)

    def test_downloaded_upload_bytes_must_match_before_publication(self):
        with patch.object(r, 'download_asset', side_effect=lambda _id, path: path.write_bytes(b'corrupt')):
            with self.assertRaisesRegex(ValueError, 'Immutable asset differs'):
                r.publish(self.ver, self.path)
        self.assertTrue(self.remote['draft'])
        self.assertIsNone(self.formula)

    def test_bad_payload_rejected_before_any_public_mutation(self):
        archive(self.path / f'alondra-{self.ver}-{r.TARGETS[0]}.tar.gz', tarfile.TarInfo('Cargo.toml'))
        with self.assertRaises(ValueError):
            r.publish(self.ver, self.path)
        self.assertIsNone(self.remote)
        self.assertEqual(self.uploads, 0)


class RestTransportTests(unittest.TestCase):
    def test_binary_download_preserves_non_utf8_bytes_and_uses_asset_id(self):
        binary = b'\x00\xff\x1b[31m\r\n'
        with tempfile.TemporaryDirectory() as temp:
            destination = pathlib.Path(temp) / 'asset.tar.gz'
            def run(args, **kwargs):
                self.assertEqual(args[:4], ['gh', 'api', '--hostname', 'github.com'])
                self.assertIn(f'repos/{r.REPO}/releases/assets/123', args)
                self.assertIn('Accept: application/octet-stream', args)
                self.assertNotIn('text', kwargs)
                kwargs['stdout'].write(binary)
                return type('Result', (), {'returncode': 0, 'stderr': b''})()
            with patch.object(r.subprocess, 'run', side_effect=run):
                r.download_asset(123, destination)
            self.assertEqual(destination.read_bytes(), binary)

    def test_formula_404_does_not_hide_repository_auth_failure(self):
        with patch.object(r, 'api', side_effect=[RuntimeError('gh (HTTP 404)'), RuntimeError('gh (HTTP 401)')]):
            with self.assertRaisesRegex(RuntimeError, '401'):
                r.optional_api(f'repos/{r.REPO}/contents/Formula/alondra.rb')


if __name__ == '__main__':
    unittest.main()
