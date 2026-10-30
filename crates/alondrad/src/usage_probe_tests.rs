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

#[test]
fn stale_usage_retries_keep_their_deadline_without_blocking_other_accounts() {
    use crate::native_probe::Outcome;
    let account = id("default");
    let other = id("work");
    let accounts = [account.clone(), other.clone()];
    let mut schedule = ProbeSchedule::default();
    let mut now = NOW;
    for seconds in [0, 1, 2, 4, 8, 16, 30, 30] {
        schedule.probed.insert(account.clone(), now);
        schedule.finished(&account, Outcome::Stale, now);
        let deadline = now + seconds * 1_000;
        if seconds > 0 {
            // Repeated domain notifications can reread the schedule, but cannot advance it.
            for _ in 0..3 {
                assert_eq!(schedule.due(&accounts, &[], now), (vec![other.clone()], Some(deadline)));
            }
        }
        assert!(schedule.due(&accounts, &[], deadline).0.contains(&account));
        now = deadline;
    }
    schedule.finished(&account, Outcome::Finished, now);
    assert!(!schedule.retries.contains_key(&account));
    assert_eq!(schedule.due(&accounts, &[], now).0, [other], "ordinary completion returns to the periodic interval");
    schedule.finished(&account, Outcome::Stale, now);
    assert!(schedule.due(&accounts, &[], now).0.contains(&account), "success resets the backoff");
}

#[tokio::test]
async fn a_usage_retry_rechecks_settings_before_launching_a_disabled_account() {
    use alondra_core::accounts::{AccountsConfig, ProviderAccount};
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    two_accounts(&domain);
    let mut config = AccountsConfig::default();
    config.claude = vec![ProviderAccount { enabled: false, ..ProviderAccount::implicit_default() },
        ProviderAccount { id: id("work"), name: "Work".into(), enabled: true, billing: AccountBilling::default() }];
    std::fs::write(alondra_core::options::path_in(&home), config.to_toml()).unwrap();
    let state = crate::sdk_owner::tests::HostRoot::new();
    let mut backend = probe_backend(&home, &state);
    let count = home.join("probe-count");
    backend.native_environment.insert("PROBE_COUNT".into(), count.display().to_string());
    domain.enable_sdk(backend);
    assert_eq!(measure_account(&domain, &home, &id("default")).await, crate::native_probe::Outcome::Finished);
    assert!(!count.exists(), "the disabled account must not run a native probe");
    assert!(domain.read().state().account_usage.is_empty(), "a disabled account's successful fake probe must not be published");
}

/// A native Claude that answers the probe as Claude Code does, with what `CLAUDE_CONFIG_DIR` it got.
const PROBE_NATIVE: &str = r#"#!/usr/bin/python3
import json, os, sys
if os.environ.get('PROBE_COUNT'):
    with open(os.environ['PROBE_COUNT'], 'a') as count:
        count.write('x')
folder = os.environ.get('CLAUDE_CONFIG_DIR', 'default')
if os.path.exists(os.path.join(os.environ['HOME'], 'usage-fails')):
    sys.exit(3)
for raw in sys.stdin:
    message = json.loads(raw)
    request = message['request']
    if request['subtype'] == 'initialize':
        response = dict(commands=[], models=[], output_style='default', account=dict(subscriptionType='pro' if folder != 'default' else 'max'))
    elif request['subtype'] == 'get_usage':
        if os.environ.get('STALE_OPTIONS') and os.path.exists(os.path.join(os.environ['HOME'], 'invalidate-usage-once')):
            os.unlink(os.path.join(os.environ['HOME'], 'invalidate-usage-once'))
            with open(os.environ['STALE_OPTIONS'], 'a') as options:
                options.write('\n# settings changed during native usage read\n')
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

