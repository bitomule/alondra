use super::*;
use proptest::prelude::*;

const NOW: Millis = 1_790_192_202_000;
const TODAY: &str = "2026-09-23";
const SESSION_RESET: Millis = 1_790_202_000_000;
const WEEK_RESET: Millis = 1_790_553_600_000;

fn id(text: &str) -> AccountId {
    AccountId::parse(text).unwrap()
}

fn status(text: &str, state: AccountState, billing: AccountBilling) -> AccountStatus {
    AccountStatus { provider: Provider::Claude, id: id(text), name: text.into(), state, email: None, plan: None, billing }
}

fn window(kind: WindowKind, used: f64, resets_at: Millis) -> WindowReading {
    WindowReading { kind, used: Share::of(used), resets_at: Some(resets_at), exhausted: false }
}

fn reading(session: f64, week: f64) -> UsageReading {
    UsageReading { windows: vec![window(WindowKind::Session, session, SESSION_RESET), window(WindowKind::Weekly, week, WEEK_RESET)], plan: None, complete: true }
}

/// This Mac on 23 Sep at ~21:30: 12% of the week used at the day's first reading, 53% now, the
/// week resetting in just under 5 days.
fn measured(text: &str) -> AccountUsage {
    let morning = record(None, Provider::Claude, &id(text), &reading(0.02, 0.12), NOW - 3_600_000, TODAY);
    record(Some(&morning), Provider::Claude, &id(text), &reading(0.06, 0.53), NOW, TODAY)
}

#[test]
fn plans_weigh_as_many_pro_weeks_as_they_hold() {
    assert_eq!(plan_weight(Some("pro")), 1);
    assert_eq!(plan_weight(Some("Max 5x")), 5);
    assert_eq!(plan_weight(Some("max_20x")), 10);
    for unknown in [Some("max"), Some("team"), Some("enterprise"), Some("something new"), None] {
        assert_eq!(plan_weight(unknown), 5, "{unknown:?}");
    }
    assert_eq!(weight(&AccountBilling::Subscription { plan: None, weight: None }, Some("pro")), Some(1), "the plan the provider reported");
    assert_eq!(weight(&AccountBilling::Subscription { plan: Some("max 20x".into()), weight: None }, Some("max")), Some(10), "the plan the user set wins");
    assert_eq!(weight(&AccountBilling::Subscription { plan: None, weight: Some(3) }, Some("pro")), Some(3), "and the weight the user set wins over both");
    assert_eq!(weight(&AccountBilling::PayPerUse, Some("max")), None, "an account paid per use never counts");
}

#[test]
fn todays_slice_is_anchored_at_the_days_first_reading_of_the_week() {
    let usage = measured("default");
    assert_eq!(usage.day_start, Some(DayStart { day: TODAY.into(), weekly: Share::of(0.12), week_resets_at: Some(WEEK_RESET) }));
    let pace = pace(&usage, NOW, TODAY).unwrap();
    assert_eq!(pace.week_used, 0.53);
    assert_eq!(pace.today_spent.map(|spent| (spent * 100.0).round()), Some(41.0));
    assert_eq!(pace.today_share.map(|share| (share * 100.0).round()), Some(18.0));
    assert!(pace.over());
    let tomorrow = record(Some(&usage), Provider::Claude, &id("default"), &reading(0.0, 0.55), NOW + 86_400_000, "2026-09-24");
    assert_eq!(tomorrow.day_start.as_ref().map(|start| start.weekly), Some(Share::of(0.55)), "a new day starts from its first reading");
    let next_week = record(Some(&usage), Provider::Claude, &id("default"), &UsageReading {
        windows: vec![window(WindowKind::Weekly, 0.01, WEEK_RESET + 7 * 86_400_000)], plan: None, complete: false,
    }, NOW + 1_000, TODAY);
    assert_eq!(next_week.day_start.as_ref().map(|start| start.weekly), Some(Share::of(0.01)), "so does a new week");
}

#[test]
fn a_partial_reading_changes_only_its_windows_and_a_complete_one_replaces_them_all() {
    let usage = measured("default");
    let event = UsageReading { windows: vec![WindowReading { kind: WindowKind::Session, used: Share::FULL, resets_at: Some(SESSION_RESET), exhausted: true }], plan: Some("max".into()), complete: false };
    let limited = record(Some(&usage), Provider::Claude, &id("default"), &event, NOW + 5, TODAY);
    assert_eq!(limited.window(&WindowKind::Weekly), usage.window(&WindowKind::Weekly), "the week keeps its reading");
    assert!(limited.window(&WindowKind::Session).unwrap().reading.exhausted);
    assert_eq!((limited.plan.as_deref(), limited.measured_at()), (Some("max"), Some(NOW + 5)));
    assert_eq!(limited.exhausted_until(NOW + 5), Some(SESSION_RESET));
    let probe = UsageReading { windows: vec![window(WindowKind::Weekly, 0.6, WEEK_RESET)], plan: None, complete: true };
    let probed = record(Some(&limited), Provider::Claude, &id("default"), &probe, NOW + 10, TODAY);
    assert!(probed.window(&WindowKind::Session).is_none(), "the probe lists every window the account has");
    assert_eq!(probed.plan.as_deref(), Some("max"), "a reading without a plan keeps the one reported");
}

