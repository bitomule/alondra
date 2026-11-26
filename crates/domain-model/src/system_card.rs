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
    /// A crash report waits on this Mac because `report.repo` changed since it was first sent.
    ReportHeld { report: String },
    /// The node credential registry has no room: new starts wait.
    CapabilityCapacity,
    McpWait,
    ClaudeSignedOut,
    /// Claude's API, or this Mac's way to it, failed a task past Claude Code's own retries:
    /// Claude Code's finer code for the error and its words. A `quiet` one comes before Alondra
    /// retried: Waiting for You leaves it out while Alondra retries by itself.
    ClaudeOutage {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        quiet: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        said: Option<String>,
    },
    /// Claude's usage limit holds a task; it resets at `resets_at`, in Unix seconds, when Claude said.
    ClaudeUsage { resets_at: Option<u64> },
    /// A task's host could not be taken over; `signed_out` says its provider refused the account
    /// for want of a sign-in or a key, and the card asks to sign in.
    HostLost {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        signed_out: bool,
    },
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
    /// A change of a task's approved specs, or a lasting decision, for the user to decide on.
    SpecChange,
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
    TasksEndedNotice,
}

/// The ids Alondra's own cards were named with: a node or the orchestrator may not name a card so.
pub const RESERVED_PREFIXES: [&str; 41] = [
    crate::spec_card::AMENDMENT_PREFIX,
    crate::spec_card::DECISION_PREFIX,
    super::NODE_PAUSE_PREFIX,
    alondra_core::usage::OVER_PACE_PREFIX,
    alondra_core::usage::PACE_RESUME_PREFIX,
    alondra_core::machine_incident::MACHINE_PREFIX,
    crate::local_network_card::LOCAL_NETWORK_PREFIX,
    crate::report_held_card::REPORT_HELD_PREFIX,
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
    crate::orchestrator_cards::TASKS_ENDED_NOTICE_PREFIX,
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
    /// What a card stored before Alondra recorded its kind is, from its id and the cause it was
    /// raised with: a refused-cards card names the node it is about in its cause. Only the
    /// database's one-time upgrade reads it.
    pub fn of_stored(id: &str, cause: Option<&str>) -> Option<Self> {
        match cause.and_then(|cause| cause.strip_prefix("refused-cards:")) {
            Some(owner) => Some(Self::RefusedCards { owner: owner.to_owned() }),
            None => Self::of_stored_id(id),
        }
    }

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
            (alondra_core::usage::OVER_PACE_PREFIX, |day| Some(Self::OverPace { day: day.to_owned() })),
            (alondra_core::usage::PACE_RESUME_PREFIX, |day| Some(Self::PaceResume { day: day.to_owned() })),
            (alondra_core::machine_incident::MACHINE_PREFIX, |_| Some(Self::Machine)),
            (crate::local_network_card::LOCAL_NETWORK_PREFIX, |_| Some(Self::LocalNetwork)),
            (super::MCP_WAIT_PREFIX, |_| Some(Self::McpWait)),
            (super::SIGNED_OUT_PREFIX, |_| Some(Self::ClaudeSignedOut)),
            (super::OUTAGE_PREFIX, |_| Some(Self::ClaudeOutage { quiet: false, detail: None, said: None })),
            (super::USAGE_PREFIX, |_| Some(Self::ClaudeUsage { resets_at: None })),
            (super::HOST_LOST_PREFIX, |_| Some(Self::HostLost { signed_out: false })),
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
            (inbox::PLAN_REFUSED_NOTICE_PREFIX, InboxCard::PlanRefusedNotice),
            (inbox::TASKS_ENDED_NOTICE_PREFIX, InboxCard::TasksEndedNotice),
        ];
        if let Some((rest, about)) = counted.into_iter().find_map(|(prefix, about)| Some((after(prefix)?, about))) {
            return numbered(rest, about);
        }
        let once = [(inbox::UNSETTLED_NOTICE_PREFIX, InboxCard::UnsettledNotice), (inbox::STALL_NOTICE_PREFIX, InboxCard::StallNotice)];
        once.into_iter().find_map(|(prefix, about)| Some(Self::Inbox { about, subject: after(prefix)?.to_owned(), number: 1 }))
    }
}