#[tokio::test]
async fn a_stale_usage_probe_retries_with_a_fresh_fence_and_makes_the_account_selectable() {
    use alondra_core::accounts::{AccountKey, AccountsConfig};
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, ModelProfile, TaskSelection};
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(DomainRuntime::open(&home).unwrap());
    let state = crate::sdk_owner::tests::HostRoot::new();
    let mut backend = probe_backend(&home, &state);
    let mut config = AccountsConfig::fresh();
    config.connect(Provider::Claude);
    let options = alondra_core::options::path_in(&home);
    std::fs::write(&options, config.to_toml()).unwrap();
    backend.native_environment.insert("STALE_OPTIONS".into(), options.display().to_string());
    domain.enable_sdk(backend);
    let account = AccountId::default_account();
    readiness_accounts(&domain, vec![AccountStatus {
        provider: Provider::Claude, id: account.clone(), name: "Personal".into(), state: AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0,
    }]);
    let key = AccountKey::native(Provider::Claude);
    let commands = [
        DomainCommand::RecordDistribution { policy: config.distribution.policy, move_when_exhausted: config.distribution.move_when_exhausted },
        DomainCommand::RecordAccountConfiguration { order: vec![key.clone()], version: alondra_core::accounts::config_version(&config) },
        DomainCommand::RecordModelCatalog { catalog: AccountModelCatalog {
            account: key.clone(), revision: "fixture".into(), native_default: Some("fixture-model".into()), measured_at: NOW,
            models: vec![CatalogModel { model: "fixture-model".into(), display_name: "Fixture".into(), reasoning_efforts: vec![],
                default_reasoning_effort: None, context_window: None, input_modalities: vec!["text".into()], is_default: true, hidden: false }],
        } },
    ];
    let (fence, _) = crate::accounts::probe_fence(&domain, &home).unwrap();
    crate::accounts::publish_probe(&domain, &home, fence, commands.into()).unwrap();
    let now = now_millis();
    record_limit(&domain, &account, Some(now + 3_600_000), now);
    let intent = TaskSelection::automatic(ModelProfile::Implementation);
    assert!(crate::accounts::select_for_launch(&domain, &home, &intent, now).is_err());
    let before = domain.read().state().account_usage.clone();
    std::fs::write(home.join("invalidate-usage-once"), "").unwrap();
    let mut schedule = ProbeSchedule::default();
    for attempt in 1..=2 {
        let now = now_millis();
        assert_eq!(schedule.due(std::slice::from_ref(&account), &domain.read().state().account_usage, now).0, [account.clone()],
            "the discarded attempt must be due again before the periodic interval");
        schedule.probed.insert(account.clone(), now);
        let outcome = measure_account(&domain, &home, &account).await;
        if attempt == 1 {
            assert_eq!(outcome, crate::native_probe::Outcome::Stale);
            assert_eq!(domain.read().state().account_usage, before, "discarded usage cannot clear the quota fence");
            assert!(crate::accounts::select_for_launch(&domain, &home, &intent, now).is_err());
        } else {
            assert_eq!(outcome, crate::native_probe::Outcome::Finished);
        }
        schedule.finished(&account, outcome, now_millis());
    }
    assert_eq!(crate::accounts::select_for_launch(&domain, &home, &intent, now_millis()).unwrap().account, key);
    let recovered_at = now_millis();
    follow(&domain, recovered_at);
    assert_eq!(domain.live.usage_pause.current().map(|standing| standing.until), Some(recovered_at), "fresh capacity makes the pause due for resumption now");
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
        provider: Provider::Claude, id: id(account), name: account.into(), state: AccountState::SignedIn, native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0,
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

fn readiness_accounts(domain: &DomainRuntime, accounts: Vec<AccountStatus>) {
    let read = domain.read();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: read.state().data_epoch.clone(), command_id: "readiness-accounts".into(),
        expected_revision: read.state().revision, command: DomainCommand::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() },
    };
    drop(read);
    domain.execute_as_system(envelope).unwrap();
}

fn codex_status(state: AccountState) -> AccountStatus {
    AccountStatus { provider: Provider::Codex, id: id("default"), name: "Personal".into(), state, native_identity: None, email: None, plan: None,
        billing: AccountBilling::default(), sign_ins: 0 }
}

