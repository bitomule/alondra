use super::*;
use crate::views::{LiveBackground, RunOutcome};

#[test]
fn the_command_is_shown_with_its_start_and_never_what_it_said() {
    let node = LiveNode { tool: Some("bazel test //…\nsecond line".into()), tool_since: Some("15:38".into()),
        last_message: Some("The checks passed. I am opening the PR.".into()), last_message_at: Some("2026-11-11T15:31:00+01:00".into()),
        ..Default::default() };
    let fact = now_fact(Some(&node), &Doing::Turn, "15:00", Lang::Es);
    assert_eq!(fact.text(Lang::Es), "`bazel test //…` · desde las 15:38");
    assert_eq!(fact.text(Lang::En), "`bazel test //…` · since 15:38");
}

#[test]
fn work_in_the_background_names_what_runs_and_since_when() {
    let run = |label: &str, since: &str, state| LiveBackground { kind: "shell".into(), label: label.into(), since: since.into(), state };
    let node = LiveNode { background: vec![
        run("second", "2026-11-11T15:40:00+01:00", RunState::Running),
        run("finished", "14:00", RunState::Ended { outcome: RunOutcome::Finished, ran_secs: 10 }),
        run("bazel test //…", "2026-11-11T15:38:00+01:00", RunState::Running),
    ], ..Default::default() };
    let fact = now_fact(Some(&node), &Doing::OwnWork, "15:00", Lang::Es);
    assert_eq!(fact.text(Lang::Es), "En segundo plano: `bazel test //…` y 1 más · desde las 15:38");
    assert_eq!(fact.text(Lang::En), "In the background: `bazel test //…` and 1 more · since 15:38");
}

#[test]
fn a_command_wins_over_background_work_and_missing_times_use_the_turn_start() {
    let node = LiveNode { tool: Some("command".into()), background: vec![LiveBackground {
        kind: "shell".into(), label: "background".into(), since: "15:00".into(), state: RunState::Running,
    }], ..Default::default() };
    assert_eq!(now_fact(Some(&node), &Doing::OwnWork, "15:38", Lang::En).doing,
        NowDoing::Command { label: "command".into(), since: "15:38".into() });
    assert_eq!(now_fact(None, &Doing::Turn, "2026-11-11T15:38:00+01:00", Lang::En).text(Lang::En), "Turn running since 15:38");
}

#[test]
fn labels_fit_sixty_characters_without_splitting_unicode() {
    let node = LiveNode { tool: Some(format!("{}\nignored", "á🦜".repeat(50))), ..Default::default() };
    let fact = now_fact(Some(&node), &Doing::Turn, "15:38", Lang::En);
    let NowDoing::Command { label, .. } = fact.doing else { panic!("command") };
    assert_eq!(label.chars().count(), 60);
    assert!(!label.contains('\n'));
}
