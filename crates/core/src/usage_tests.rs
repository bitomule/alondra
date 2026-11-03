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
    AccountStatus { provider: Provider::Claude, id: id(text), name: text.into(), state, native_identity: None, email: None, plan: None, billing, sign_ins: 0, profile: None }
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
fn a_native_limit_without_reset_requires_fresh_positive_account_capacity_to_clear() {
    let provider = Provider::Codex; let account = AccountId::default_account();
    let marker = UsageReading { plan: None, complete: false, windows: vec![WindowReading { kind: WindowKind::Other("codex:account:limit".into()), used: Share::NONE, resets_at: None, exhausted: true }] };
    let held = record(None, provider, &account, &marker, 100, TODAY);
    let has_marker = |usage: &AccountUsage| usage.windows.iter().any(|window| window.reading.kind.is_native_limit_marker());
    let account_windows = vec![window(WindowKind::Other("codex:account:primary:90".into()), 0.1, 10_000), window(WindowKind::Other("codex:account:secondary:1200".into()), 0.2, 20_000)];
    assert!(has_marker(&record(Some(&held), provider, &account, &UsageReading { plan: None, complete: true, windows: vec![] }, 101, TODAY)), "unknown capacity never releases the marker");
    assert!(has_marker(&record(Some(&held), provider, &account, &UsageReading { plan: None, complete: false, windows: vec![account_windows[0].clone()] }, 101, TODAY)), "one partial dimension is not positive account-wide evidence");
    assert!(has_marker(&record(Some(&held), provider, &account, &UsageReading { plan: None, complete: false, windows: account_windows.clone() }, 99, TODAY)), "a response older than the limit cannot release it");
    let cleared = record(Some(&held), provider, &account, &UsageReading { plan: None, complete: false, windows: account_windows }, 101, TODAY);
    assert!(!has_marker(&cleared));
    assert_eq!(held.windows[0].reading.used, Share::NONE, "the error did not report a measured percentage");
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

fn any_state() -> impl Strategy<Value = AccountState> {
    prop_oneof![state(), Just(AccountState::Disabled)]
}

fn billing() -> impl Strategy<Value = AccountBilling> {
    prop_oneof![
        4 => (1..12u32).prop_map(|weight| AccountBilling::Subscription { plan: None, weight: Some(weight) }),
        1 => Just(AccountBilling::Subscription { plan: Some("pro".into()), weight: None }),
        1 => Just(AccountBilling::PayPerUse),
    ]
}

/// Up to four accounts in the user's order, each measured or not.
fn accounts_with_usage() -> impl Strategy<Value = (Vec<AccountStatus>, Vec<AccountUsage>)> {
    let account = (any_state(), billing(), proptest::option::of(proptest::collection::vec(arbitrary_reading(), 1..4)));
    proptest::collection::vec(account, 1..5).prop_map(|accounts| {
        let mut statuses = vec![];
        let mut usage = vec![];
        for (n, (state, billing, readings)) in accounts.into_iter().enumerate() {
            let name = format!("account-{n}");
            statuses.push(status(&name, state, billing));
            if let Some(readings) = readings {
                let mut measured = usage_after(&readings);
                measured.account = id(&name);
                usage.push(measured);
            }
        }
        (statuses, usage)
    })
}

fn policy() -> impl Strategy<Value = Policy> {
    prop_oneof![Just(Policy::Margin), Just(Policy::Chain)]
}

fn usable(account: &AccountStatus, usage: &[AccountUsage], now: Millis) -> bool {
    can_work(account, usage.iter().find(|u| u.account == account.id), now)
}

fn absolute(account: &AccountStatus, usage: &[AccountUsage], reserve: Share, now: Millis) -> f64 {
    let measured = usage.iter().find(|u| u.account == account.id);
    let weight = f64::from(weight(&account.billing, None).unwrap());
    weight * own_margin(account.state, measured, reserve, now, TODAY).unwrap_or(1.0)
}

proptest! {
    #[test]
    fn the_chosen_account_can_work_and_one_is_chosen_whenever_any_can((accounts, usage) in accounts_with_usage(), policy in policy(), reserve in 0.5..=1.0f64, later in 0..6 * 86_400_000i64) {
        let (reserve, now) = (Share::of(reserve), NOW + later);
        let chosen = choose_account(policy, Provider::Claude, &accounts, &usage, reserve, now, TODAY);
        let any = accounts.iter().any(|account| usable(account, &usage, now));
        prop_assert_eq!(chosen.is_some(), any);
        if let Some(chosen) = chosen {
            let account = accounts.iter().find(|account| account.id == chosen).unwrap();
            prop_assert!(usable(account, &usage, now), "{account:?}");
            prop_assert!(!matches!(account.billing, AccountBilling::PayPerUse), "paid per use is never chosen on its own");
            prop_assert!(!matches!(account.state, AccountState::Disabled | AccountState::SignedOut));
            prop_assert!(margin_returns_at(Provider::Claude, &accounts, &usage, now).is_none(), "nothing to wait for");
        }
    }

    #[test]
    fn by_margin_the_most_absolute_margin_wins_and_the_first_of_equals((accounts, usage) in accounts_with_usage(), reserve in 0.5..=1.0f64) {
        let reserve = Share::of(reserve);
        let Some(chosen) = choose_account(Policy::Margin, Provider::Claude, &accounts, &usage, reserve, NOW, TODAY) else { return Ok(()) };
        let position = accounts.iter().position(|account| account.id == chosen).unwrap();
        let best = absolute(&accounts[position], &usage, reserve, NOW);
        for (n, other) in accounts.iter().enumerate().filter(|(_, other)| usable(other, &usage, NOW)) {
            let theirs = absolute(other, &usage, reserve, NOW);
            prop_assert!(theirs <= best, "{} has {theirs}, more than the chosen {best}", other.id);
            if n < position && best > 0.0 {
                prop_assert!(theirs < best, "{} comes first with as much margin", other.id);
            }
        }
    }

    #[test]
    fn in_a_chain_the_first_account_with_margin_wins((accounts, usage) in accounts_with_usage(), reserve in 0.5..=1.0f64) {
        let reserve = Share::of(reserve);
        let chosen = choose_account(Policy::Chain, Provider::Claude, &accounts, &usage, reserve, NOW, TODAY);
        let with_margin = accounts.iter().find(|account| usable(account, &usage, NOW) && absolute(account, &usage, reserve, NOW) > 0.0);
        let first_usable = accounts.iter().find(|account| usable(account, &usage, NOW));
        prop_assert_eq!(chosen, with_margin.or(first_usable).map(|account| account.id.clone()));
    }

    #[test]
    fn the_margin_says_no_account_can_work_exactly_when_launches_wait((accounts, usage) in accounts_with_usage(), reserve in 0.5..=1.0f64, later in 0..6 * 86_400_000i64) {
        let (reserve, now) = (Share::of(reserve), NOW + later);
        let lines = account_lines(Provider::Claude, &accounts, &usage, reserve, now, TODAY);
        prop_assert_eq!(lines.len(), accounts.len(), "every account has its line, in the user's order");
        for (line, account) in lines.iter().zip(&accounts) {
            prop_assert_eq!(&line.name, &account.name);
            prop_assert_eq!(line.stand == AccountStand::CanWork, usable(account, &usage, now), "{:?}", line);
            prop_assert!(line.margin.is_none_or(|margin| (0.0..=1.0).contains(&margin)));
        }
        let summed = Usage { accounts: lines, ..Usage::default() };
        prop_assert_eq!(summed.none_can_work(), choose_account(Policy::Margin, Provider::Claude, &accounts, &usage, reserve, now, TODAY).is_none(),
            "a margin of 0 from the reserve or today's slice alone never reads as launches waiting");
    }

    #[test]
    fn when_no_account_can_work_one_can_once_its_limits_reset((accounts, usage) in accounts_with_usage(), reserve in 0.5..=1.0f64) {
        let reserve = Share::of(reserve);
        if choose_account(Policy::Margin, Provider::Claude, &accounts, &usage, reserve, NOW, TODAY).is_some() {
            return Ok(());
        }
        if let Some(back) = margin_returns_at(Provider::Claude, &accounts, &usage, NOW) {
            prop_assert!(back > NOW);
            prop_assert!(choose_account(Policy::Margin, Provider::Claude, &accounts, &usage, reserve, back, TODAY).is_some(), "at {back} one can work");
            prop_assert!(choose_account(Policy::Margin, Provider::Claude, &accounts, &usage, reserve, back - 1, TODAY).is_none(), "not before");
        }
    }
}

#[test]
fn without_recorded_accounts_the_default_sign_in_is_chosen() {
    assert_eq!(choose_account(Policy::Margin, Provider::Claude, &[], &[], default_reserve(), NOW, TODAY), Some(AccountId::default_account()));
    let out = record(None, Provider::Claude, &AccountId::default_account(), &reading(1.0, 0.5), NOW, TODAY);
    assert_eq!(choose_account(Policy::Chain, Provider::Claude, &[], &[out.clone()], default_reserve(), NOW, TODAY), None);
    assert_eq!(margin_returns_at(Provider::Claude, &[], &[out], NOW), Some(SESSION_RESET));
}

#[test]
fn margin_spreads_new_work_and_a_chain_fills_the_first_account() {
    let accounts = [status("main", AccountState::SignedIn, AccountBilling::default()), status("work", AccountState::SignedIn, AccountBilling::default())];
    let usage = |main: f64, work: f64| [
        record(None, Provider::Claude, &id("main"), &reading(0.1, main), NOW, TODAY),
        record(None, Provider::Claude, &id("work"), &reading(0.1, work), NOW, TODAY),
    ];
    let choose = |policy, usage: &[AccountUsage]| choose_account(policy, Provider::Claude, &accounts, usage, default_reserve(), NOW, TODAY).map(|id| id.to_string());
    assert_eq!(choose(Policy::Margin, &usage(0.6, 0.2)), Some("work".into()), "more room");
    assert_eq!(choose(Policy::Margin, &usage(0.3, 0.3)), Some("main".into()), "as much: the first");
    assert_eq!(choose(Policy::Chain, &usage(0.6, 0.2)), Some("main".into()), "the first with margin");
    let mut over = usage(0.2, 0.2);
    over[0] = record(None, Provider::Claude, &id("main"), &reading(0.95, 0.2), NOW, TODAY);
    assert_eq!(choose(Policy::Chain, &over), Some("work".into()), "past its reserve, the first has no margin");
    over[1] = record(None, Provider::Claude, &id("work"), &reading(0.97, 0.2), NOW, TODAY);
    assert_eq!(choose(Policy::Margin, &over), Some("main".into()), "the reserve never holds work back: both past it, the first");
    over[0] = record(None, Provider::Claude, &id("main"), &reading(1.0, 0.2), NOW, TODAY);
    assert_eq!(choose(Policy::Margin, &over), Some("work".into()), "only a limit run out does");
    let opus = UsageReading { windows: vec![WindowReading { kind: WindowKind::Other("seven_day_opus".into()), used: Share::FULL, resets_at: Some(WEEK_RESET), exhausted: true }], plan: None, complete: false };
    let only_opus = [record(Some(&usage(0.2, 0.2)[0]), Provider::Claude, &id("main"), &opus, NOW, TODAY), over[1].clone()];
    assert_eq!(choose(Policy::Chain, &only_opus), Some("main".into()), "one model's week alone leaves the others working");
}

fn line(stand: AccountStand, week: Option<f64>, five: Option<f64>, today: Option<(f64, f64)>) -> AccountLine {
    let mut line = AccountLine {
        name: "Principal".into(), stand, margin: None, week_percent: week, five_hour_percent: five,
        today_spent: today.map(|(spent, _)| spent), today_share: today.map(|(_, share)| share),
        week_resets_at: None, five_hour_resets_at: None, measured_at: None, state: None, sign_in: None,
    };
    line.state = usage_state(&line, default_reserve());
    line
}

#[test]
fn an_account_has_room_goes_over_pace_runs_low_or_is_out() {
    use UsageState::*;
    let can = || AccountStand::CanWork;
    assert_eq!(line(can(), Some(30.0), Some(10.0), Some((2.0, 15.0))).state, Some(WithRoom));
    assert_eq!(line(can(), None, None, None).state, Some(WithRoom), "not measured yet holds nothing back");
    assert_eq!(line(can(), Some(30.0), Some(10.0), Some((15.0, 15.0))).state, Some(OverPace), "today's share is spent");
    assert_eq!(line(can(), Some(86.0), Some(10.0), Some((2.0, 15.0))).state, Some(Low), "under 15% of the week left");
    assert_eq!(line(can(), Some(30.0), Some(91.0), Some((2.0, 15.0))).state, Some(Low), "the 5-hour window past the reserve");
    assert_eq!(line(can(), Some(90.0), Some(10.0), Some((20.0, 15.0))).state, Some(Low), "running low outweighs the pace");
    assert_eq!(line(AccountStand::AtLimit { until: Some(NOW) }, None, None, None).state, Some(Out));
    assert_eq!(line(AccountStand::SignedOut, None, None, None).state, Some(Out));
    assert_eq!(line(AccountStand::PayPerUse, None, None, None).state, None, "paid per use limits nothing");
    assert_eq!(line(AccountStand::Disabled, None, None, None).state, None);
}

#[test]
fn a_provider_stands_as_its_best_account_and_the_whole_as_its_worst_provider() {
    use UsageState::*;
    let provider = |provider, accounts| ProviderUsage { provider, accounts };
    let claude = provider(Provider::Claude, vec![
        line(AccountStand::AtLimit { until: Some(NOW + 7_200_000) }, None, None, None),
        line(AccountStand::CanWork, Some(30.0), None, Some((20.0, 15.0))),
    ]);
    let codex = provider(Provider::Codex, vec![line(AccountStand::CanWork, Some(10.0), None, None)]);
    assert_eq!(claude.state(), Some(OverPace));
    assert_eq!(codex.state(), Some(WithRoom));
    let both = [claude, codex];
    let (worst_provider, state) = worst(&both).unwrap();
    assert_eq!((worst_provider.provider, state), (Provider::Claude, OverPace));

    let out = provider(Provider::Claude, vec![
        line(AccountStand::AtLimit { until: Some(NOW + 7_200_000) }, None, None, None),
        line(AccountStand::AtLimit { until: Some(NOW + 3_600_000) }, None, None, None),
        line(AccountStand::PayPerUse, None, None, None),
    ]);
    assert_eq!((out.state(), out.back_at()), (Some(Out), Some(NOW + 3_600_000)), "back with the soonest account");
    let per_use = provider(Provider::Codex, vec![line(AccountStand::PayPerUse, None, None, None)]);
    assert_eq!(per_use.state(), None);
    assert!(worst(&[per_use]).is_none(), "only accounts paid per use: nothing to show");
}

#[test]
fn codex_windows_count_as_its_week_and_its_session_by_how_long_they_last() {
    let codex = AccountStatus { provider: Provider::Codex, ..status("codex", AccountState::SignedIn, AccountBilling::Subscription { plan: None, weight: None }) };
    let claude = status("default", AccountState::SignedIn, AccountBilling::Subscription { plan: None, weight: None });
    let windows = UsageReading { plan: None, complete: true, windows: vec![
        window(WindowKind::Other("codex:account:primary:300".into()), 0.4, SESSION_RESET),
        window(WindowKind::Other("codex:account:secondary:10080".into()), 0.2, WEEK_RESET),
    ] };
    let usage = vec![record(None, Provider::Codex, &id("codex"), &windows, NOW, TODAY), measured("default")];
    let providers = providers(&[claude, codex], &usage, default_reserve(), NOW, TODAY);
    assert_eq!(providers.iter().map(|p| p.provider).collect::<Vec<_>>(), [Provider::Claude, Provider::Codex]);
    let codex = &providers[1].accounts[0];
    assert_eq!((codex.week_percent.map(f64::round), codex.week_resets_at), (Some(20.0), Some(WEEK_RESET)));
    assert_eq!((codex.five_hour_percent.map(f64::round), codex.five_hour_resets_at), (Some(40.0), Some(SESSION_RESET)));
    assert_eq!((codex.measured_at, codex.state), (Some(NOW), Some(UsageState::WithRoom)));
    let claude = &providers[0].accounts[0];
    assert_eq!((claude.week_resets_at, claude.five_hour_resets_at), (Some(WEEK_RESET), Some(SESSION_RESET)));
    assert_eq!(claude.state, Some(UsageState::OverPace), "41% spent today against a ~18% slice");
}

#[test]
fn an_account_not_checked_yet_is_listed_but_stands_nowhere() {
    let implicit = providers(&[], &[], default_reserve(), NOW, TODAY);
    assert_eq!(implicit.iter().map(|p| p.provider).collect::<Vec<_>>(), [Provider::Claude], "the implicit Claude account");
    assert_eq!(implicit[0].accounts[0].state, None);
    assert!(worst(&implicit).is_none(), "no indicator says there is room while no account was checked");
    let unchecked = status("default", AccountState::Unchecked, AccountBilling::Subscription { plan: None, weight: None });
    let measured_unchecked = providers(&[unchecked], &[measured("default")], default_reserve(), NOW, TODAY);
    assert_eq!(measured_unchecked[0].accounts[0].state, None);
}
