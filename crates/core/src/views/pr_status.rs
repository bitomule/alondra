//! A pull request's one state and its checks in one line, the way GitHub shows them.
//!
//! The state follows one priority: merged, closed, conflicts, failing checks, running checks,
//! then whether GitHub lets it merge. A repository without CI has no checks, so its state comes
//! only from that last part, never from checks that do not exist.

use super::{
    CheckState, Label, Lang, LiveCheck, LivePullRequest, Tone,
    text::{t, tf},
};
use crate::domain::{PrState, PullRequest};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrStatus {
    Merged,
    Closed,
    Conflicts,
    ChecksFailing,
    ChecksRunning,
    Draft,
    /// GitHub refuses the merge for something other than checks, such as a required review.
    Blocked,
    Ready,
    /// GitHub has not answered yet, or is still working out whether it merges.
    Unknown,
}

pub fn status(pr: &PullRequest, live: Option<&LivePullRequest>) -> PrStatus {
    match pr.state {
        PrState::Merged => return PrStatus::Merged,
        PrState::Closed => return PrStatus::Closed,
        PrState::Open => {}
    }
    let Some(live) = live.filter(|live| live.fetched) else { return PrStatus::Unknown };
    let any = |state: CheckState| live.checks.iter().any(|check| check.state == state);
    if live.conflicts {
        PrStatus::Conflicts
    } else if any(CheckState::Fail) {
        PrStatus::ChecksFailing
    } else if any(CheckState::Pending) {
        PrStatus::ChecksRunning
    } else if live.draft {
        PrStatus::Draft
    } else if live.mergeability_unknown {
        PrStatus::Unknown
    } else if live.blocked {
        PrStatus::Blocked
    } else {
        PrStatus::Ready
    }
}

pub fn label(status: PrStatus, lang: Lang) -> Label {
    let (text, tone) = match status {
        PrStatus::Merged => (t!(lang, "Merged", "Mergeado"), Tone::Success),
        PrStatus::Closed => (t!(lang, "Closed", "Cerrado"), Tone::Neutral),
        PrStatus::Conflicts => (t!(lang, "Conflicts", "Conflictos"), Tone::Warning),
        PrStatus::ChecksFailing => (t!(lang, "Checks failing", "Checks fallando"), Tone::Critical),
        PrStatus::ChecksRunning => (t!(lang, "Checks running", "Checks en marcha"), Tone::Info),
        PrStatus::Draft => (t!(lang, "Draft", "Borrador"), Tone::Neutral),
        PrStatus::Blocked => (t!(lang, "Blocked on GitHub", "Bloqueado en GitHub"), Tone::Warning),
        PrStatus::Ready => (t!(lang, "Ready to merge", "Listo para merge"), Tone::Success),
        PrStatus::Unknown => (t!(lang, "Checking with GitHub…", "Consultando GitHub…"), Tone::Neutral),
    };
    Label { text: text.into(), tone, elapsed: None }
}

/// "3 of 5 passed, 1 failing, 1 running"; nothing when the PR has no checks.
pub fn checks_summary(checks: &[LiveCheck], lang: Lang) -> Option<Label> {
    if checks.is_empty() {
        return None;
    }
    let count = |state: CheckState| checks.iter().filter(|check| check.state == state).count();
    let (passed, failed, running) = (count(CheckState::Pass), count(CheckState::Fail), count(CheckState::Pending));
    let mut text = tf!(lang, "{passed} of {total} passed", "{passed} de {total} correctos", total = checks.len());
    if failed > 0 {
        text.push_str(&tf!(lang, ", {failed} failing", ", {failed} fallando"));
    }
    if running > 0 {
        text.push_str(&tf!(lang, ", {running} running", ", {running} en marcha"));
    }
    let tone = if failed > 0 {
        Tone::Critical
    } else if running > 0 {
        Tone::Info
    } else {
        Tone::Success
    };
    Some(Label { text, tone, elapsed: None })
}

