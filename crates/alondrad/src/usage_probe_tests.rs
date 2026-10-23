use super::*;
use crate::test_support::Directory;
use alondra_core::usage::WindowKind;

const NOW: Millis = 1_790_192_202_000;

fn id(text: &str) -> AccountId {
    AccountId::parse(text).unwrap()
}

#[test]
fn a_binding_on_the_native_sign_in_runs_on_the_default_account() {
    assert!(account_of("native-login").is_default());
    assert_eq!(account_of("work"), id("work"));
    assert!(account_of("Not An Id").is_default());
}

#[test]
fn the_probes_answer_is_a_complete_reading_of_the_session_and_the_week() {
    let report: ProbeReport = serde_json::from_value(serde_json::json!({
        "plan": "max", "rate_limits_available": true,
        "windows": [
            {"kind": "five_hour", "used": 0.06, "resets_at": 1_790_202_000_000.0},
            {"kind": "seven_day", "used": 0.53, "resets_at": 1_790_553_600_000.0},
            {"kind": "seven_day_opus", "used": 0.1},
        ],
    })).unwrap();
    let reading = reading(report).unwrap();
    assert!(reading.complete);
    assert_eq!(reading.plan.as_deref(), Some("max"));
    assert_eq!(reading.windows, [
        WindowReading { kind: WindowKind::Session, used: Share::of(0.06), resets_at: Some(1_790_202_000_000), exhausted: false },
        WindowReading { kind: WindowKind::Weekly, used: Share::of(0.53), resets_at: Some(1_790_553_600_000), exhausted: false },
        WindowReading { kind: WindowKind::Other("seven_day_opus".into()), used: Share::of(0.1), resets_at: None, exhausted: false },
    ]);
    let api_key: ProbeReport = serde_json::from_value(serde_json::json!({"rate_limits_available": false, "windows": []})).unwrap();
    assert!(super::reading(api_key).is_err(), "an account without plan limits has nothing to read");
}

fn measured(account: &str, at: Millis, exhausted: bool) -> AccountUsage {
    let reading = UsageReading {
        windows: vec![WindowReading { kind: WindowKind::Session, used: Share::of(0.5), resets_at: Some(at + 3_600_000), exhausted }],
        plan: None, complete: false,
    };
    alondra_core::usage::record(None, Provider::Claude, &id(account), &reading, at, "2026-09-23")
}

#[test]
fn an_account_is_probed_at_start_every_15_minutes_while_quiet_and_right_after_a_refusal() {
    let accounts = [id("default"), id("work")];
    let (ready, next) = due(&accounts, &[], &HashMap::new(), NOW);
    assert_eq!((ready, next), (accounts.to_vec(), None), "at start, every account");

    let busy = [measured("default", NOW - 60_000, false)];
    let probed = HashMap::from([(id("default"), NOW - 20 * 60_000), (id("work"), NOW - 5 * 60_000)]);
    let (ready, next) = due(&accounts, &busy, &probed, NOW);
    assert!(ready.is_empty(), "a node measured the default account a minute ago, and the work one was probed");
    assert_eq!(next, Some(NOW - 5 * 60_000 + EVERY), "the next is the work account, 15 minutes after its probe");

    let failed = HashMap::from([(id("default"), NOW - 60_000), (id("work"), NOW - 60_000)]);
    assert!(due(&accounts, &[], &failed, NOW).0.is_empty(), "a probe that failed waits its 15 minutes too");

    let refused = [measured("default", NOW - 1_000, true)];
    assert_eq!(due(&accounts, &refused, &failed, NOW).0, [id("default")], "Claude refused work after the last probe");
    let probed_after = HashMap::from([(id("default"), NOW), (id("work"), NOW)]);
    assert!(due(&accounts, &refused, &probed_after, NOW).0.is_empty(), "and once probed it waits again");
}

/// A native Claude that answers the probe as Claude Code does, with what `CLAUDE_CONFIG_DIR` it got.
const PROBE_NATIVE: &str = r#"#!/usr/bin/python3
import json, os, sys
folder = os.environ.get('CLAUDE_CONFIG_DIR', 'default')
if os.path.exists(os.path.join(os.environ['HOME'], 'usage-fails')):
    sys.exit(3)
