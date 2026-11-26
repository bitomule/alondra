//! The validation of a task with specs: the implementer says its work is ready, a new agent in the
//! validator's role runs every check the spec engine still lists for it and records what each
//! found, and only then does the work reach the user. Alondra counts only what that validator
//! recorded, each at the fingerprint Alondra itself read of the commit it validates.
use crate::{InteractionId, Revision};
use serde::{Deserialize, Serialize};

/// How many rounds in a row the validator may send the work back before the user decides.
pub const MAX_VALIDATION_ROUNDS: u8 = 3;
/// The most validations a task keeps; the oldest go first.
pub const MAX_VALIDATION_RECORDS: usize = 512;
/// The most items one round validates.
pub const MAX_ROUND_ITEMS: usize = 512;
/// The most rounds a task keeps; the oldest go first.
pub const MAX_KEPT_ROUNDS: usize = 16;
/// The most deliveries whose validation a task keeps for their cards.
pub const MAX_DELIVERED: usize = 8;
/// The longest text of a verdict's reasoning, the nearest wrong behaviour or a round's findings.
pub const MAX_VALIDATION_TEXT: usize = 4000;
/// The longest output of a run Alondra keeps: its end.
pub const MAX_VALIDATION_OUTPUT: usize = 8000;
/// The most files a round lists outside the task's components.
pub const MAX_SCOPE_FILES: usize = 200;

/// What a validation checks, as the spec engine names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationKind {
    /// A suite of a component ran and passed: `<component>/<suite>`.
    Suite,
    /// The tests that declare a scenario check it: `<spec>/<scenario>`, a judgment.
    Adequate,
    /// A scenario marked as a judgment holds, on its evidence: a judgment.
    Judged,
    /// A section of reference prose is still true: a judgment.
    Reference,
}

impl ValidationKind {
    pub const ALL: [ValidationKind; 4] = [Self::Suite, Self::Adequate, Self::Judged, Self::Reference];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Suite => "suite",
            Self::Adequate => "adequate",
            Self::Judged => "judged",
            Self::Reference => "reference",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == word)
    }

    /// A judgment is someone's verdict, never a fact a run showed.
    pub fn judgment(self) -> bool {
        self != Self::Suite
    }

    pub fn engine(self) -> crate::specs::engine::Kind {
        use crate::specs::engine::Kind;
        match self {
            Self::Suite => Kind::Suite,
            Self::Adequate => Kind::Adequate,
            Self::Judged => Kind::Judged,
            Self::Reference => Kind::Reference,
        }
    }

    pub fn of_engine(kind: crate::specs::engine::Kind) -> Option<Self> {
        use crate::specs::engine::Kind;
        match kind {
            Kind::Suite => Some(Self::Suite),
            Kind::Adequate => Some(Self::Adequate),
            Kind::Judged => Some(Self::Judged),
            Kind::Reference => Some(Self::Reference),
            Kind::Passes => None,
        }
    }
}

/// What the validator found: the check holds, it does not, or it could not be run at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationVerdict {
    Met,
    Unmet,
    /// It could not run (a tool missing, its time ran out): never a failure, never valid.
    Incomplete,
}

impl ValidationVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Met => "met",
            Self::Unmet => "unmet",
            Self::Incomplete => "incomplete",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        [Self::Met, Self::Unmet, Self::Incomplete].into_iter().find(|verdict| verdict.as_str() == word)
    }
}

/// Where one validation of a round stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationState {
    /// The validator owes it.
    Running,
    Met,
    Unmet,
    /// It could not run, or its validator ended before recording it: it runs again.
    Incomplete,
}

impl ValidationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Met => "met",
            Self::Unmet => "unmet",
            Self::Incomplete => "incomplete",
        }
    }
}

/// One thing the spec engine lists as not validated on the commit a round validates, at the
/// fingerprint Alondra read of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationItem {
    pub subject: String,
    pub kind: ValidationKind,
    pub inputs: String,
}

