//! What the screens show after the domain runs its commands: each module drives the domain the way
//! its rules do and renders the result for every client.
mod activity_counts;
mod card;
mod checklist;
mod decision;
mod lifecycle;
mod plan_wait;
mod request_outcome;
mod several_prs;
mod stall;
mod turn_end_wait;
mod domain;

use alondra_domain_rules::{DomainState, InboxItem, ReadIndex};

/// The inbox as the index lists it, each card with its buttons.
pub(crate) fn inbox_items(state: &DomainState) -> Vec<InboxItem> {
    ReadIndex::build(state).queued(state).map(std::borrow::Cow::into_owned).collect()
}
