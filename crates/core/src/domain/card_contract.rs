//! The catalog of every card Alondra can put in front of someone, and the contract each type
//! keeps: who creates it, who reads it, what it carries, what its reader can do with it, what it
//! refers to and when it goes away by itself. A card is checked against its contract before it
//! exists, by the type the command and its fields give it, never by its words; a card that breaks
//! its contract is refused with a `ContractBreach` naming the rule and the field.
use super::{Author, CardKind, CardSpec, InteractionPayload, Recipient, SystemCard};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardType {
    /// `ask`: a decision between options.
    Question,
    /// `request_action`: something the user does outside Alondra.
    Action,
    /// `request_input`: a file, a text or a secret only the user has.
    Input,
    /// `request_test`: the user tries a build or a screen.
    Trial,
    /// `suggest`: something a node saw outside its task, for the orchestrator.
    Suggestion,
    /// `publish_information`: a notice to read.
    Notice,
    /// `raise_problem`: something failed and someone must act.
    Problem,
    /// `present_result` with `is_final`: the work to accept.
    Result,
    /// `present_result` with `is_final` and a `plan`: a plan to approve.
    ProposedPlan,
    /// `present_result` without `is_final`: progress to read.
    Progress,
    /// A Claude Code permission prompt, held for the user.
    Permission,
    /// A native plan, held for approval.
    HeldPlan,
    /// A Claude Code menu question, held for the user.
    HeldQuestion,
    /// An MCP server asking the user for input.
    Elicitation,
    /// One of Alondra's own cards.
    System(SystemCardKind),
}

/// Which of Alondra's own cards, without what each one carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemCardKind {
    NodePause,
    OverPace,
    PaceResume,
    Machine,
    LocalNetwork,
    McpWait,
    ClaudeSignedOut,
    ClaudeOutage,
    ClaudeUsage,
    HostLost,
    TurnEnded,
    Stalled,
    SessionEnded,
    HandoffDoubt,
    DecisionCancelled,
    Dropped,
    Unheard,
    EndedUnanswered,
    StartProblem,
    PostMergeProblem,
    CloseProblem,
    DiscardWorktrees,
    OpenPrDelivery,
    LaunchProblem,
    BatchProblem,
    StuckProblem,
    RequestDone,
    RequestTasks,
    Inbox,
    RefusedCards,
    Unknown,
}

impl SystemCardKind {
    pub const ALL: [SystemCardKind; 31] = {
        use SystemCardKind::*;
        [NodePause, OverPace, PaceResume, Machine, LocalNetwork, McpWait, ClaudeSignedOut, ClaudeOutage, ClaudeUsage,
            HostLost, TurnEnded, Stalled, SessionEnded, HandoffDoubt, DecisionCancelled, Dropped, Unheard, EndedUnanswered,
            StartProblem, PostMergeProblem, CloseProblem, DiscardWorktrees, OpenPrDelivery, LaunchProblem, BatchProblem,
            StuckProblem, RequestDone, RequestTasks, Inbox, RefusedCards, Unknown]
    };
}

