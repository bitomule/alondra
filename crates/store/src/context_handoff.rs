//! Bounded public context for a fresh provider session. This is a token estimate, not native
//! telemetry: changing models never carries the previous model's context window forward.
use crate::{ContextEntry, ContextRef, HistoryCoverage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What the earlier public conversation may add to the task's records in a handoff.
pub const HANDOFF_TOKEN_CAP: u64 = 16_000;
/// What a handoff may take when the destination model's window is unknown: well under the
/// smallest window of the models Alondra runs, 200,000 tokens.
pub const UNKNOWN_WINDOW_TOKENS: u64 = 64_000;
/// What a handoff leaves free in the destination's window for its answer.
pub const HANDOFF_RESERVED_TOKENS: u64 = 4_096;

#[derive(Clone, Copy, Debug)]
pub struct HandoffBudget {
    pub destination_context_tokens: Option<u64>,
    pub reserved_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffError { IncompleteHistory(String), NewInputTooLarge, CriticalContextTooLarge { needed: u64, available: u64 } }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedHandoff {
    pub context: String,
    pub new_input: String,
    pub digest: Vec<u8>,
    pub references: Vec<ContextRef>,
    pub coverage: HistoryCoverage,
    pub omitted_items: usize,
    pub estimated_context_tokens: u64,
    pub destination_window_known: bool,
}

/// What the context and the message that continues the task may take together in the destination:
/// its window less the reserve, or `UNKNOWN_WINDOW_TOKENS` when the window is unknown. `None` when
/// the reserve alone does not fit.
pub fn handoff_capacity(budget: HandoffBudget) -> Option<u64> {
    match budget.destination_context_tokens {
        Some(window) => window.checked_sub(budget.reserved_tokens),
        None => Some(UNKNOWN_WINDOW_TOKENS),
    }
}

pub fn estimated_tokens(text: &str) -> u64 {
    // Three UTF-8 bytes per token is deliberately below the usual four-byte text heuristic.
    (text.len() as u64).div_ceil(3)
}

pub fn prepare_context_handoff(canonical: &str, new_input: &str, entries: &[ContextEntry], coverage: &HistoryCoverage, after: Option<&ContextRef>, budget: HandoffBudget) -> std::result::Result<PreparedHandoff, HandoffError> {
    if coverage.omitted_items > 0 { return Err(HandoffError::IncompleteHistory(coverage.reason.clone().unwrap_or_else(|| "Some provider conversation content is no longer available".into()))); }
    if entries.windows(2).any(|pair| pair[0].reference.task != pair[1].reference.task || pair[0].reference.sequence >= pair[1].reference.sequence) {
        return Err(HandoffError::IncompleteHistory("Context items are not one ordered task history".into()));
    }
    if let Some(after) = after {
        if !entries.iter().any(|entry| &entry.reference == after) {
            return Err(HandoffError::IncompleteHistory("The continuation cursor is not an exact retained context reference".into()));
        }
    }
    let input_tokens = estimated_tokens(new_input);
    let window = match budget.destination_context_tokens {
        Some(_) => handoff_capacity(budget).and_then(|tokens| tokens.checked_sub(input_tokens)).ok_or(HandoffError::NewInputTooLarge)?,
        None => UNKNOWN_WINDOW_TOKENS,
    };
    let header = format!("Alondra task context (public records)\n{canonical}\n");
    let records = estimated_tokens(&header);
    if records > window { return Err(HandoffError::CriticalContextTooLarge { needed: records, available: window }); }
    let available = window.min(records.saturating_add(HANDOFF_TOKEN_CAP));
    let relevant: Vec<_> = entries.iter().filter(|entry| after.is_none_or(|after| entry.reference.task != after.task || entry.reference.sequence > after.sequence)).collect();
    if after.is_some_and(|after| entries.iter().any(|entry| entry.reference.task != after.task)) {
        return Err(HandoffError::IncompleteHistory("The continuation cursor belongs to another task".into()));
    }
    let mut chosen = vec![];
    let mut used = estimated_tokens(&header);
    let omission_reserve = estimated_tokens("\nEarlier public conversation items were omitted to fit the destination.\n");
    for entry in relevant.iter().rev() {
        let omission = if entry.item.omitted_bytes > 0 { format!("\n[{} bytes omitted; {}]", entry.item.omitted_bytes, entry.item.artifact.as_deref().unwrap_or("no complete artifact retained")) } else { String::new() };
        let content = format!("\n{:?} [{}]:\n{}{}\n", entry.item.source, entry.reference.sequence, entry.item.content, omission);
        let tokens = estimated_tokens(&content);
        if used.saturating_add(tokens).saturating_add(omission_reserve) > available { break; }
        used += tokens;
        chosen.push((*entry, content));
    }
    chosen.reverse();
    let omitted_items = relevant.len() - chosen.len();
    let mut context = header;
    if omitted_items > 0 { context.push_str(&format!("\n{omitted_items} earlier public conversation items were omitted to fit the destination.\n")); }
    let references = chosen.iter().map(|(entry, _)| entry.reference.clone()).collect();
    for (_, content) in chosen { context.push_str(&content); }
    if estimated_tokens(&context) > available { return Err(HandoffError::CriticalContextTooLarge { needed: estimated_tokens(&context), available }); }
    let mut hash = Sha256::new();
    hash.update((context.len() as u64).to_be_bytes());
    hash.update(context.as_bytes());
    hash.update(new_input.as_bytes());
    let mut coverage = coverage.clone();
    coverage.through = relevant.last().map(|entry| entry.reference.clone()).or_else(|| after.cloned());
    Ok(PreparedHandoff { estimated_context_tokens: estimated_tokens(&context), context, new_input: new_input.into(), digest: hash.finalize().to_vec(), references, coverage, omitted_items, destination_window_known: budget.destination_context_tokens.is_some() })
}

/// What a new session of the same model and account starts from, in a few thousand tokens: the rest
/// of the task is in files it reads on demand.
#[derive(Clone, Debug)]
pub struct RefreshParts {
    pub worktree: String,
    pub branch: Option<String>,
    pub pull_requests: Vec<String>,
    pub plan: Option<String>,
    pub plan_file: Option<String>,
    pub last_assistant_message: Option<String>,
    pub records_file: String,
    pub conversation_file: String,
}

/// What an approved plan may take inside the handoff before it goes to its file.
const INLINE_PLAN_TOKENS: u64 = 3_000;
/// What the assistant's last message may take inside the handoff.
const LAST_MESSAGE_TOKENS: u64 = 3_000;

fn tail(text: &str, tokens: u64) -> String {
    let limit = (tokens * 3) as usize;
    if text.len() <= limit { return text.to_owned(); }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) { start += 1; }
    format!("[the beginning of this message is cut; it is in the conversation file]\n{}", &text[start..])
}

fn refresh_context(parts: &RefreshParts, plan_inline: bool, last_message: Option<&str>) -> String {
    let mut text = String::from("Alondra task context (this session continues the previous one of this task)\n");
    text.push_str(&format!("\nWorktree: {}\n", parts.worktree));
    if let Some(branch) = &parts.branch { text.push_str(&format!("Branch: {branch}\n")); }
    for pull_request in &parts.pull_requests { text.push_str(&format!("Pull request: {pull_request}\n")); }
    match (&parts.plan, &parts.plan_file) {
        (Some(plan), _) if plan_inline => text.push_str(&format!("\n{plan}\n")),
        (_, Some(file)) => text.push_str(&format!("\nThe plan the user approved is in {file}: read it before you go on.\n")),
        _ => {}
    }
    if let Some(message) = last_message {
        text.push_str(&format!("\n## Your last message before this session\n{message}\n"));
    }
    text.push_str(&format!("\n## Where the rest is\nThe whole conversation of the session before this one is in {conversation}. \
Do not read it whole: it is reference material. Each of its turns starts with a line `## Turn N`: find the one you need with `grep -n '## Turn' {conversation}` and read only that turn.\n\
The task's complete records (cards, reports, decisions, every input) are in {records}: search it with grep and read only the part you need, never the whole file.\n\
Go on with the message that follows.\n", conversation = parts.conversation_file, records = parts.records_file));
    text
}

/// The short handoff of a refresh. It always fits: the plan goes to its file when it is long or
/// when the whole does not fit, then the assistant's last message is cut; only the fixed lines
/// (paths, worktree, branch) can still not fit.
pub fn prepare_refresh_handoff(parts: &RefreshParts, new_input: &str, through: Option<ContextRef>, budget: HandoffBudget) -> std::result::Result<PreparedHandoff, HandoffError> {
    let capacity = handoff_capacity(budget).and_then(|tokens| tokens.checked_sub(estimated_tokens(new_input))).ok_or(HandoffError::NewInputTooLarge)?;
    let available = capacity.min(HANDOFF_TOKEN_CAP);
    let plan_fits = parts.plan.as_deref().is_some_and(|plan| estimated_tokens(plan) <= INLINE_PLAN_TOKENS);
    let message = parts.last_assistant_message.as_deref().map(|message| tail(message, LAST_MESSAGE_TOKENS));
    let mut context = refresh_context(parts, plan_fits, message.as_deref());
    if estimated_tokens(&context) > available { context = refresh_context(parts, false, message.as_deref()); }
    if estimated_tokens(&context) > available { context = refresh_context(parts, false, None); }
    if estimated_tokens(&context) > available { return Err(HandoffError::CriticalContextTooLarge { needed: estimated_tokens(&context), available }); }
    let mut hash = Sha256::new();
    hash.update((context.len() as u64).to_be_bytes());
    hash.update(context.as_bytes());
    hash.update(new_input.as_bytes());
    let coverage = HistoryCoverage { through, ..Default::default() };
    Ok(PreparedHandoff { estimated_context_tokens: estimated_tokens(&context), context, new_input: new_input.into(), digest: hash.finalize().to_vec(), references: vec![], coverage, omitted_items: 0, destination_window_known: budget.destination_context_tokens.is_some() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContextSource, TaskContextItem};

    fn item(sequence: u64, body: &str) -> ContextEntry {
        ContextEntry { reference: ContextRef { task: "task".into(), sequence, generation: 1, cursor: sequence }, item: TaskContextItem::text(format!("item-{sequence}"), ContextSource::Assistant, body).unwrap() }
    }
    fn budget() -> HandoffBudget { HandoffBudget { destination_context_tokens: Some(10_000), reserved_tokens: 1_000 } }

    #[test]
    fn handoff_preserves_new_input_and_uses_recent_history_with_explicit_omissions() {
        let entries = vec![item(1, &"old".repeat(15_000)), item(2, "The recent decision")];
        let prepared = prepare_context_handoff("Request and approved plan", "The new user input", &entries, &HistoryCoverage::default(), None, budget()).unwrap();
        assert_eq!(prepared.new_input, "The new user input");
        assert!(prepared.context.contains("The recent decision"));
        assert!(!prepared.context.contains(&"old".repeat(100)));
        assert_eq!(prepared.omitted_items, 1);
        assert_eq!(prepared.references, vec![entries[1].reference.clone()]);
        assert!(prepared.estimated_context_tokens < 9_000);
    }

    #[test]
    fn handoff_rejects_lost_critical_history_and_never_truncates_new_input() {
        let coverage = HistoryCoverage { omitted_items: 1, reason: Some("Content was purged".into()), ..Default::default() };
        assert!(matches!(prepare_context_handoff("Task", "New input", &[], &coverage, None, budget()), Err(HandoffError::IncompleteHistory(_))));
        assert_eq!(prepare_context_handoff("Task", &"x".repeat(30_000), &[], &HistoryCoverage::default(), None, budget()), Err(HandoffError::NewInputTooLarge));
    }

    #[test]
    fn records_beyond_the_conversation_cap_move_whole_when_the_destination_holds_them() {
        let records = "r".repeat(60_000);
        let entries = vec![item(1, "The recent decision")];
        let roomy = HandoffBudget { destination_context_tokens: Some(200_000), reserved_tokens: 4_096 };
        let prepared = prepare_context_handoff(&records, "Continue", &entries, &HistoryCoverage::default(), None, roomy).unwrap();
        assert!(prepared.context.contains(&records), "the task's records are never cut to the conversation cap");
        assert!(prepared.context.contains("The recent decision"));
        let unknown = HandoffBudget { destination_context_tokens: None, reserved_tokens: 4_096 };
        assert!(prepare_context_handoff(&records, "Continue", &entries, &HistoryCoverage::default(), None, unknown).unwrap().context.contains(&records));
    }

    #[test]
    fn records_the_destination_cannot_hold_are_refused_by_size_never_cut() {
        let records = "r".repeat(3 * UNKNOWN_WINDOW_TOKENS as usize + 3);
        let unknown = HandoffBudget { destination_context_tokens: None, reserved_tokens: 4_096 };
        assert!(matches!(prepare_context_handoff(&records, "Continue", &[], &HistoryCoverage::default(), None, unknown),
            Err(HandoffError::CriticalContextTooLarge { available: UNKNOWN_WINDOW_TOKENS, .. })));
        let small = HandoffBudget { destination_context_tokens: Some(20_000), reserved_tokens: 4_096 };
        let Err(HandoffError::CriticalContextTooLarge { needed, available }) = prepare_context_handoff(&"r".repeat(60_000), "Continue", &[], &HistoryCoverage::default(), None, small) else { panic!("too large for the destination") };
        assert!(needed > available && available < 16_000, "{needed} {available}");
    }

    #[test]
    fn returning_session_gets_only_the_unconsumed_delta_and_a_stable_digest() {
        let entries = vec![item(1, "Already consumed"), item(2, "New context")];
        let first = prepare_context_handoff("Canonical records", "New input", &entries, &HistoryCoverage::default(), Some(&entries[0].reference), budget()).unwrap();
        let replay = prepare_context_handoff("Canonical records", "New input", &entries, &HistoryCoverage::default(), Some(&entries[0].reference), budget()).unwrap();
        assert!(!first.context.contains("Already consumed"));
        assert!(first.context.contains("New context"));
        assert_eq!(first.digest, replay.digest);
        assert_eq!(first.coverage.through, Some(entries[1].reference.clone()));
    }
    #[test]
    fn delta_requires_the_exact_retained_reference_and_one_ordered_task() {
        let entries = vec![item(1, "First"), item(2, "Second")];
        let mut forged = entries[0].reference.clone();
        forged.generation += 1;
        assert!(matches!(prepare_context_handoff("Task", "New", &entries, &HistoryCoverage::default(), Some(&forged), budget()), Err(HandoffError::IncompleteHistory(_))));
        let mut future = forged;
        future.sequence = 99;
        assert!(prepare_context_handoff("Task", "New", &entries, &HistoryCoverage::default(), Some(&future), budget()).is_err());
        let mut unsorted = entries.clone();
        unsorted.reverse();
        assert!(prepare_context_handoff("Task", "New", &unsorted, &HistoryCoverage::default(), None, budget()).is_err());
    }


    fn parts(plan: &str, message: &str) -> RefreshParts {
        RefreshParts { worktree: "/work/tree".into(), branch: Some("feature".into()), pull_requests: vec!["https://github.com/o/r/pull/1".into()], plan: Some(plan.into()), plan_file: Some("/files/plan.md".into()), last_assistant_message: Some(message.into()), records_file: "/files/records.md".into(), conversation_file: "/files/conversation.md".into() }
    }
    fn unknown() -> HandoffBudget { HandoffBudget { destination_context_tokens: None, reserved_tokens: 4_096 } }

    #[test]
    fn refresh_handoff_is_short_and_names_where_the_rest_is() {
        let prepared = prepare_refresh_handoff(&parts("Approved plan", "Last words"), "Go on", None, unknown()).unwrap();
        for wanted in ["/work/tree", "feature", "pull/1", "Approved plan", "Last words", "/files/conversation.md", "/files/records.md", "Do not read it whole"] {
            assert!(prepared.context.contains(wanted), "{wanted}");
        }
        assert!(prepared.estimated_context_tokens < 1_000);
    }

    #[test]
    fn refresh_handoff_sends_a_long_plan_by_its_path_and_always_fits() {
        let huge = "p".repeat(3 * 200_000);
        let prepared = prepare_refresh_handoff(&parts(&huge, &huge), "Go on", None, unknown()).unwrap();
        assert!(prepared.context.contains("/files/plan.md"));
        assert!(!prepared.context.contains(&huge));
        assert!(prepared.estimated_context_tokens <= HANDOFF_TOKEN_CAP);
        let tight = HandoffBudget { destination_context_tokens: Some(6_000), reserved_tokens: 4_096 };
        let small = prepare_refresh_handoff(&parts("Plan", &huge), "Go on", None, tight).unwrap();
        assert!(small.estimated_context_tokens + estimated_tokens("Go on") <= 1_904);
    }
}
