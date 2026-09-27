"""Fast lifecycle safety oracles: no Brew, daemon, network or VM."""
from contextlib import closing
import importlib.util
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE / 'macos'))
import cli_lifecycle as lifecycle


class LifecycleContracts(unittest.TestCase):
    def test_snapshot_includes_committed_wal_and_is_independent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); original = root / 'original'; original.mkdir()
            (original / 'marker').write_text('before')
            with closing(sqlite3.connect(original / 'alondra.db')) as db:
                db.execute('PRAGMA journal_mode=WAL')
                db.execute('CREATE TABLE event_log(value TEXT)'); db.commit()
                db.execute("INSERT INTO event_log VALUES ('committed in WAL')"); db.commit()
                self.assertTrue((original / 'alondra.db-wal').exists())
                target = root / 'snapshot'; hashes = lifecycle.snapshot(original, target)
                with closing(sqlite3.connect(target / 'alondra.db')) as saved:
                    self.assertEqual(saved.execute('SELECT value FROM event_log').fetchall(), [('committed in WAL',)])
                db.execute("INSERT INTO event_log VALUES ('post-upgrade')"); db.commit()
                self.assertEqual(lifecycle.state_manifest(target), hashes)
                self.assertNotIn('alondra.db-wal', hashes)
                self.assertEqual((target / 'marker').read_text(), 'before')

    def test_snapshot_refuses_links_and_never_overwrites_existing_backup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); original = root / 'original'; original.mkdir()
            outside = root / 'outside'; outside.write_text('must remain')
            (original / 'link').symlink_to(outside)
            with self.assertRaisesRegex(RuntimeError, 'link'): lifecycle.snapshot(original, root / 'backup')
            self.assertEqual(outside.read_text(), 'must remain')
            (original / 'link').unlink()
            with self.assertRaises(FileExistsError): lifecycle.snapshot(original, root / 'backup')

    def test_negative_downgrade_requires_specific_event_rejection(self):
        lifecycle.reject_unsupported_downgrade(1, 'event 4 unreadable: unknown variant `splits_released`')
        for code, output in [(0, 'unknown variant `splits_released`'), (1, 'permission denied'),
                             (1, 'database locked'), (1, 'unknown variant other_event'),
                             (1, 'unknown variant `other`, expected splits_released'),
                             (1, 'could not open splits_released')]:
            with self.subTest(code=code, output=output), self.assertRaises(RuntimeError):
                lifecycle.reject_unsupported_downgrade(code, output)

    def test_post_upgrade_original_is_not_the_rollback_snapshot(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); state = root / 'state'; state.mkdir()
            (state / 'marker').write_text('018')
            pre = root / 'pre'; hashes = lifecycle.snapshot(state, pre)
            (state / 'marker').write_text('019')
            preserved = root / 'post'; state.rename(preserved)
            saved = lifecycle.state_manifest(preserved)
            import shutil
            shutil.copytree(pre, state)
            self.assertEqual(lifecycle.state_manifest(state), hashes)
            self.assertEqual(lifecycle.state_manifest(preserved), saved)
            self.assertEqual((preserved / 'marker').read_text(), '019')
            self.assertEqual((state / 'marker').read_text(), '018')

    def test_plan_never_starts_tools_and_rejects_mismatched_profiles(self):
        spec = importlib.util.spec_from_file_location('lifecycle_tart', HERE / 'macos.py')
        gate = importlib.util.module_from_spec(spec); spec.loader.exec_module(gate)
        with tempfile.TemporaryDirectory() as directory:
            args = ['macos.py', '--brew-lifecycle', '--brew-version', '0.1.9', '--output-dir', str(Path(directory) / 'absent')]
            with patch.object(sys, 'argv', args), patch('subprocess.run') as run, patch('subprocess.Popen') as popen:
                self.assertEqual(gate.main(), 0)
                run.assert_not_called(); popen.assert_not_called()
                self.assertFalse((Path(directory) / 'absent').exists())
            with patch.object(sys, 'argv', [*args, '--profile', 'app']), self.assertRaises(SystemExit): gate.main()


if __name__ == '__main__': unittest.main()