#[test]
fn a_window_whose_reset_has_passed_counts_as_unused() {
    let usage = record(None, Provider::Claude, &id("default"), &UsageReading {
        windows: vec![WindowReading { kind: WindowKind::Session, used: Share::FULL, resets_at: Some(SESSION_RESET), exhausted: true }, window(WindowKind::Weekly, 0.2, WEEK_RESET)],
        plan: None, complete: true,
    }, NOW, TODAY);
    let signed_in = AccountState::SignedIn;
    assert_eq!(own_margin(signed_in, Some(&usage), default_reserve(), NOW, TODAY), Some(0.0), "out of its session window");
    assert_eq!(usage.exhausted_until(SESSION_RESET + 1), None);
    assert_eq!(own_margin(signed_in, Some(&usage), default_reserve(), SESSION_RESET + 1, TODAY), Some(0.8), "the session reset: the week's 80% is left");
    assert!(pace(&usage, WEEK_RESET + 1, TODAY).is_none(), "a week that reset says nothing of today");
}

#[test]
fn the_margin_is_the_least_of_the_week_and_todays_slice() {
    let usage = measured("default");
    let reserve = default_reserve();
    assert_eq!(own_margin(AccountState::SignedIn, Some(&usage), reserve, NOW, TODAY), Some(0.0), "today took more than its slice");
    let fresh = record(None, Provider::Claude, &id("default"), &reading(0.10, 0.30), NOW, TODAY);
    assert_eq!(own_margin(AccountState::SignedIn, Some(&fresh), reserve, NOW, TODAY), Some(0.7), "nothing spent today yet: what is left of the week");
    let busy = record(None, Provider::Claude, &id("default"), &reading(0.95, 0.30), NOW, TODAY);
    assert_eq!(own_margin(AccountState::SignedIn, Some(&busy), reserve, NOW, TODAY), Some(0.0), "the session window passed the reserve");
    assert_eq!(own_margin(AccountState::SignedIn, None, reserve, NOW, TODAY), None, "never measured");
    assert_eq!(own_margin(AccountState::Disabled, Some(&fresh), reserve, NOW, TODAY), None, "turned off");
}

#[test]
fn the_combined_margin_weighs_each_account_by_its_plan_and_leaves_out_pay_per_use() {
    let subscription = |plan: &str| AccountBilling::Subscription { plan: Some(plan.into()), weight: None };
    let accounts = [
        status("default", AccountState::SignedIn, subscription("max 20x")),
        status("pro", AccountState::SignedIn, subscription("pro")),
        status("api", AccountState::SignedIn, AccountBilling::PayPerUse),
    ];
    let usage = [
        record(None, Provider::Claude, &id("default"), &reading(0.1, 0.5), NOW, TODAY),
        record(None, Provider::Claude, &id("pro"), &reading(0.1, 0.0), NOW, TODAY),
        record(None, Provider::Claude, &id("api"), &reading(0.1, 0.0), NOW, TODAY),
    ];
    let margins = margins(&accounts, &usage, default_reserve(), NOW, TODAY);
    let counted: Vec<_> = margins.accounts.iter().map(|m| (m.account.as_str(), m.weight, m.absolute)).collect();
    assert_eq!(counted, [("default", 10, 5.0), ("pro", 1, 1.0)]);
    assert_eq!(margins.combined, Some(6.0 / 11.0));
}

#[test]
fn one_account_sums_up_as_the_week_and_today_it_reads() {
    let accounts = [status("default", AccountState::SignedIn, AccountBilling::default())];
    let usage = summary(&accounts, &[measured("default")], default_reserve(), NOW, TODAY).unwrap();
    assert_eq!(usage.week_percent, 53.0);
    assert_eq!(usage.week_resets_at, WEEK_RESET);
    assert_eq!(usage.today_spent.map(f64::round), Some(41.0));
    assert_eq!(usage.today_share.map(f64::round), Some(18.0));
    assert_eq!(usage.per_day_left.round(), 9.0);
    assert!(usage.over_pace);
    assert_eq!((usage.five_hour_percent, usage.five_hour_resets_at), (Some(6.0), Some(SESSION_RESET)));
    assert_eq!((usage.measured_at, usage.margin), (NOW, Some(0.0)));
    assert!(summary(&accounts, &[], default_reserve(), NOW, TODAY).is_none(), "nothing measured, nothing to sum");
}