/// One validation of a round and where it stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundItem {
    pub subject: String,
    pub kind: ValidationKind,
    pub inputs: String,
    pub state: ValidationState,
    /// Why it is incomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What the validator recorded of one validation, at the fingerprint Alondra read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationRecord {
    pub subject: String,
    pub kind: ValidationKind,
    pub inputs: String,
    pub verdict: ValidationVerdict,
    pub round: u32,
    /// The execution of the validator that recorded it.
    pub generation: u64,
    /// A judge's reasoning, or why it is incomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// For a test that checks its scenario: the nearest wrong behaviour it would still accept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest_wrong: Option<String>,
    /// The end of what a run printed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Attachment ids of the captures or videos a judgment looked at.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    pub revision: Revision,
}

/// Where a round of validation stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundState {
    /// The implementer said its work is ready: a validator takes the task over.
    Requested,
    /// A validator works on it.
    Validating,
    /// Its validator ended before it finished: the next validator carries on with what is left.
    Interrupted,
    /// The validator sent the work back with its findings.
    Failed,
    /// The validator delivered it.
    Delivered,
}

impl RoundState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Validating => "validating",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::Delivered => "delivered",
        }
    }

    pub fn open(self) -> bool {
        matches!(self, Self::Requested | Self::Validating | Self::Interrupted)
    }
}

/// One round: the commit the implementer said is ready, what the engine listed for it, and what
/// the validator made of each.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationRound {
    pub number: u32,
    /// The commit it validates: the implementer's, or one the PR received after the delivery.
    pub head: String,
    /// The commit the implementer handed over; between it and the delivered commit only the ledger
    /// may change, whoever made the commits in between.
    pub implemented: String,
    pub items: Vec<RoundItem>,
    /// Files the branch changed outside the components of the task's specs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    pub state: RoundState,
    /// The execution of the validator working on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    /// Why the validator sent the work back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings: Option<String>,
    /// A validator already ended before finishing it once: the next end is the stuck task's path.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retried: bool,
}

impl ValidationRound {
    pub fn item_mut(&mut self, subject: &str, kind: ValidationKind) -> Option<&mut RoundItem> {
        self.items.iter_mut().find(|item| item.subject == subject && item.kind == kind)
    }

    /// Every validation the round owes and its validator has not recorded.
    pub fn owed(&self) -> impl Iterator<Item = &RoundItem> {
        self.items.iter().filter(|item| matches!(item.state, ValidationState::Running | ValidationState::Incomplete))
    }
}

