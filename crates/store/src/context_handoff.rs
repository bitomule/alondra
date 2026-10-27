//! Bounded public context for a fresh provider session. This is a token estimate, not native
//! telemetry: changing models never carries the previous model's context window forward.
use crate::{ContextEntry, ContextRef, HistoryCoverage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const HANDOFF_TOKEN_CAP: u64 = 16_000;

#[derive(Clone, Copy, Debug)]
pub struct HandoffBudget {
    pub destination_context_tokens: Option<u64>,
    pub reserved_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HandoffError { IncompleteHistory(String), NewInputTooLarge, CriticalContextTooLarge }

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
    let available = match budget.destination_context_tokens {
        Some(window) => window.checked_sub(budget.reserved_tokens).and_then(|tokens| tokens.checked_sub(input_tokens)).ok_or(HandoffError::NewInputTooLarge)?.min(HANDOFF_TOKEN_CAP),
        None => HANDOFF_TOKEN_CAP,
    };
    let header = format!("Alondra task context (public records)\n{canonical}\n");
    if estimated_tokens(&header) > available { return Err(HandoffError::CriticalContextTooLarge); }
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
    if estimated_tokens(&context) > available { return Err(HandoffError::CriticalContextTooLarge); }
    let mut hash = Sha256::new();
    hash.update((context.len() as u64).to_be_bytes());
    hash.update(context.as_bytes());
    hash.update(new_input.as_bytes());
    let mut coverage = coverage.clone();
    coverage.through = relevant.last().map(|entry| entry.reference.clone()).or_else(|| after.cloned());
    Ok(PreparedHandoff { estimated_context_tokens: estimated_tokens(&context), context, new_input: new_input.into(), digest: hash.finalize().to_vec(), references, coverage, omitted_items, destination_window_known: budget.destination_context_tokens.is_some() })
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

}
