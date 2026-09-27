"""Shared visual scenario driver; only public, synthetic launch/control metadata."""
from __future__ import annotations
import json
from pathlib import Path
import time

CHECKS = {
    'reconnect': {'saved_restoration_observed', 'workspace_remained_selected', 'restoration_completed', 'alpha_content_loaded'},
    'account-settings': {'settings_sheet_presented', 'real_settings_account', 'alpha_selected', 'workspace_approved'},
    'mac-selection': {'selection_requested_by_settings', 'beta_selected', 'beta_content_loaded', 'late_alpha_delivered', 'late_alpha_rejected'},
}


def atomic_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def validate_report(value, run_id, scenario):
    if not isinstance(value, dict) or value.get('schema') != 1 or value.get('scope') != 'visual-native-fixture':
        raise ValueError('wrong visual report contract')
    if value.get('runID') != run_id or value.get('scenario') != scenario:
        raise ValueError('stale or mismatched visual report')
    if value.get('status') != 'passed' or value.get('error') is not None:
        raise ValueError('visual fixture did not finish successfully')
    checks = value.get('checks')
    if not isinstance(checks, dict) or set(checks) != CHECKS[scenario] or any(v is not True for v in checks.values()):
        raise ValueError('missing or failed visual fixture checks')


def wait_state(read, run_id, scenario, predicate, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = read()
        if value is not None:
            if value.get('runID') != run_id or value.get('scenario') != scenario:
                raise ValueError('stale visual fixture state')
            if value.get('status') == 'failed':
                raise ValueError('visual fixture failed')
            if predicate(value):
                return value
        time.sleep(.1)
    raise TimeoutError('visual fixture did not reach expected state')


def assert_tree(tree, required, forbidden):
    for text in required:
        if text not in tree:
            raise ValueError('required UI text absent: ' + text)
    for text in forbidden:
        if text in tree:
            raise ValueError('forbidden UI text present: ' + text)


def validate_mav_report(path, expected):
    report = json.loads(path.read_text())
    steps = report.get('steps') or []
    names = [step.get('name') for step in steps]
    if (len(names) != len(set(names)) or not set(expected).issubset(names)
            or set(names) - set(expected) - {'final'}):
        raise ValueError('missing or unexpected MAV capture')
    if any(step.get('image', {}).get('ok') is not True for step in steps):
        raise ValueError('invalid MAV capture')
    if report.get('crashes') or report.get('invalid_step_count') != 0 or report.get('video_status') != 'accepted':
        raise ValueError('MAV evidence incomplete or crashed')
    if any(issue.get('severity') in ('error', 'blocking') for issue in report.get('issues', [])):
        raise ValueError('MAV reported blocking issue')


def exercise(mav, read, control, output: Path, run_id: str, scenario: str):
    """mav is a recorded command adapter pinned to one owned app/simulator."""
    output.mkdir(exist_ok=False)
    sequence = 0
    steps = []
    def command(action):
        nonlocal sequence
        sequence += 1
        control({'runID': run_id, 'sequence': sequence, 'action': action})
    def capture(name, required, forbidden=()):
        tree = mav('ui', 'tree', '--raw')
        (output / (name + '-tree.txt')).write_text(tree)
        assert_tree(tree, required, forbidden)
        mav('evidence', 'step', '--name', name, '--note', 'Synthetic visual scenario ' + scenario)
        value = read()
        if value is None or value.get('runID') != run_id:
            raise ValueError('missing current fixture at capture')
        atomic_json(output / (name + '-state.json'), value)
        steps.append(name)
    onboarding = ('pairing.screen', 'Your work.', 'A home for your work.', 'Connecting securely', 'pairing.signIn')
    failure = None
    failure_diagnostics = []
    try:
        mav('open', '--no-relaunch')
        mav('evidence', 'start')
        if scenario == 'reconnect':
            wait_state(read, run_id, scenario, lambda s: s.get('savedConfiguration') is True and s.get('restoring') is True and not s.get('ready'))
            mav('ui', 'wait', '--text', 'Connecting to the fleet…', '--timeout', '15s')
            capture('saved-home-reconnecting', ('Tasks', 'Connecting to the fleet'), onboarding)
            command('restore')
            wait_state(read, run_id, scenario, lambda s: s.get('ready') and 'q101' in s.get('inboxIDs', []))
            mav('ui', 'wait', '--text', 'Alpha Mac question', '--timeout', '15s')
            capture('saved-home-connected', ('Alpha Mac question',), onboarding)
        elif scenario == 'account-settings':
            wait_state(read, run_id, scenario, lambda s: s.get('ready') and s.get('settingsPresented'))
            mav('ui', 'wait', '--text', 'Sign out', '--timeout', '15s')
            capture('account-in-settings', ('Account', 'Sign out', 'synthetic.invalid', 'Mac alpha-mac', 'Mac beta-mac'), onboarding)
        elif scenario == 'mac-selection':
            wait_state(read, run_id, scenario, lambda s: s.get('ready') and s.get('settingsPresented'))
            mav('ui', 'wait', '--text', 'Mac beta-mac', '--timeout', '15s')
            capture('alpha-selected', ('Mac alpha-mac', 'Mac beta-mac'), onboarding)
            mav('ui', 'tap', '--text', 'Mac beta-mac')
            wait_state(read, run_id, scenario, lambda s: s.get('selectedMac') == 'beta' and 'q202' in s.get('inboxIDs', []))
            mav('ui', 'wait', '--text', 'Beta Mac question', '--timeout', '15s')
            capture('beta-selected', ('Beta Mac question',), (*onboarding, 'Alpha Mac question'))
            command('late-alpha')
            wait_state(read, run_id, scenario, lambda s: s.get('checks', {}).get('late_alpha_delivered') is True)
            capture('late-alpha-rejected', ('Beta Mac question',), (*onboarding, 'Alpha Mac question'))
        else:
            raise ValueError('unknown scenario')
        command('finish')
        report = wait_state(read, run_id, scenario, lambda s: s.get('status') == 'passed')
        atomic_json(output / 'result.json', report)
        validate_report(report, run_id, scenario)
        mav('evidence', 'stop')
        mav('evidence', 'report')
        validate_mav_report(output.parent / 'mav-run/report.json', steps)
        atomic_json(output / 'driver.json', {'schema': 1, 'runID': run_id, 'scenario': scenario, 'steps': steps, 'status': 'passed'})
        return report
    except BaseException as error:
        failure = error
        try:
            value = read()
            if value is not None:
                atomic_json(output / 'failed-result.json', value)
        except Exception as receipt_error:
            failure_diagnostics.append('Failure receipt unavailable: ' + type(receipt_error).__name__)
        try:
            mav('evidence', 'stop')
        except Exception:
            pass
        raise
    finally:
        try:
            mav('stop')
        except Exception as stop_error:
            if failure is None:
                raise
            failure_diagnostics.append('MAV cleanup failed: ' + type(stop_error).__name__)
        if failure_diagnostics:
            try:
                atomic_json(output / 'failure-diagnostics.json', failure_diagnostics)
            except OSError:
                pass  # An unavailable evidence directory must not replace the initial failure.