/// Failures first, then the ones still running, then the ones that passed; by name within each.
pub fn ordered(checks: &[LiveCheck]) -> Vec<LiveCheck> {
    let rank = |state: CheckState| match state {
        CheckState::Fail => 0,
        CheckState::Pending => 1,
        CheckState::Pass => 2,
    };
    let mut ordered = checks.to_vec();
    ordered.sort_by(|a, b| rank(a.state).cmp(&rank(b.state)).then_with(|| a.name.cmp(&b.name)));
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PrId, TaskId};

    fn pr(state: PrState) -> PullRequest {
        PullRequest {
            id: PrId("pr".into()),
            task_id: TaskId("task".into()),
            host: "github.com".into(),
            repository: "o/r".into(),
            number: 7,
            head: "abc".into(),
            base: "main".into(),
            state,
            merged_head: None,
            allowed_methods: vec![],
            review: None,
        }
    }

    fn check(name: &str, state: CheckState) -> LiveCheck {
        LiveCheck { name: name.into(), state }
    }

    fn github(checks: Vec<LiveCheck>) -> LivePullRequest {
        LivePullRequest { id: "pr".into(), fetched: true, checks, ..LivePullRequest::default() }
    }

    #[test]
    fn one_state_by_priority_with_and_without_ci() {
        let failing = || github(vec![check("build", CheckState::Fail), check("lint", CheckState::Pending)]);
        let no_ci = || github(vec![]);
        let cases: Vec<(&str, PrState, Option<LivePullRequest>, PrStatus)> = vec![
            ("merged with failing checks", PrState::Merged, Some(failing()), PrStatus::Merged),
            ("closed with conflicts", PrState::Closed, Some(LivePullRequest { conflicts: true, ..failing() }), PrStatus::Closed),
            ("conflicts with failing checks", PrState::Open, Some(LivePullRequest { conflicts: true, ..failing() }), PrStatus::Conflicts),
            ("failing beats running", PrState::Open, Some(failing()), PrStatus::ChecksFailing),
            ("running", PrState::Open, Some(github(vec![check("build", CheckState::Pass), check("ui", CheckState::Pending)])), PrStatus::ChecksRunning),
            ("passing checks", PrState::Open, Some(github(vec![check("build", CheckState::Pass)])), PrStatus::Ready),
            ("no CI, merges cleanly", PrState::Open, Some(no_ci()), PrStatus::Ready),
            ("no CI, conflicts", PrState::Open, Some(LivePullRequest { conflicts: true, ..no_ci() }), PrStatus::Conflicts),
            ("no CI, draft", PrState::Open, Some(LivePullRequest { draft: true, ..no_ci() }), PrStatus::Draft),
            ("no CI, a required review", PrState::Open, Some(LivePullRequest { blocked: true, ..no_ci() }), PrStatus::Blocked),
            ("no CI, GitHub still computing", PrState::Open, Some(LivePullRequest { mergeability_unknown: true, ..no_ci() }), PrStatus::Unknown),
            ("GitHub has not answered", PrState::Open, Some(LivePullRequest { fetched: false, ..no_ci() }), PrStatus::Unknown),
            ("nothing known", PrState::Open, None, PrStatus::Unknown),
            ("merged, nothing known", PrState::Merged, None, PrStatus::Merged),
        ];
        for (what, state, live, expected) in cases {
            assert_eq!(status(&pr(state), live.as_ref()), expected, "{what}");
        }
    }

    #[test]
    fn checks_read_as_github_counts_them() {
        let checks = [check("a", CheckState::Pass), check("b", CheckState::Fail), check("c", CheckState::Pass), check("d", CheckState::Pending), check("e", CheckState::Pass)];
        let en = checks_summary(&checks, Lang::En).unwrap();
        assert_eq!((en.text.as_str(), en.tone), ("3 of 5 passed, 1 failing, 1 running", Tone::Critical));
        assert_eq!(checks_summary(&checks, Lang::Es).unwrap().text, "3 de 5 correctos, 1 fallando, 1 en marcha");
        let passed = checks_summary(&[check("a", CheckState::Pass)], Lang::Es).unwrap();
        assert_eq!((passed.text.as_str(), passed.tone), ("1 de 1 correctos", Tone::Success));
        assert_eq!(checks_summary(&[check("a", CheckState::Pending)], Lang::En).unwrap().tone, Tone::Info);
        assert_eq!(checks_summary(&[], Lang::En), None);
        let names: Vec<_> = ordered(&checks).into_iter().map(|check| check.name).collect();
        assert_eq!(names, ["b", "d", "a", "c", "e"]);
    }
}