/// One test that declares a scenario, as the delivery shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaringTest {
    pub path: String,
    pub line: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The suites whose `tests` hold it, `<component>/<suite>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suites: Vec<String>,
}

/// One scenario as the delivery shows it: its text, the engine's state of each validation and the
/// tests that declare it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredScenario {
    /// `<spec>/<scenario>`.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    /// The engine's state of each of its validations on the delivered commit: `passes`,
    /// `adequate`, `judged`, each as the engine words it (`valid`, `stale`, `uncovered`…).
    pub states: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<DeclaringTest>,
    /// How it is judged when it is a judgment: `judged`, `capture` or `video`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged: Option<String>,
    /// What the validator said of each of its judgments on the delivered commit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verdicts: Vec<DeliveredVerdict>,
}

/// A judgment the validator recorded, as the delivery shows it: always labelled a judgment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredVerdict {
    pub kind: ValidationKind,
    pub verdict: ValidationVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearest_wrong: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

/// A suite of a component and how it ran on the delivered commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredSuite {
    /// `<component>/<suite>`.
    pub id: String,
    /// The engine's state of it on the delivered commit.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ValidationVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// One spec the delivered work touches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredSpec {
    pub id: String,
    pub path: String,
    pub title: String,
    pub scenarios: Vec<DeliveredScenario>,
}

/// One section of reference prose the branch changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredSection {
    pub id: String,
    pub path: String,
    pub text: String,
}

/// What a validator's delivery showed of its specs, read by Alondra from the delivered commit when
/// the delivery passed the gate. A card shows it as it was then.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredValidation {
    pub delivery: InteractionId,
    pub head: String,
    pub specs: Vec<DeliveredSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suites: Vec<DeliveredSuite>,
    /// Files the branch changed outside the components of the task's specs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outside: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<DeliveredSection>,
}

/// Everything a task knows of its validation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskValidation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rounds: Vec<ValidationRound>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<ValidationRecord>,
    /// Rounds the validator sent back since the user last decided how the task goes on.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failed: u8,
    /// The card that asks the user how the task goes on after the last round failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhausted: Option<InteractionId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivered: Vec<DeliveredValidation>,
    /// What Alondra read of the commits the task delivers or is about to deliver.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readings: Vec<crate::delivery_validation::CommitReading>,
}

fn is_zero(value: &u8) -> bool {
    *value == 0
}

impl TaskValidation {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn current(&self) -> Option<&ValidationRound> {
        self.rounds.last()
    }

    pub fn current_mut(&mut self) -> Option<&mut ValidationRound> {
        self.rounds.last_mut()
    }

    /// The round a validator works on now, if one is open.
    pub fn open_round(&self) -> Option<&ValidationRound> {
        self.current().filter(|round| round.state.open())
    }

    /// What the validator recorded for exactly this subject, kind and fingerprint.
    pub fn record(&self, subject: &str, kind: ValidationKind, inputs: &str) -> Option<&ValidationRecord> {
        self.records.iter().rev().find(|record| record.subject == subject && record.kind == kind && record.inputs == inputs)
    }

    pub fn delivered(&self, delivery: &InteractionId) -> Option<&DeliveredValidation> {
        self.delivered.iter().find(|delivered| delivered.delivery == *delivery)
    }

    pub fn push_round(&mut self, round: ValidationRound) {
        self.rounds.push(round);
        let over = self.rounds.len().saturating_sub(MAX_KEPT_ROUNDS);
        self.rounds.drain(..over);
    }

    pub fn push_record(&mut self, record: ValidationRecord) {
        self.records.retain(|old| !(old.subject == record.subject && old.kind == record.kind && old.inputs == record.inputs));
        self.records.push(record);
        let over = self.records.len().saturating_sub(MAX_VALIDATION_RECORDS);
        self.records.drain(..over);
    }

    pub fn push_delivered(&mut self, delivered: DeliveredValidation) {
        self.delivered.retain(|old| old.delivery != delivered.delivery);
        self.delivered.push(delivered);
        let over = self.delivered.len().saturating_sub(MAX_DELIVERED);
        self.delivered.drain(..over);
    }

    /// The validator ended before it finished its round: what it owed is incomplete, what it
    /// recorded stays.
    pub fn interrupted(&mut self, reason: &str) -> bool {
        let Some(round) = self.current_mut().filter(|round| round.state == RoundState::Validating) else { return false };
        round.state = RoundState::Interrupted;
        for item in round.items.iter_mut().filter(|item| item.state == ValidationState::Running) {
            item.state = ValidationState::Incomplete;
            item.reason = Some(reason.to_owned());
        }
        true
    }
}

/// The end of `text`, at most `limit` characters.
pub fn tail(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    text.chars().skip(count - limit).collect()
}

impl crate::DomainState {
    /// A new execution of the task started: a validator takes the round that waits for one, and
    /// what an earlier validator could not finish is owed again.
    pub fn validation_admitted(&mut self, task_id: &crate::TaskId, generation: u64) {
        let Some(task) = self.tasks.get_mut(task_id) else { return };
        if task.stages.role() != Some(crate::ExecutionRole::Validator) {
            return;
        }
        let Some(round) = task.validation.current_mut().filter(|round| matches!(round.state, RoundState::Requested | RoundState::Interrupted)) else { return };
        round.state = RoundState::Validating;
        round.generation = Some(generation);
        for item in round.items.iter_mut().filter(|item| item.state == ValidationState::Incomplete) {
            item.state = ValidationState::Running;
            item.reason = None;
        }
    }

    /// The validator's host came back: it carries on with what it still owes.
    pub fn validator_retaken(&mut self, task_id: &crate::TaskId, generation: u64) {
        self.validation_admitted(task_id, generation);
    }

    /// The validator's host was lost: what it recorded stays, what it owed is incomplete, and the
    /// task follows the path of any lost host.
    pub fn validator_lost(&mut self, task_id: &crate::TaskId) {
        let Some(task) = self.tasks.get_mut(task_id) else { return };
        if task.stages.role() == Some(crate::ExecutionRole::Validator) {
            task.validation.interrupted("validator_lost");
        }
    }
}
