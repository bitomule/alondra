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

    def report(self, profile=None, mode=None):
        install = {'program': f'{self.prefix}/bin/alondrad', 'marketplace': f'{self.prefix}/libexec/alondra',
                   'claude_profile': profile, 'profile_mode': mode or ('explicit' if profile else 'implicit')}
        return {'checks': [{'id': 'providers', 'status': 'warn'}, {'id': 'service', 'fixable': True, 'install': install}]}

    def observations(self, profile=None):
        return [['claude', ['plugin', 'list', '--json'], [str(self.home), profile, profile]],
                ['launchctl', ['print', 'gui/501/dev.alondra.alondrad'], [str(self.home), None, None]]]

    def test_implicit_unset_rejects_the_previous_forced_default_contract(self):
        s.check_report(self.report(), self.prefix)
        s.check_observations(self.observations(), self.home)
        old = str(self.home / '.claude')
        with self.assertRaisesRegex(RuntimeError, 'forced an implicit'):
            s.check_report(self.report(old, 'implicit'), self.prefix)
        with self.assertRaisesRegex(RuntimeError, 'different user/profile'):
            s.check_observations(self.observations(old), self.home)

    def test_explicit_custom_and_explicit_default_preserve_profile_and_provenance(self):
        for profile in (str(self.home / 'custom-profile'), str(self.home / '.claude')):
            with self.subTest(profile=profile):
                s.check_report(self.report(profile), self.prefix, profile)
                s.check_observations(self.observations(profile), self.home, profile)
                with self.assertRaisesRegex(RuntimeError, 'provenance'):
                    s.check_report(self.report(profile, 'implicit'), self.prefix, profile)
                with self.assertRaisesRegex(RuntimeError, 'lost the explicitly requested'):
                    s.check_report(self.report(), self.prefix, profile)
                with self.assertRaisesRegex(RuntimeError, 'different user/profile'):
                    s.check_observations(self.observations(), self.home, profile)

    def test_profile_checks_do_not_allow_mutations_and_a_fresh_user_hears_of_providers(self):
        with self.assertRaisesRegex(RuntimeError, 'attempted a mutation'):
            s.check_observations([['claude', ['auth', 'login'], [str(self.home), None, None]]], self.home)
        s.check_observations([], self.home)
        without = self.report()
        without['checks'] = [check for check in without['checks'] if check['id'] != 'providers']
        with self.assertRaisesRegex(RuntimeError, 'did not report the AI providers'):
            s.check_report(without, self.prefix)
        cellar = self.report()
        cellar['checks'][1]['install']['program'] = '/fixture/Cellar/alondra/0.1.6/bin/alondrad'
        with self.assertRaisesRegex(RuntimeError, 'daemon through the installed opt path'):
            s.check_report(cellar, self.prefix)
        with self.assertRaisesRegex(RuntimeError, 'did not say what it would install'):
            s.check_report({'checks': [{'id': 'providers'}, {'id': 'service', 'fixable': True}]}, self.prefix)


if __name__ == '__main__':
    unittest.main()
