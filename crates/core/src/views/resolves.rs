use super::action_id::{ActionId, Verb};
use crate::domain::{DeliveryTarget, DomainState, Interaction, InteractionId, InteractionPayload, PrId, PrState, PullRequest};

/// Whether an action, applied now, takes its card out of the queue. The clients hide the card the
/// moment it is sent when this is true, and leave it in place otherwise: they never work it out
/// from the action's name. The daemon decides what a message becomes with the same rule.
pub fn resolves_card(domain: &DomainState, action: &ActionId) -> bool {
    always_resolves(action.verb) || (handed_back(domain, action) && !reply_nobody_attends(domain, action))
}

/// A reply to a delivery takes it away while its node acts on it, and the delivery comes back once
/// the node is done with it (`DomainState::open_reply`). A node that is paused, lost or gone cannot
/// act on it now, so the delivery stays in front of the user.
fn reply_nobody_attends(domain: &DomainState, action: &ActionId) -> bool {
    action.verb == Verb::Reply
        && domain.interactions.get(&InteractionId(action.subject.clone()))
            .is_some_and(|interaction| matches!(interaction.payload, InteractionPayload::Delivery { .. })
                && !domain.node_attends(interaction))
}

/// An answer, an acceptance, a read or a request for changes always settles the card it came from, and
/// clearing the completed work empties the section whose header offers it.
pub fn always_resolves(verb: Verb) -> bool {
    matches!(verb, Verb::Ans | Verb::Ctx | Verb::Changes | Verb::Read | Verb::Accept | Verb::Merge | Verb::CloseUnmerged
        | Verb::ClearCompleted)
}

/// A reply to a delivery, a question about a presented PR or a request to resolve its conflicts
/// is feedback on that delivery, and a reply to an open question answers it, held by Claude Code
/// or not. With nothing to hand back the text only reaches the node.
pub fn handed_back(domain: &DomainState, action: &ActionId) -> bool {
    match action.verb {
        Verb::Reply => domain.interactions.get(&InteractionId(action.subject.clone()))
            .is_some_and(|interaction| matches!(&interaction.payload, InteractionPayload::Delivery { .. }
                | InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })),
        Verb::AskPr | Verb::Conflicts => domain.pull_requests.get(&PrId(action.subject.clone()))
            .is_some_and(|pr| pr_presentation(domain, pr).is_some()),
        _ => false,
    }
}

/// The open PR's current final delivery, presented at the PR's head.
pub fn pr_presentation<'a>(domain: &'a DomainState, pr: &PullRequest) -> Option<&'a Interaction> {
    if pr.state != PrState::Open { return None; }
    let task = domain.tasks.get(&pr.task_id).filter(|task| !task.state.terminal())?;
    let presentation = domain.interactions.get(task.final_presentation.as_ref()?)?;
    let InteractionPayload::Delivery { targets, .. } = &presentation.payload else { return None; };
    targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { pr_id, head }
        if pr_id == &pr.id && head == &pr.head)).then_some(presentation)
}