for raw in sys.stdin:
    message = json.loads(raw)
    request = message['request']
    if request['subtype'] == 'initialize':
        response = dict(commands=[], models=[], output_style='default', account=dict(subscriptionType='pro' if folder != 'default' else 'max'))
    elif request['subtype'] == 'get_usage':
        response = dict(session=dict(total_cost_usd=0, total_api_duration_ms=0, total_duration_ms=0, total_lines_added=0, total_lines_removed=0, model_usage={}),
                        subscription_type='max', rate_limits_available=True, behaviors=None, rate_limits=dict(
                            five_hour=dict(utilization=12.0, resets_at='2099-01-01T05:00:00Z'),
                            seven_day=dict(utilization=40.0 if folder == 'default' else 3.0, resets_at='2099-01-05T00:00:00Z'),
                            seven_day_sonnet=None, extra_usage=None))
    else:
        sys.stdout.write(json.dumps(dict(type='control_response', response=dict(subtype='error', request_id=message['request_id'], error='unexpected'))) + '\n')
        sys.stdout.flush()
        continue
    sys.stdout.write(json.dumps(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'], response=response))) + '\n')
    sys.stdout.flush()
"#;

fn probe_backend(home: &Path, state: &crate::sdk_owner::tests::HostRoot) -> SdkBackend {
    use std::os::unix::fs::PermissionsExt;
    let (mut backend, _) = crate::sdk_owner::tests::fixture_backend(home, state);
    std::fs::write(&backend.native, PROBE_NATIVE).unwrap();
    std::fs::set_permissions(&backend.native, std::fs::Permissions::from_mode(0o700)).unwrap();
    backend.native_sha256 = crate::sdk_owner::backend::digest(&backend.native).unwrap();
    backend
}

#[test]
fn the_probe_runs_the_released_host_without_a_prompt_and_reads_each_accounts_folder() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let state = crate::sdk_owner::tests::HostRoot::new();
    let backend = probe_backend(&home, &state);

    let default = probe(&backend, None).expect("the default profile answers");
    assert_eq!(default.plan.as_deref(), Some("max"));
    assert_eq!(default.windows.iter().map(|w| (w.kind.clone(), w.used)).collect::<Vec<_>>(),
        [(WindowKind::Session, Share::of(0.12)), (WindowKind::Weekly, Share::of(0.4))]);
    assert_eq!(default.windows[1].resets_at, Some(4_071_254_400_000), "2099-01-05T00:00:00Z");

    let work = probe(&backend, Some(&home.join("accounts/work"))).expect("an account's folder answers");
    assert_eq!((work.plan.as_deref(), work.windows[1].used), (Some("pro"), Share::of(0.03)), "Claude ran with the account's folder");

    std::fs::write(home.join("usage-fails"), "").unwrap();
    let failed = probe(&backend, None).unwrap_err();
    assert!(failed.contains("the probe failed"), "{failed}");
}

#[test]
fn a_probe_that_fails_keeps_the_last_good_reading_and_its_age() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    let state = crate::sdk_owner::tests::HostRoot::new();
    let backend = probe_backend(&home, &state);
    let reading = probe(&backend, None).unwrap();
    record(&domain, &AccountId::default_account(), reading, NOW);
    let before = domain.read().state().account_usage.clone();
    std::fs::write(home.join("usage-fails"), "").unwrap();
    assert!(probe(&backend, None).is_err());
    assert_eq!(domain.read().state().account_usage, before, "nothing recorded");
    assert_eq!(before[0].measured_at(), Some(NOW), "the reading says how old it is");
    let usage = domain.read().index().usage(NOW + 3_600_000, &crate::node_reports::local_day(NOW)).unwrap();
    assert_eq!((usage.week_percent, usage.measured_at), (40.0, NOW));
}

#[test]
fn the_readings_and_the_days_start_survive_a_restart() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let week = |used: f64| UsageReading {
        windows: vec![WindowReading { kind: WindowKind::Weekly, used: Share::of(used), resets_at: Some(NOW + 3 * 86_400_000), exhausted: false }],
        plan: Some("max".into()), complete: false,
    };
    let before = {
        let domain = DomainRuntime::open(&home).unwrap();
        record(&domain, &AccountId::default_account(), week(0.12), NOW);
        record(&domain, &AccountId::default_account(), week(0.30), NOW + 60_000);
        domain.read().state().account_usage.clone()
    };
    let restarted = DomainRuntime::open(&home).unwrap();
    assert_eq!(restarted.read().state().account_usage, before);
    let usage = restarted.read().index().usage(NOW + 120_000, &crate::node_reports::local_day(NOW)).unwrap();
    assert_eq!(usage.today_spent.map(f64::round), Some(18.0), "today still counts from its first reading");
}

fn two_accounts(domain: &DomainRuntime) {
    let accounts = ["default", "work"].map(|account| AccountStatus {
        provider: Provider::Claude, id: id(account), name: account.into(), state: AccountState::SignedIn, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0,
    }).to_vec();
    let read = domain.read();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: read.state().data_epoch.clone(), command_id: "two-accounts".into(),
        expected_revision: read.state().revision, command: DomainCommand::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() },
    };
    drop(read);
    domain.execute_as_system(envelope).unwrap();
}

// The Mac waits only when no account can work: one account out of usage leaves new work to the
// other, both out pause everything until the soonest comes back, and one coming back early ends
// the wait then, not at the later reset.
#[test]
fn the_mac_waits_only_while_no_account_can_work() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    two_accounts(&domain);
    let now = now_millis();
    record_limit(&domain, &id("default"), Some(now + 3_600_000), now);
    assert!(!domain.live.usage_pause.is_active(), "the work account can still work");
    assert!(domain.read().state().account_usage.iter().any(|usage| usage.account.is_default() && usage.exhausted_until(now) == Some(now + 3_600_000)));
    record_limit(&domain, &id("work"), Some(now + 7_200_000), now);
    assert_eq!(domain.live.usage_pause.current().map(|standing| standing.until), Some(now + 3_600_000), "until the soonest account comes back");
    record_limit(&domain, &id("work"), Some(now + 1_800_000), now);
    assert_eq!(domain.live.usage_pause.current().map(|standing| standing.until), Some(now + 1_800_000), "a reading that moves a reset earlier moves the wait");
    record(&domain, &id("default"), UsageReading {
        windows: vec![WindowReading { kind: WindowKind::Session, used: Share::of(0.2), resets_at: Some(now + 3_600_000), exhausted: false }],
        plan: None, complete: true,
    }, now + 1_000);
    follow(&domain, now + 1_000);
    assert_eq!(domain.live.usage_pause.current().map(|standing| standing.until), Some(now + 1_000), "the default account works again: the wait ends now");
    domain.live.usage_pause.resume(now + 1_000);
    domain.live.usage_pause.exhaust(Some(now + 600_000), now + 2_000);
    follow(&domain, now + 2_000);
    assert_eq!(domain.live.usage_pause.current().map(|standing| standing.until), Some(now + 600_000), "a limit Claude reported that no reading shows yet stands");
}