#[test]
fn missing_or_unavailable_providers_do_not_invent_a_claude_quota_reset() {
    for status in [None, Some(codex_status(AccountState::Unavailable)), Some(codex_status(AccountState::SignedOut))] {
        let home = Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        readiness_accounts(&domain, status.into_iter().collect());
        follow(&domain, NOW);
        let wait = domain.live.usage_pause.current().unwrap();
        assert!(wait.no_session, "provider availability is unknown, not quota exhaustion");
        assert_eq!(wait.until, NOW, "there is no fabricated future reset");
    }
}

#[test]
fn codex_capacity_ends_legacy_global_limits_but_preserves_a_user_pause() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    readiness_accounts(&domain, vec![codex_status(AccountState::SignedIn)]);
    domain.live.usage_pause.exhaust(Some(NOW + 3_600_000), NOW);
    follow(&domain, NOW);
    assert!(domain.live.usage_pause.current().is_none(), "Claude's old global limit cannot block Codex");
    domain.live.usage_pause.pause_by_user(NOW + 7_200_000, NOW);
    follow(&domain, NOW);
    let pause = domain.live.usage_pause.current().unwrap();
    assert!(pause.by_user);
    assert_eq!(pause.until, NOW + 7_200_000);
}

#[test]
fn an_explicit_paid_account_is_not_blocked_by_automatic_chain_readiness() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    let paid = AccountStatus { billing: AccountBilling::PayPerUse, ..codex_status(AccountState::SignedIn) };
    readiness_accounts(&domain, vec![paid]);
    assert!(alondra_core::usage::eligible_account_keys(&[], &domain.read().state().accounts, &[], NOW).is_empty(), "automatic selection still refuses paid accounts");
    domain.live.usage_pause.wait_for_session(NOW);
    follow(&domain, NOW);
    assert!(!domain.live.usage_pause.is_active(), "a user-authorized paid launch must reach its own guarded selector");
}

#[test]
fn codex_native_quota_uses_its_actual_reset() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    readiness_accounts(&domain, vec![codex_status(AccountState::SignedIn)]);
    let window = WindowReading { kind: WindowKind::Other("codex:account:primary:300".into()), used: Share::FULL, resets_at: Some(NOW + 900_000), exhausted: true };
    let reading = UsageReading { plan: None, complete: true, windows: vec![window] };
    let read = domain.read();
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: read.state().data_epoch.clone(), command_id: "native-quota".into(),
        expected_revision: read.state().revision, command: DomainCommand::RecordAccountUsage { provider: Provider::Codex, account: id("default"), reading, at: NOW, day: crate::node_reports::local_day(NOW) } };
    drop(read);
    domain.execute_as_system(envelope).unwrap();
    follow(&domain, NOW);
    let pause = domain.live.usage_pause.current().unwrap();
    assert!(!pause.no_session);
    assert_eq!(pause.until, NOW + 900_000, "the native Codex reset is retained");
}

#[test]
fn an_unknown_native_quota_marker_never_fabricates_a_future_reset() {
    let home = Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = DomainRuntime::open(&home).unwrap();
    readiness_accounts(&domain, vec![codex_status(AccountState::SignedIn)]);
    let window = WindowReading { kind: WindowKind::Other("codex:account:limit".into()), used: Share::of(0.0), resets_at: None, exhausted: true };
    let reading = UsageReading { plan: None, complete: false, windows: vec![window] };
    let read = domain.read();
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: read.state().data_epoch.clone(), command_id: "unknown-native-quota".into(),
        expected_revision: read.state().revision, command: DomainCommand::RecordAccountUsage { provider: Provider::Codex, account: id("default"), reading, at: NOW, day: crate::node_reports::local_day(NOW) } };
    drop(read);
    domain.execute_as_system(envelope).unwrap();
    follow(&domain, NOW);
    let pause = domain.live.usage_pause.current().unwrap();
    assert!(pause.no_session, "availability awaits native metadata; no reset is known");
    assert_eq!(pause.until, NOW);
    assert!(alondra_core::usage::eligible_account_keys(&[], &domain.read().state().accounts, &domain.read().state().account_usage, NOW).is_empty(), "the real quota marker still fences automatic launches");
}