impl SystemCard {
    pub fn kind(&self) -> SystemCardKind {
        use SystemCardKind as K;
        match self {
            Self::NodePause { .. } => K::NodePause,
            Self::OverPace { .. } => K::OverPace,
            Self::PaceResume { .. } => K::PaceResume,
            Self::Machine => K::Machine,
            Self::LocalNetwork => K::LocalNetwork,
            Self::McpWait => K::McpWait,
            Self::ClaudeSignedOut => K::ClaudeSignedOut,
            Self::ClaudeOutage => K::ClaudeOutage,
            Self::ClaudeUsage { .. } => K::ClaudeUsage,
            Self::HostLost => K::HostLost,
            Self::TurnEnded => K::TurnEnded,
            Self::Stalled => K::Stalled,
            Self::SessionEnded => K::SessionEnded,
            Self::HandoffDoubt => K::HandoffDoubt,
            Self::DecisionCancelled => K::DecisionCancelled,
            Self::Dropped => K::Dropped,
            Self::Unheard => K::Unheard,
            Self::EndedUnanswered { .. } => K::EndedUnanswered,
            Self::StartProblem => K::StartProblem,
            Self::PostMergeProblem => K::PostMergeProblem,
            Self::CloseProblem => K::CloseProblem,
            Self::DiscardWorktrees => K::DiscardWorktrees,
            Self::OpenPrDelivery => K::OpenPrDelivery,
            Self::LaunchProblem { .. } => K::LaunchProblem,
            Self::BatchProblem { .. } => K::BatchProblem,
            Self::StuckProblem => K::StuckProblem,
            Self::RequestDone => K::RequestDone,
            Self::RequestTasks => K::RequestTasks,
            Self::Inbox { .. } => K::Inbox,
            Self::RefusedCards { .. } => K::RefusedCards,
            Self::Unknown => K::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Creator {
    Node,
    Orchestrator,
    /// A Claude Code hook of a node, held until the user answers.
    Hook,
    Alondra,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reader {
    User,
    Orchestrator,
    Node,
}

/// What the reader can do with the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    /// At least one action that changes something: an option, accepting the result. Opening a
    /// link is never one.
    Decides,
    /// Only read: it never blocks and never counts as waiting for the user.
    Reads,
    /// Decided by its payload: a question decides, a notice is read.
    Either,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardField {
    Title,
    Why,
    Content,
    Verify,
}

/// What the card is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reference {
    Task,
    Request,
    /// The pull requests the delivery targets, every one registered on its task.
    PullRequests,
    McpServer,
    /// The question it reports on.
    Question,
}

/// Who writes the line that says what the reader does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    Author,
    /// Alondra writes it from the type, so an author can never ask in it for something the card's
    /// actions do not do. The English source; clients show it in their language.
    Alondra(&'static str),
}

/// When the card leaves its reader's queue without being answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retirement {
    Answered,
    Read,
    Accepted,
    HandedBack,
    Withdrawn,
    Replaced,
    TaskEnded,
    PrMerged,
    PrClosed,
    /// What it reported on was fixed.
    Resolved,
    /// The paused node went on.
    Resumed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contract {
    pub creators: &'static [Creator],
    pub readers: &'static [Reader],
    pub kinds: &'static [CardKind],
    pub response: Response,
    /// Its reader may answer with text alone: what they provide, or what they did.
    pub answers_in_text: bool,
    pub requires: &'static [CardField],
    pub refers_to: Reference,
    pub need: Need,
    pub retires: &'static [Retirement],
}

pub const RESULT_NEED: &str = "Review the result";
pub const PLAN_NEED: &str = "Review the plan";
pub const PROGRESS_NEED: &str = "Read this update";

/// The rule of a contract a card broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractRule {
    /// No card type is made by this command with this kind.
    Type,
    Creator,
    Reader,
    Kind,
    /// It asks the reader for something but offers no action that changes anything.
    NoDecision,
    /// A card that is only read cannot block.
    ReadOnly,
    Required,
    /// The field breaks its own shape: length, lines, an option it names.
    Shape,
    PrNotOpen,
    CitedPrNotTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractBreach {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_type: Option<CardType>,
    pub rule: ContractRule,
    pub field: String,
}

impl CardType {
    /// Every type, for the catalog and its tests.
    pub fn all() -> Vec<CardType> {
        use CardType::*;
        let mut all = vec![Question, Action, Input, Trial, Suggestion, Notice, Problem, Result, ProposedPlan, Progress,
            Permission, HeldPlan, HeldQuestion, Elicitation];
        all.extend(SystemCardKind::ALL.map(System));
        all
    }

    /// The type of a card, from what carries it: its system kind, whether a hook holds it, its
    /// payload and its kind. None when no type is made of this combination.
    pub fn of(card: &CardSpec, system: Option<&SystemCard>, payload: &InteractionPayload, held: bool) -> Option<Self> {
        if let Some(system) = system {
            return Some(Self::System(system.kind()));
        }
        if held {
            return match card.kind {
                CardKind::Authorize => Some(Self::Permission),
                CardKind::Review => Some(Self::HeldPlan),
                CardKind::Decide => Some(Self::HeldQuestion),
                CardKind::Provide => Some(Self::Elicitation),
                _ => None,
            };
        }
        match payload {
            InteractionPayload::Delivery { is_final: true, .. } if card.plan.is_some() => Some(Self::ProposedPlan),
            InteractionPayload::Delivery { is_final: true, .. } => Some(Self::Result),
            InteractionPayload::Delivery { is_final: false, .. } => Some(Self::Progress),
            InteractionPayload::Information { .. } => (card.kind == CardKind::Know).then_some(Self::Notice),
            InteractionPayload::Question { .. } | InteractionPayload::ActionRequest { .. } => match card.kind {
                CardKind::Decide => Some(Self::Question),
                CardKind::Do => Some(Self::Action),
                CardKind::Provide => Some(Self::Input),
                CardKind::Test => Some(Self::Trial),
                CardKind::Propose => Some(Self::Suggestion),
                CardKind::Problem => Some(Self::Problem),
                _ => None,
            },
        }
    }

    /// The name the catalog and a refusal give the type.
    pub fn name(self) -> String {
        let quoted = match self {
            Self::System(kind) => return format!("system:{}", serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()),
            other => serde_json::to_value(other),
        };
        quoted.ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
    }

    pub fn contract(self) -> Contract {
        use Response::*;
        use CardField::*;
        use CardKind as K;
        use Creator::*;
        use Reader as R;
        use Reference::*;
        use Retirement::*;
        const ASKED: &[Retirement] = &[Answered, Withdrawn, Replaced, TaskEnded];
        const DELIVERED: &[Retirement] = &[Accepted, HandedBack, Replaced, TaskEnded, PrMerged, PrClosed];
        let c = |creators, readers, kinds, response, requires, refers_to, retires| Contract {
            creators, readers, kinds, response, answers_in_text: false, requires, refers_to, need: Need::Author, retires,
        };
        let alondra = |readers, kinds, response, refers_to, retires| c(&[Alondra], readers, kinds, response, &[], refers_to, retires);
        let to_user = &[R::User];
        let anyone = &[Node, Orchestrator, Alondra];
        let everyone = &[R::User, R::Orchestrator, R::Node];
        match self {
            Self::Question => c(anyone, everyone, &[K::Decide], Decides, &[Title], Task, ASKED),
            Self::Action => Contract { answers_in_text: true, ..c(anyone, everyone, &[K::Do], Decides, &[Title, Why, Verify], Task, ASKED) },
            Self::Input => Contract { answers_in_text: true, ..c(anyone, everyone, &[K::Provide], Decides, &[Title, Why], Task, ASKED) },
            Self::Trial => c(anyone, everyone, &[K::Test], Decides, &[Title], Task, ASKED),
            Self::Suggestion => c(&[Node], &[R::Orchestrator], &[K::Propose], Decides, &[Title, Why], Task, ASKED),
            Self::Notice => c(&[Node, Orchestrator, Alondra], &[R::User, R::Orchestrator, R::Node], &[K::Know], Reads, &[Title], Request, &[Read, Withdrawn, Replaced]),
            Self::Problem => c(&[Orchestrator, Alondra], to_user, &[K::Problem], Decides, &[Title, Why], Task, &[Answered, Withdrawn, Replaced, Resolved, TaskEnded]),
            Self::Result => Contract { need: Need::Alondra(RESULT_NEED), ..c(&[Node], to_user, &[K::Review, K::Know], Decides, &[Title, Content], PullRequests, DELIVERED) },
            Self::ProposedPlan => Contract { need: Need::Alondra(PLAN_NEED), ..c(&[Node], to_user, &[K::Review], Decides, &[Title, Content], Task, &[Accepted, HandedBack, Replaced, TaskEnded]) },
            Self::Progress => Contract { need: Need::Alondra(PROGRESS_NEED), ..c(&[Node], to_user, &[K::Know, K::Review], Reads, &[Title], Task, &[Read, Replaced, TaskEnded]) },
            Self::Permission => c(&[Hook], to_user, &[K::Authorize], Decides, &[], Task, ASKED),
            Self::HeldPlan => c(&[Hook], to_user, &[K::Review], Decides, &[], Task, ASKED),
            Self::HeldQuestion => c(&[Hook], to_user, &[K::Decide, K::Provide], Decides, &[], Task, ASKED),
            Self::Elicitation => Contract { answers_in_text: true, ..c(&[Hook], to_user, &[K::Provide], Decides, &[], McpServer, ASKED) },
            Self::System(kind) => {
                use SystemCardKind as S;
                match kind {
                    S::NodePause => alondra(to_user, &[K::Decide], Decides, Task, &[Answered, Resumed, TaskEnded]),
                    S::OverPace | S::PaceResume => alondra(to_user, &[K::Decide], Decides, Task, &[Answered, Withdrawn, Resumed]),
                    S::StartProblem => alondra(&[R::User, R::Orchestrator], &[K::Problem], Decides, Task, &[Answered, Withdrawn, Replaced, Resolved, TaskEnded]),
                    S::Machine | S::LocalNetwork | S::PostMergeProblem | S::CloseProblem
                    | S::DiscardWorktrees | S::LaunchProblem | S::BatchProblem | S::StuckProblem => {
                        alondra(to_user, &[K::Problem], Decides, Task, &[Answered, Withdrawn, Replaced, Resolved, TaskEnded])
                    }
                    S::McpWait => alondra(to_user, &[K::Problem], Decides, McpServer, &[Answered, Resolved, TaskEnded]),
                    S::ClaudeSignedOut | S::ClaudeOutage | S::ClaudeUsage => alondra(to_user, &[K::Problem], Decides, Task, &[Answered, Resolved, TaskEnded]),
                    S::HostLost => alondra(to_user, &[K::Problem], Decides, Task, ASKED),
                    S::TurnEnded | S::SessionEnded | S::HandoffDoubt | S::DecisionCancelled | S::Dropped => {
                        alondra(to_user, &[K::Decide, K::Problem], Decides, Task, ASKED)
                    }
                    S::Stalled => alondra(&[R::Orchestrator], &[K::Decide], Decides, Task, ASKED),
                    S::Unheard => c(&[Node, Alondra], to_user, &[K::Decide], Decides, &[], Task, ASKED),
                    S::EndedUnanswered => alondra(to_user, &[K::Know], Reads, Question, &[Read, Withdrawn]),
                    S::OpenPrDelivery => alondra(to_user, &[K::Review, K::Know], Decides, PullRequests, DELIVERED),
                    S::RequestDone | S::RequestTasks => alondra(to_user, &[K::Know], Reads, Request, &[Read, Withdrawn, Replaced]),
                    S::Inbox => alondra(&[R::Orchestrator, R::User], &[K::Know, K::Problem], Either, Request, &[Answered, Read, Withdrawn, Replaced]),
                    S::RefusedCards => alondra(&[R::Orchestrator], &[K::Know], Reads, Task, &[Read, Withdrawn]),
                    S::Unknown => c(&[], &[], &[], Reads, &[], Task, &[]),
                }
            }
        }
    }

    /// Whether a card of this type, once open, asks its reader for something: what lists it in
    /// Waiting for You and counts it on the app's icon. A card that is only read never does.
    pub fn asks(self, payload: &InteractionPayload) -> bool {
        match self.contract().response {
            Response::Reads => false,
            Response::Decides => true,
            Response::Either => !matches!(payload, InteractionPayload::Information { .. }),
        }
    }
}

fn creator(author: &Author, held: bool) -> Creator {
    match author {
        Author::Agent { .. } if held => Creator::Hook,
        Author::Agent { .. } => Creator::Node,
        Author::Orchestrator { .. } => Creator::Orchestrator,
        Author::System => Creator::Alondra,
    }
}

fn reader(to: &Recipient) -> Reader {
    match to {
        Recipient::User => Reader::User,
        Recipient::Orchestrator => Reader::Orchestrator,
        Recipient::Agent { .. } => Reader::Node,
    }
}

/// The card a command is about to create, as its contract sees it.
pub struct NewCard<'a> {
    pub card: &'a CardSpec,
    pub system: Option<&'a SystemCard>,
    pub payload: &'a InteractionPayload,
    pub author: &'a Author,
    pub to: &'a Recipient,
    pub blocking: bool,
    pub held: bool,
}

impl NewCard<'_> {
    pub fn card_type(&self) -> Option<CardType> {
        CardType::of(self.card, self.system, self.payload, self.held)
    }

    /// The card keeps its type's contract, or the breach says which rule and field it broke.
    pub fn check(&self) -> Result<CardType, ContractBreach> {
        let breach = |card_type, rule, field: &str| Err(ContractBreach { card_type, rule, field: field.into() });
        let Some(card_type) = self.card_type() else {
            return breach(None, ContractRule::Type, "card.kind");
        };
        let at = Some(card_type);
        let contract = card_type.contract();
        if !contract.creators.contains(&creator(self.author, self.held)) {
            return breach(at, ContractRule::Creator, "author");
        }
        if !contract.readers.contains(&reader(self.to)) {
            return breach(at, ContractRule::Reader, "recipient");
        }
        if !contract.kinds.contains(&self.card.kind) {
            return breach(at, ContractRule::Kind, "card.kind");
        }
        let card = self.card;
        for field in contract.requires {
            let (present, name) = match field {
                CardField::Title => (card.title.as_deref().is_some_and(|t| !t.trim().is_empty()), "card.title"),
                CardField::Why => (!card.why.trim().is_empty(), "card.why"),
                CardField::Content => (card.content.is_some(), "card.content"),
                CardField::Verify => (card.verify.is_some(), "card.verify"),
            };
            if !present {
                return breach(at, ContractRule::Required, name);
            }
        }
        let options = match self.payload {
            InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => Some(options.len()),
            _ => None,
        };
        let read_only = matches!(self.payload, InteractionPayload::Information { .. } | InteractionPayload::Delivery { is_final: false, .. });
        match contract.response {
            Response::Decides if read_only => return breach(at, ContractRule::NoDecision, "kind"),
            Response::Decides if options == Some(0) && !contract.answers_in_text && !self.resolves_itself() => {
                return breach(at, ContractRule::NoDecision, "options");
            }
            Response::Reads if !read_only => return breach(at, ContractRule::ReadOnly, "kind"),
            Response::Reads | Response::Either if read_only && self.blocking => return breach(at, ContractRule::ReadOnly, "blocking"),
            _ => {}
        }
        Ok(card_type)
    }

    /// Alondra's own Problem that asks for something done outside it and withdraws itself once
    /// it sees it done: its card has no option to press.
    fn resolves_itself(&self) -> bool {
        *self.author == Author::System && self.card.kind == CardKind::Problem
    }
}

impl ContractBreach {
    /// What the author does to keep the contract.
    pub fn fix(&self) -> String {
        let named = self.card_type.map_or_else(|| "this card".to_owned(), |t| format!("a {} card", t.name()));
        match self.rule {
            ContractRule::Type => "no card is made of this command with this kind: use the tool for what you need (ask, request_action, request_input, request_test, present_result)".into(),
            ContractRule::Creator => format!("{named} is not yours to send: ask the orchestrator, or use the tool meant for you"),
            ContractRule::Reader => format!("{named} goes to someone else: a suggestion and a stuck node go to the orchestrator, a result to the user"),
            ContractRule::Kind => format!("{named} does not take this kind"),
            ContractRule::NoDecision => format!("{named} asks for something, so it offers at least one option that changes something; opening a link is not one"),
            ContractRule::ReadOnly => format!("{named} is only read: it never blocks and carries no options"),
            ContractRule::Required => format!("{named} carries this field"),
            ContractRule::Shape => format!("{named} breaks the shape of this field"),
            ContractRule::PrNotOpen => format!("{named} cannot deliver a closed pull request: remove the closed PR target"),
            ContractRule::CitedPrNotTarget => format!("{named} cites a pull request it does not deliver: include that registered PR in targets, or remove the citation"),
        }
    }
}

fn words<T: Serialize>(values: &[T]) -> String {
    let named: Vec<String> = values.iter()
        .filter_map(|value| serde_json::to_value(value).ok())
        .map(|value| match value {
            serde_json::Value::String(name) => name,
            serde_json::Value::Object(map) => map.into_iter().map(|(key, value)| format!("{key} {}", value.as_str().unwrap_or_default())).collect(),
            other => other.to_string(),
        })
        .collect();
    if named.is_empty() { "—".into() } else { named.join(", ") }
}

/// The catalog as a Markdown table, one row per type: what `docs/architecture/cards.md` shows.
pub fn catalog_table() -> String {
    let mut table = String::from("| Type | Created by | Read by | Kind | Reader can | Carries | About | Need | Goes away when |\n\
        |---|---|---|---|---|---|---|---|---|\n");
    for card_type in CardType::all() {
        let c = card_type.contract();
        let need = match c.need {
            Need::Author => "its author".to_owned(),
            Need::Alondra(need) => format!("Alondra: «{need}»"),
        };
        let response = if c.answers_in_text { format!("{}, or write", words(&[c.response])) } else { words(&[c.response]) };
        table.push_str(&format!("| `{}` | {} | {} | {} | {response} | {} | {} | {need} | {} |\n",
            card_type.name(), words(c.creators), words(c.readers), words(c.kinds),
            words(c.requires), words(&[c.refers_to]), words(c.retires)));
    }
    table
}