#[test]
fn a_reading_with_a_window_twice_or_a_blank_plan_is_refused() {
    let mut twice = reading(0.1, 0.2);
    twice.windows.push(window(WindowKind::Weekly, 0.3, WEEK_RESET));
    assert!(twice.check().is_err());
    assert!(UsageReading { plan: Some(" ".into()), ..UsageReading::default() }.check().is_err());
    assert!(reading(0.1, 0.2).check().is_ok());
    assert_eq!(serde_json::to_value(Share::of(0.25)).unwrap(), serde_json::json!(2500), "kept in ten-thousandths");
    assert!(serde_json::from_value::<Share>(serde_json::json!(10_001)).is_err(), "a share is from 0 to 1");
}

fn state() -> impl Strategy<Value = AccountState> {
    prop_oneof![Just(AccountState::Unchecked), Just(AccountState::SignedIn), Just(AccountState::SignedOut), Just(AccountState::Unavailable)]
}

fn arbitrary_reading() -> impl Strategy<Value = UsageReading> {
    let kind = prop_oneof![Just(WindowKind::Session), Just(WindowKind::Weekly), Just(WindowKind::Other("seven_day_opus".into()))];
    let window = (kind, 0.0..=1.0f64, proptest::option::of(NOW - 86_400_000..NOW + 8 * 86_400_000), any::<bool>())
        .prop_map(|(kind, used, resets_at, exhausted)| WindowReading { kind, used: Share::of(used), resets_at, exhausted });
    (proptest::collection::vec(window, 0..4), any::<bool>()).prop_map(|(mut windows, complete)| {
        let mut seen = std::collections::BTreeSet::new();
        windows.retain(|w| seen.insert(w.kind.clone()));
        UsageReading { windows, plan: None, complete }
    })
}

fn usage_after(readings: &[UsageReading]) -> AccountUsage {
    let mut usage: Option<AccountUsage> = None;
    for (n, reading) in readings.iter().enumerate() {
        let day = if n % 3 == 2 { "2026-09-24" } else { TODAY };
        usage = Some(record(usage.as_ref(), Provider::Claude, &id("default"), reading, NOW + n as Millis * 60_000, day));
    }
    usage.unwrap_or_else(|| record(None, Provider::Claude, &id("default"), &UsageReading::default(), NOW, TODAY))
}

proptest! {
    #[test]
    fn the_margin_is_between_0_and_1(readings in proptest::collection::vec(arbitrary_reading(), 1..6), state in state(), reserve in 0.01..=1.0f64, later in 0..10 * 86_400_000i64) {
        let usage = usage_after(&readings);
        let margin = own_margin(state, Some(&usage), Share::of(reserve), NOW + later, TODAY).unwrap();
        prop_assert!((0.0..=1.0).contains(&margin), "{margin}");
        let accounts = [status("default", state, AccountBilling::default())];
        if let Some(combined) = margins(&accounts, &[usage], Share::of(reserve), NOW + later, TODAY).combined {
            prop_assert!((0.0..=1.0).contains(&combined), "{combined}");
        }
    }

    #[test]
    fn the_margin_is_0_when_the_account_is_blocked(readings in proptest::collection::vec(arbitrary_reading(), 0..4), reserve in 0.01..0.99f64, over in 0.0..1.0f64) {
        let usage = usage_after(&readings);
        prop_assert_eq!(own_margin(AccountState::SignedOut, Some(&usage), Share::of(reserve), NOW, TODAY), Some(0.0));
        let exhausted = record(Some(&usage), Provider::Claude, &id("default"), &UsageReading {
            windows: vec![WindowReading { kind: WindowKind::Other("seven_day_opus".into()), used: Share::of(0.4), resets_at: Some(NOW + 60_000), exhausted: true }],
            plan: None, complete: false,
        }, NOW, TODAY);
        prop_assert_eq!(own_margin(AccountState::SignedIn, Some(&exhausted), Share::of(reserve), NOW, TODAY), Some(0.0));
        let session = reserve + (1.0 - reserve) * over;
        let past_reserve = record(Some(&usage), Provider::Claude, &id("default"), &UsageReading {
            windows: vec![window(WindowKind::Session, session, NOW + 60_000)], plan: None, complete: false,
        }, NOW, TODAY);
        if Share::of(session) > Share::of(reserve) {
            prop_assert_eq!(own_margin(AccountState::SignedIn, Some(&past_reserve), Share::of(reserve), NOW, TODAY), Some(0.0));
        }
    }

    #[test]
    fn a_heavier_account_has_more_absolute_margin(light in 1..20u32, extra in 1..20u32, week in 0.0..0.99f64) {
        let accounts = |weight: u32| [status("default", AccountState::SignedIn, AccountBilling::Subscription { plan: None, weight: Some(weight) })];
        let usage = [record(None, Provider::Claude, &id("default"), &reading(0.1, week), NOW, TODAY)];
        let absolute = |weight: u32| margins(&accounts(weight), &usage, default_reserve(), NOW, TODAY).accounts[0].absolute;
        prop_assert!(absolute(light + extra) > absolute(light));
    }
}
