import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).parents[1] / 'smoke-setup.py'
SPEC = importlib.util.spec_from_file_location('smoke_setup', SCRIPT)
s = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(s)


class ProfileSmokeTests(unittest.TestCase):
    prefix = pathlib.Path('/fixture/opt/alondra')
    home = pathlib.Path('/fixture/user')

    def plan(self, profile=None, marker=False):
        text = (f'<string>{self.prefix}/bin/alondrad</string>\n'
                f'would run: claude plugin marketplace add {self.prefix}/libexec/alondra\n'
                '(dry run: nothing was written or run)\n')
        if profile is not None:
            text += ''.join(f'<key>{key}</key><string>{profile}</string>'
                            for key in ('CLAUDE_CONFIG_DIR', 'CLAUDE_HOME'))
        if marker:
            text += '<key>ALONDRA_CLAUDE_PROFILE_MODE</key><string>explicit</string>'
        return text

    def observations(self, profile=None):
        return [['claude', ['plugin', 'list', '--json'], [str(self.home), profile, profile]],
                ['launchctl', ['print', 'gui/501/dev.alondra.alondrad'], [str(self.home), None, None]]]

    def test_implicit_unset_rejects_the_previous_forced_default_contract(self):
        s.check_plan(self.plan(), self.prefix, self.home)
        s.check_observations(self.observations(), self.home)
        old = str(self.home / '.claude')
        with self.assertRaisesRegex(RuntimeError, 'forced an implicit'):
            s.check_plan(self.plan(old), self.prefix, self.home)
        with self.assertRaisesRegex(RuntimeError, 'different user/profile'):
            s.check_observations(self.observations(old), self.home)

    def test_explicit_custom_and_explicit_default_preserve_profile_and_provenance(self):
        for profile in (str(self.home / 'custom-profile'), str(self.home / '.claude')):
            with self.subTest(profile=profile):
                s.check_plan(self.plan(profile, marker=True), self.prefix, self.home, profile)
                s.check_observations(self.observations(profile), self.home, profile)
                with self.assertRaisesRegex(RuntimeError, 'provenance'):
                    s.check_plan(self.plan(profile), self.prefix, self.home, profile)
                with self.assertRaisesRegex(RuntimeError, 'lost the explicitly requested'):
                    s.check_plan(self.plan(), self.prefix, self.home, profile)
                with self.assertRaisesRegex(RuntimeError, 'different user/profile'):
                    s.check_observations(self.observations(), self.home, profile)

    def test_profile_checks_do_not_allow_mutations_or_skip_observation(self):
        with self.assertRaisesRegex(RuntimeError, 'attempted a mutation'):
            s.check_observations([['claude', ['auth', 'login'], [str(self.home), None, None]]], self.home)
        with self.assertRaisesRegex(RuntimeError, 'did not observe'):
            s.check_observations([], self.home)
        with self.assertRaisesRegex(RuntimeError, 'daemon through the installed opt path'):
            s.check_plan(self.plan().replace('/opt/alondra/bin', '/Cellar/alondra/0.1.6/bin'), self.prefix, self.home)


if __name__ == '__main__':
    unittest.main()
