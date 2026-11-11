//! What one of Alondra's own cards is. The command that creates the card says it, and every
//! effect of the card and every screen that draws it decide on it, never on the card's id: a node
//! names its cards, and an id it chose must never pause a node or resume a pause of the user.
use super::{InteractionId, Millis};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SystemCard {
    /// The gate holds a node of the Agent SDK paused since `since`.
    NodePause { pause: NodePauseKind, since: Millis },
    /// The pace of the day went over its budget, on `day`.
    OverPace { day: String },
    /// Every node is paused for the pace and waits for the user to resume them.
    PaceResume { day: String },
    Machine,
    LocalNetwork,
    McpWait,
    ClaudeSignedOut,
    ClaudeOutage,
    /// Claude's usage limit holds a task; it resets at `resets_at`, in Unix seconds, when Claude said.
    ClaudeUsage { resets_at: Option<u64> },
    HostLost,
    TurnEnded,
    Stalled,
    SessionEnded,
    HandoffDoubt,
    DecisionCancelled,
    Dropped,
    Unheard,
    /// A question that was still unanswered when its task ended.
    EndedUnanswered { question: InteractionId },
    StartProblem,
    PostMergeProblem,
    CloseProblem,
    /// Cancelled tasks whose worktrees hold changes no commit has, and the user decides on them.
    DiscardWorktrees,
    /// The open pull request of a task closed without delivering it, which Alondra delivers.
    OpenPrDelivery,
    LaunchProblem { run: String },
    BatchProblem { run: String },
    StuckProblem,
    RequestDone,
    /// The tasks of a request of the user, as the orchestrator gave them.
    RequestTasks,
    /// A reminder or a Problem the orchestrator's inbox raised about `subject`, the `number`th.
    Inbox { about: InboxCard, subject: String, number: u32 },
    /// Alondra refused the same card of `owner` again and again.
    RefusedCards { owner: String },
    /// A kind a newer daemon wrote: it has no effect here.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodePauseKind {
    User,
    Machine,
    Usage,
    Disk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxCard {
    UnsplitNotice,
    UnsplitProblem,
    UnansweredNotice,
    UnansweredProblem,
    UnsettledNotice,
    StallNotice,
    PlanReviewNotice,
    PlanReviewProblem,
    DependencyCancelledNotice,
    PlanRefusedNotice,
}

/// The ids Alondra's own cards were named with: a node or the orchestrator may not name a card so.
pub const RESERVED_PREFIXES: [&str; 37] = [
    super::NODE_PAUSE_PREFIX,
    crate::usage::OVER_PACE_PREFIX,
    crate::usage::PACE_RESUME_PREFIX,
    crate::machine_incident::MACHINE_PREFIX,
    crate::local_network_card::LOCAL_NETWORK_PREFIX,
    super::MCP_WAIT_PREFIX,
    super::SIGNED_OUT_PREFIX,
    super::OUTAGE_PREFIX,
    super::USAGE_PREFIX,
    super::HOST_LOST_PREFIX,
    super::TURN_ENDED_PREFIX,
    super::STALLED_PREFIX,
    super::SESSION_ENDED_PREFIX,
    super::HANDOFF_DOUBT_PREFIX,
    super::DECISION_CANCELLED_PREFIX,
    super::DROPPED_PREFIX,
    super::UNHEARD_PREFIX,
    super::ENDED_UNANSWERED_PREFIX,
    super::START_PROBLEM_PREFIX,
    super::POST_MERGE_PROBLEM_PREFIX,
    crate::close_problem::CLOSE_PROBLEM_PREFIX,
    super::LAUNCH_PROBLEM_PREFIX,
    crate::orchestrator_batch::BATCH_PROBLEM_PREFIX,
    crate::orchestrator_cards::STUCK_PROBLEM_PREFIX,
    crate::request_outcome::DONE_PREFIX,
    crate::orchestrator_cards::UNSPLIT_NOTICE_PREFIX,
    crate::orchestrator_cards::UNSPLIT_PROBLEM_PREFIX,
    crate::orchestrator_cards::UNANSWERED_NOTICE_PREFIX,
    crate::orchestrator_cards::UNANSWERED_PROBLEM_PREFIX,
    crate::orchestrator_cards::UNSETTLED_NOTICE_PREFIX,
    crate::orchestrator_cards::STALL_NOTICE_PREFIX,
    crate::orchestrator_cards::PLAN_REVIEW_NOTICE_PREFIX,
    super::PLAN_REVIEW_PROBLEM_PREFIX,
    crate::orchestrator_cards::DEPENDENCY_CANCELLED_NOTICE_PREFIX,
    crate::orchestrator_cards::PLAN_REFUSED_NOTICE_PREFIX,
    crate::open_pr_delivery::OPEN_PR_DELIVERY_PREFIX,
    ALONDRA_PROBLEM_PREFIX,
];
/// What every Problem Alondra raises is named with.
pub const ALONDRA_PROBLEM_PREFIX: &str = "problem-";

/// Whether a card named `id` by a node or the orchestrator would take the name of one of Alondra's.
pub fn reserved_card_id(id: &str) -> bool {
    RESERVED_PREFIXES.iter().any(|prefix| id.starts_with(prefix))
}

impl SystemCard {
    /// What a card stored before Alondra recorded its kind is, from the id Alondra gave it. Only
    /// the database's one-time upgrade reads it.
    pub fn of_stored_id(id: &str) -> Option<Self> {
        use crate::orchestrator_cards as inbox;
        let numbered = |rest: &str, about| {
            let (subject, number) = rest.rsplit_once('-')?;
            Some(Self::Inbox { about, subject: subject.to_owned(), number: number.parse().ok()? })
        };
        let after = |prefix: &str| id.strip_prefix(prefix);
        if let Some(rest) = after(super::NODE_PAUSE_PREFIX) {
            let (kind, rest) = rest.split_once('-')?;
            let pause = match kind {
                "user" => NodePauseKind::User,
                "machine" => NodePauseKind::Machine,
                "usage" => NodePauseKind::Usage,
                "disk" => NodePauseKind::Disk,
                _ => return None,
            };
            return Some(Self::NodePause { pause, since: rest.split_once('-')?.0.parse().ok()? });
        }
        if let Some(rest) = after(super::USAGE_UNTIL) {
            return Some(Self::ClaudeUsage { resets_at: rest.split('-').next()?.parse().ok() });
        }
        type Read = fn(&str) -> Option<SystemCard>;
        let cards: [(&str, Read); 26] = [
            (crate::usage::OVER_PACE_PREFIX, |day| Some(Self::OverPace { day: day.to_owned() })),
            (crate::usage::PACE_RESUME_PREFIX, |day| Some(Self::PaceResume { day: day.to_owned() })),
            (crate::machine_incident::MACHINE_PREFIX, |_| Some(Self::Machine)),
            (crate::local_network_card::LOCAL_NETWORK_PREFIX, |_| Some(Self::LocalNetwork)),
            (super::MCP_WAIT_PREFIX, |_| Some(Self::McpWait)),
            (super::SIGNED_OUT_PREFIX, |_| Some(Self::ClaudeSignedOut)),
            (super::OUTAGE_PREFIX, |_| Some(Self::ClaudeOutage)),
            (super::USAGE_PREFIX, |_| Some(Self::ClaudeUsage { resets_at: None })),
            (super::HOST_LOST_PREFIX, |_| Some(Self::HostLost)),
            (super::TURN_ENDED_PREFIX, |_| Some(Self::TurnEnded)),
            (super::STALLED_PREFIX, |_| Some(Self::Stalled)),
            (super::SESSION_ENDED_PREFIX, |_| Some(Self::SessionEnded)),
            (super::HANDOFF_DOUBT_PREFIX, |_| Some(Self::HandoffDoubt)),
            (super::DECISION_CANCELLED_PREFIX, |_| Some(Self::DecisionCancelled)),
            (super::DROPPED_PREFIX, |_| Some(Self::Dropped)),
            (super::UNHEARD_PREFIX, |_| Some(Self::Unheard)),
            (super::ENDED_UNANSWERED_PREFIX, |question| Some(Self::EndedUnanswered { question: question.into() })),
            (super::START_PROBLEM_PREFIX, |_| Some(Self::StartProblem)),
            (super::POST_MERGE_PROBLEM_PREFIX, |_| Some(Self::PostMergeProblem)),
            (crate::close_problem::CLOSE_PROBLEM_PREFIX, |_| Some(Self::CloseProblem)),
            (crate::open_pr_delivery::OPEN_PR_DELIVERY_PREFIX, |_| Some(Self::OpenPrDelivery)),
            (super::LAUNCH_PROBLEM_PREFIX, |run| Some(Self::LaunchProblem { run: run.to_owned() })),
            (crate::orchestrator_cards::STUCK_PROBLEM_PREFIX, |_| Some(Self::StuckProblem)),
            (crate::orchestrator_batch::BATCH_PROBLEM_PREFIX, |run| Some(Self::BatchProblem { run: run.to_owned() })),
            (crate::request_outcome::DONE_PREFIX, |_| Some(Self::RequestDone)),
            (crate::request_outcome::TASKS_PREFIX, |_| Some(Self::RequestTasks)),
        ];
        if let Some((rest, read)) = cards.into_iter().find_map(|(prefix, read)| Some((after(prefix)?, read))) {
            return read(rest);
        }
        let counted = [
            (inbox::UNSPLIT_NOTICE_PREFIX, InboxCard::UnsplitNotice),
            (inbox::UNSPLIT_PROBLEM_PREFIX, InboxCard::UnsplitProblem),
            (inbox::UNANSWERED_NOTICE_PREFIX, InboxCard::UnansweredNotice),
            (inbox::UNANSWERED_PROBLEM_PREFIX, InboxCard::UnansweredProblem),
            (inbox::PLAN_REVIEW_NOTICE_PREFIX, InboxCard::PlanReviewNotice),
            (super::PLAN_REVIEW_PROBLEM_PREFIX, InboxCard::PlanReviewProblem),
            (inbox::DEPENDENCY_CANCELLED_NOTICE_PREFIX, InboxCard::DependencyCancelledNotice),
        ];
        if let Some((rest, about)) = counted.into_iter().find_map(|(prefix, about)| Some((after(prefix)?, about))) {
            return numbered(rest, about);
        }
        let once = [(inbox::UNSETTLED_NOTICE_PREFIX, InboxCard::UnsettledNotice), (inbox::STALL_NOTICE_PREFIX, InboxCard::StallNotice),
            (inbox::PLAN_REFUSED_NOTICE_PREFIX, InboxCard::PlanRefusedNotice)];
        once.into_iter().find_map(|(prefix, about)| Some(Self::Inbox { about, subject: after(prefix)?.to_owned(), number: 1 }))
    }
}
