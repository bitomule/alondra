//! Whether GitHub would let a PR merge, from what `gh pr view` says and the checks its base requires.
//!
//! GitHub stays the judge: `mergeStateStatus` says whether the merge is allowed, and only when it
//! says the merge is blocked or behind do the review decision and the required checks explain why.
use alondra_domain_rules::{MergeBlocker, MergeNote, MergeVerdict};
use serde_json::Value;

/// `mergeable` and `mergeStateStatus` as the verdict needs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Clean,
    /// Mergeable although some check that the base does not require is not passing.
    Unstable,
    Blocked,
    Behind,
    Conflicts,
    Unknown,
}

pub fn gate(view: &Value) -> Gate {
    match (view["mergeable"].as_str(), view["mergeStateStatus"].as_str()) {
        (Some("CONFLICTING"), _) | (_, Some("DIRTY")) => Gate::Conflicts,
        (_, Some("CLEAN" | "HAS_HOOKS")) => Gate::Clean,
        (_, Some("UNSTABLE")) => Gate::Unstable,
        (_, Some("BLOCKED")) => Gate::Blocked,
        (_, Some("BEHIND")) => Gate::Behind,
        _ => Gate::Unknown,
    }
}

/// Whether the reasons for the block are worth asking the repository's rules about.
pub fn explains_with_rules(gate: Gate) -> bool {
    matches!(gate, Gate::Blocked | Gate::Behind)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    Passed,
    Failed,
    Running,
    NeedsAction,
}

/// One check as GitHub reports it, with the state that matters to a merge. Cancelled, stale,
/// skipped and neutral checks neither fail nor hold it.
pub fn classify(check: &Value) -> (String, CheckKind) {
    let s = |key: &str| check.get(key).and_then(Value::as_str).unwrap_or_default();
    let name = [s("name"), s("context")].into_iter().find(|name| !name.is_empty()).unwrap_or("check").to_owned();
    let kind = match (s("status"), s("conclusion"), s("state")) {
        (_, "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE", _) | (_, _, "FAILURE" | "ERROR") => CheckKind::Failed,
        (_, "ACTION_REQUIRED", _) => CheckKind::NeedsAction,
        (_, "SUCCESS" | "NEUTRAL" | "SKIPPED" | "CANCELLED" | "STALE", _) | (_, _, "SUCCESS") => CheckKind::Passed,
        _ => CheckKind::Running,
    };
    (name, kind)
}

fn checks(view: &Value) -> Vec<(String, CheckKind)> {
    view["statusCheckRollup"].as_array().into_iter().flatten().map(classify).collect()
}

fn names_check(blocker: &MergeBlocker, wanted: &str) -> bool {
    match blocker {
        MergeBlocker::RequiredCheckFailing { name } | MergeBlocker::RequiredCheckRunning { name }
        | MergeBlocker::RequiredCheckMissing { name } | MergeBlocker::CheckNeedsAction { name } => name == wanted,
        _ => false,
    }
}

fn notes(checks: &[(String, CheckKind)], required: &[String]) -> Vec<MergeNote> {
    checks.iter().filter(|(name, _)| !required.contains(name)).filter_map(|(name, kind)| match kind {
        CheckKind::Failed | CheckKind::NeedsAction => Some(MergeNote::CheckFailingNotRequired { name: name.clone() }),
        CheckKind::Running => Some(MergeNote::CheckRunningNotRequired { name: name.clone() }),
        CheckKind::Passed => None,
    }).collect()
}

/// The verdict for a PR GitHub reports in `view`, given the checks its base requires and the ones
/// the signed-in account may bypass (both only matter when GitHub blocks the merge).
pub fn derive(view: &Value, required: &[String], bypassable: &[String]) -> MergeVerdict {
    let gate = gate(view);
    let states = checks(view);
    match gate {
        Gate::Unknown => MergeVerdict::Unknown,
        Gate::Conflicts => MergeVerdict::Blocked { blockers: vec![MergeBlocker::Conflicts] },
        Gate::Clean => MergeVerdict::mergeable(),
        Gate::Unstable => MergeVerdict::Mergeable { notes: notes(&states, &[]) },
        Gate::Blocked | Gate::Behind => {
            let mut blockers = vec![];
            for name in required {
                let found = states.iter().find(|(check, _)| check == name).map(|(_, kind)| *kind);
                match found {
                    Some(CheckKind::Passed) => {}
                    Some(CheckKind::Failed) => blockers.push(MergeBlocker::RequiredCheckFailing { name: name.clone() }),
                    Some(CheckKind::Running) => blockers.push(MergeBlocker::RequiredCheckRunning { name: name.clone() }),
                    Some(CheckKind::NeedsAction) => blockers.push(MergeBlocker::CheckNeedsAction { name: name.clone() }),
                    None => blockers.push(MergeBlocker::RequiredCheckMissing { name: name.clone() }),
                }
            }
            let required_checks = blockers.clone();
            match view["reviewDecision"].as_str() {
                Some("REVIEW_REQUIRED") => blockers.push(MergeBlocker::ReviewRequired),
                Some("CHANGES_REQUESTED") => blockers.push(MergeBlocker::ChangesRequested),
                _ => {}
            }
            if gate == Gate::Behind {
                blockers.push(MergeBlocker::BehindBase);
            }
            let extra = notes(&states, required);
            if blockers.is_empty() {
                return if view["isDraft"].as_bool() == Some(true) { MergeVerdict::Mergeable { notes: extra } } else { MergeVerdict::Blocked { blockers: vec![MergeBlocker::Other] } };
            }
            let waived: Vec<String> = required.iter().filter(|name| blockers.iter().any(|blocker| names_check(blocker, name))).cloned().collect();
            let only_waiting = blockers.iter().all(|blocker| matches!(blocker, MergeBlocker::RequiredCheckRunning { .. } | MergeBlocker::RequiredCheckMissing { .. }));
            if blockers == required_checks && only_waiting && waived.iter().all(|name| bypassable.contains(name)) {
                let mut notes = vec![MergeNote::BypassingRules { names: waived }];
                notes.extend(extra);
                return MergeVerdict::Mergeable { notes };
            }
            MergeVerdict::Blocked { blockers }
        }
    }
}

#[cfg(test)]
#[path = "verdict_tests.rs"]
mod tests;
