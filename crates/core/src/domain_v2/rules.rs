use super::*;
use std::collections::{BTreeMap, BTreeSet};
type Result<T> = std::result::Result<T, DomainError>;
fn fail<T>(code: &str, msg: &str) -> Result<T> {
    Err(DomainError::new(code, msg))
}
/// A rejection a node can act on: what is wrong, in which field, and how to fix it.
fn refuse<T>(code: &str, field: &str, fix: &str) -> Result<T> {
    let mut error = DomainError::new(code, format!("{field}: {fix}"));
    error.subject = Some(field.into());
    Err(error)
}
/// The command a card arrives with, which bounds the kinds it may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Information,
    Question,
    Action,
    Delivery,
    Hold,
    Problem,
}
impl Source {
    fn kinds(self) -> &'static [CardKind] {
        use CardKind::*;
        match self {
            Source::Information => &[Know],
            Source::Question => &[Decide, Provide, Test, Propose],
            Source::Action => &[Do, Test, Provide],
            Source::Delivery => &[Review, Know],
            Source::Hold => &[Authorize, Review, Decide, Provide],
            Source::Problem => &[Problem],
        }
    }
    fn command(self) -> &'static str {
        match self {
            Source::Information => "publish_information",
            Source::Question => "ask",
            Source::Action => "request_action",
            Source::Delivery => "present_result",
            Source::Hold => "a held hook",
            Source::Problem => "raise_problem",
        }
    }
}
/// The option of a Propose card that turns it into a Request.
pub const REQUEST_TITLE_LIMIT: usize = 60;
pub const PROPOSAL_ACCEPTED: &str = "do_it";
fn kind_name(kind: CardKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}
/// R18–R21: the card's own shape, before any state is looked at.
fn validate_card(card: &CardSpec, options: &[QuestionOption], source: Source, blocking: bool) -> Result<()> {
    let kind = kind_name(card.kind);
    if card.kind == CardKind::Problem && source != Source::Problem {
        return refuse("forbidden", "card.kind", "Only Alondra and the orchestrator raise a problem: describe it with ask or publish_information instead");
    }
    if !source.kinds().contains(&card.kind) {
        let allowed: Vec<String> = source.kinds().iter().map(|k| kind_name(*k)).collect();
        return refuse("invalid_input", "card.kind", &format!("{} does not send a {kind} card: use one of {}", source.command(), allowed.join(", ")));
    }
    let need = card.need.trim();
    if need.is_empty() || need.contains('\n') {
        return refuse("invalid_input", "card.need", "Say in one line, in the imperative, what you need from the user");
    }
    if need.chars().count() > CardSpec::NEED_MAX {
        return refuse("invalid_input", "card.need", &format!("At most {} characters: keep what you need short and move the rest to card.why or card.content", CardSpec::NEED_MAX));
    }
    if card.why.chars().count() > CardSpec::WHY_MAX {
        return refuse("invalid_input", "card.why", &format!("At most {} characters: long text goes in card.content.body, never in why", CardSpec::WHY_MAX));
    }
    if card.kind.needs_why() && source != Source::Hold && card.why.trim().is_empty() {
        return refuse("invalid_input", "card.why", &format!("A {kind} card says why: add one or two sentences, and whom it holds back"));
    }
    if card.kind == CardKind::Review && card.content.is_none() {
        return refuse("invalid_input", "card.content", "A review card carries what to review: add card.content with its kind and body");
    }
    if let Some(summary) = card.content.as_ref().and_then(|c| c.summary.as_ref())
        && summary.chars().count() > CardSpec::SUMMARY_MAX
    {
        return refuse("invalid_input", "card.content.summary", &format!("At most {} characters, like \"6 files · +120 −40\"; leave it out and Alondra computes one", CardSpec::SUMMARY_MAX));
    }
    match (&card.verify, card.kind) {
        (None, CardKind::Do) => return refuse("invalid_input", "card.verify", "A do card says how you will check the user did it"),
        (Some(_), k) if k != CardKind::Do => return refuse("invalid_input", "card.verify", "Only a do card carries verify: remove it"),
        (Some(v), _) if v.trim().is_empty() => return refuse("invalid_input", "card.verify", "Say how you will check the user did it"),
        _ => {}
    }
    if let Some(recommended) = &card.recommended
        && !options.iter().any(|o| &o.id == recommended)
    {
        return refuse("invalid_input", "card.recommended", "Recommend the id of one of the card's options, or leave it out");
    }
    if card.kind == CardKind::Decide && source != Source::Hold {
        decide_options(options)?;
    }
    if blocking && card.kind.never_blocks() {
        return refuse("invalid_input", "blocking", &format!("A {kind} card never blocks: send it with blocking false"));
    }
    if card.blocks.len() > 16 {
        return refuse("invalid_input", "card.blocks", "At most 16 targets");
    }
    Ok(())
}
/// R19: a decision offers 2–8 short distinct options, at most one of them recommended.
fn decide_options(options: &[QuestionOption]) -> Result<()> {
    if !(2..=8).contains(&options.len()) {
        return refuse("invalid_input", "options", "A decision offers 2 to 8 options: merge or drop some, or ask for text with a provide card");
    }
    let mut labels = BTreeSet::new();
    for option in options {
        if option.label.chars().count() > 40 {
            return refuse("invalid_input", "options.label", &format!("\"{}\" is longer than 40 characters: shorten it and explain it in its description", option.label));
        }
        if !labels.insert(option.label.trim().to_lowercase()) {
            return refuse("invalid_input", "options.label", &format!("\"{}\" appears twice: every option says something different", option.label));
        }
    }
    if options.iter().filter(|o| o.label.trim_end().ends_with("(Recommended)")).count() > 1 {
        return refuse("invalid_input", "card.recommended", "Recommend at most one option, with card.recommended");
    }
    Ok(())
}
fn nonempty(s: &str) -> Result<()> {
    if s.trim().is_empty() || s.len() > 1_048_576 {
        fail("invalid_input", "Empty or oversized value")
    } else {
        Ok(())
    }
}
fn id(s: &str) -> Result<()> {
    nonempty(s)?;
    if s.len() > 256 || s.chars().any(char::is_control) {
        fail("invalid_input", "Invalid identifier")
    } else {
        Ok(())
    }
}
fn digest(s: &str) -> Result<()> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        fail("invalid_input", "Content digest must be lowercase SHA-256")
    } else {
        Ok(())
    }
}
fn head(s: &str) -> Result<()> {
    nonempty(s)?;
    if s.len() > 256 || s.chars().any(char::is_whitespace) {
        fail("invalid_input", "Invalid head")
    } else {
        Ok(())
    }
}
fn user(actor: &Actor) -> Result<String> {
    match actor {
        Actor::User { user_id } => {
            id(user_id)?;
            Ok(user_id.clone())
        }
        _ => fail("forbidden", "User decision required"),
    }
}
fn system(actor: &Actor) -> Result<()> {
    if matches!(actor, Actor::System) {
        Ok(())
    } else {
        fail("forbidden", "Trusted observation required")
    }
}
impl DomainState {
    fn fence(&self, actor: &Actor) -> Result<()> {
        match actor {
            Actor::Orchestrator { generation } if *generation != self.orchestrator_generation => {
                fail("stale_generation", "Orchestrator generation changed")
            }
            Actor::User { user_id } => id(user_id),
            Actor::Agent { agent_id } => id(&agent_id.0),
            _ => Ok(()),
        }
    }
    fn manager(&self, actor: &Actor) -> Result<()> {
        self.fence(actor)?;
        if matches!(actor, Actor::User { .. } | Actor::Orchestrator { .. }) {
            Ok(())
        } else {
            fail("forbidden", "User or current orchestrator required")
        }
    }
    fn task(&self, task: &TaskId) -> Result<&Task> {
        self.tasks
            .get(task)
            .ok_or_else(|| DomainError::new("not_found", "Task does not exist"))
    }
    fn active(&self, task: &TaskId) -> Result<&Task> {
        let t = self.task(task)?;
        if t.state.terminal() {
            fail("task_not_active", "Task is terminal")
        } else {
            Ok(t)
        }
    }
    /// An Agent works on one Task at a time: the other non-terminal Task it holds, if any.
    pub fn busy_with(&self, agent: &AgentId, except: &TaskId) -> Option<&Task> {
        self.tasks.values().find(|t| &t.id != except && t.agent_id.as_ref() == Some(agent) && !t.state.terminal())
    }
    fn free(&self, agent: &AgentId, task: &TaskId) -> Result<()> {
        match self.busy_with(agent, task) {
            Some(other) => fail("agent_busy", &format!("Agent {} already works on Task {}: launch a new node for this Task", agent.0, other.id.0)),
            None => Ok(()),
        }
    }
    fn assigned(&self, actor: &Actor, task: &TaskId) -> Result<&Task> {
        let t = self.active(task)?;
        match actor {
            Actor::Agent { agent_id } if t.agent_id.as_ref() == Some(agent_id) => Ok(t),
            _ => fail("not_assigned", "Command requires the assigned Agent"),
        }
    }
    fn task_editor(&self, actor: &Actor, task: &TaskId) -> Result<()> {
        self.active(task)?;
        if matches!(actor, Actor::Agent { .. }) {
            self.assigned(actor, task)?;
        } else {
            self.manager(actor)?;
        }
        Ok(())
    }
    fn validate_scope(&self, scope: &Scope) -> Result<()> {
        match scope {
            Scope::Task { task_id } => {
                self.task(task_id)?;
            }
            Scope::Orchestrator {
                request_id: Some(r),
            } => {
                if !self.requests.contains_key(r) {
                    return fail("not_found", "Request does not exist");
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn recipient(&self, to: &Recipient) -> Result<()> {
        if let Recipient::Agent { agent_id } = to {
            if !self.agents.contains_key(agent_id) {
                return fail("not_found", "Recipient Agent does not exist");
            }
        }
        Ok(())
    }
    fn interaction(&self, i: &InteractionId) -> Result<&Interaction> {
        self.interactions
            .get(i)
            .ok_or_else(|| DomainError::new("not_found", "Interaction does not exist"))
    }
    fn presentation(
        &self,
        i: &InteractionId,
    ) -> Result<(&Interaction, &Task, &Vec<DeliveryTarget>)> {
        let p = self.interaction(i)?;
        let Scope::Task { task_id } = &p.scope else {
            return fail("invalid_action_for_kind", "Delivery must have Task scope");
        };
        let t = self.active(task_id)?;
        let InteractionPayload::Delivery { targets, .. } = &p.payload else {
            return fail(
                "invalid_action_for_kind",
                "A delivery presentation is required",
            );
        };
        if t.final_presentation.as_ref() != Some(i) {
            return fail("acceptance_stale", "Presentation is not the current final");
        };
        self.targets(task_id, targets)?;
        Ok((p, t, targets))
    }
    fn targets(&self, task: &TaskId, targets: &[DeliveryTarget]) -> Result<()> {
        let t = self.task(task)?;
        if targets.is_empty() || targets.len() > 64 {
            return fail(
                "invalid_input",
                "Delivery requires a bounded nonempty result set",
            );
        };
        let mut seen = BTreeSet::new();
        for target in targets {
            let key = serde_json::to_string(target)
                .map_err(|_| DomainError::new("invalid_input", "Invalid target"))?;
            if !seen.insert(key) {
                return fail("invalid_input", "Duplicate delivery target");
            };
            match target {
                DeliveryTarget::Text { text } => nonempty(text)?,
                DeliveryTarget::DocumentRevision {
                    document_id,
                    revision_id,
                } => {
                    if !t.documents.contains_key(document_id) {
                        return fail("forbidden", "Document is not linked to this Task");
                    };
                    let d = self
                        .documents
                        .get(document_id)
                        .ok_or_else(|| DomainError::new("not_found", "Document missing"))?;
                    let r = self
                        .document_revisions
                        .get(revision_id)
                        .ok_or_else(|| DomainError::new("not_found", "Revision missing"))?;
                    if &r.document_id != document_id {
                        return fail("invalid_input", "Revision belongs to another document");
                    };
                    if &d.current_revision != revision_id {
                        return fail("acceptance_stale", "Document has a newer revision");
                    }
                }
                DeliveryTarget::PullRequest {
                    pr_id,
                    head: expected,
                } => {
                    let p = self
                        .pull_requests
                        .get(pr_id)
                        .ok_or_else(|| DomainError::new("not_found", "PR missing"))?;
                    if &p.task_id != task || t.pr_id.as_ref() != Some(pr_id) {
                        return fail("forbidden", "PR belongs to another Task");
                    };
                    if &p.head != expected {
                        return fail("stale_head", "PR head changed");
                    }
                }
            }
        }
        Ok(())
    }
    /// Puts a card in front of its recipient after every card rule holds. `Some(existing)` is a
    /// notice already waiting unread with the same words (R8): nothing new is created.
    #[allow(clippy::too_many_arguments)]
    fn insert_interaction(
        &mut self,
        actor: &Actor,
        i: &InteractionId,
        scope: &Scope,
        to: &Recipient,
        body: &str,
        blocking: bool,
        attachments: &[AttachmentRef],
        payload: InteractionPayload,
        card: &CardSpec,
        source: Source,
    ) -> Result<Option<InteractionId>> {
        id(&i.0)?;
        content(body, attachments)?;
        if self.interactions.contains_key(i) {
            return fail("invalid_input", "Interaction ID already exists");
        };
        let options = match &payload {
            InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => options.as_slice(),
            _ => &[],
        };
        validate_card(card, options, source, blocking)?;
        let author = match source {
            Source::Hold => Author::Agent { agent_id: match actor {
                Actor::Agent { agent_id } => agent_id.clone(),
                _ => return fail("forbidden", "Only an Agent's hook can be held"),
            } },
            _ => self.card_author(actor, scope, source)?,
        };
        self.recipient(to)?;
        if let Some(existing) = self.repeated_notice(&author, scope, to, body, card, source) {
            return Ok(Some(existing));
        }
        let task = match scope {
            Scope::Task { task_id } => Some(task_id.clone()),
            Scope::Orchestrator { .. } => None,
        };
        if source != Source::Hold {
            self.clarification_answered(&author, task.as_ref(), card)?;
            self.one_blocking_ask(&author, card, blocking)?;
        }
        self.card_blocks(task.as_ref(), card)?;
        if let Some(old) = &card.replaces {
            self.replaceable(&author, old)?;
            self.retire(old);
        }
        if card.content.as_ref().is_some_and(|c| c.kind == ContentKind::Plan) {
            for old in self.open_plans(&author) {
                self.retire(&old);
            }
        }
        self.interactions.insert(
            i.clone(),
            Interaction {
                attachments: attachments.to_vec(),
                id: i.clone(),
                scope: scope.clone(),
                author,
                recipient: to.clone(),
                body: body.into(),
                blocking,
                subject: None,
                payload,
                revision: self.revision + 1,
                order: self.revision + 1,
                withdrawn: false,
                card: card.clone(),
                clarification: None,
            },
        );
        self.blocks_acyclic(i)?;
        Ok(None)
    }
    /// R1, R2, R4: who may put a card in front of someone, and on which Task.
    fn card_author(&self, actor: &Actor, scope: &Scope, source: Source) -> Result<Author> {
        self.fence(actor)?;
        self.validate_scope(scope)?;
        let notice = source == Source::Information;
        match actor {
            Actor::Agent { agent_id } => {
                let holds = |t: &Task| t.agent_id.as_ref() == Some(agent_id);
                let working = self.tasks.values().any(|t| holds(t) && !t.state.terminal());
                let owned = match scope {
                    Scope::Task { task_id } => Some(self.task(task_id)?).filter(|t| holds(t)),
                    Scope::Orchestrator { .. } => None,
                };
                match owned {
                    Some(t) if t.state.terminal() && !notice => refuse(
                        "task_not_active",
                        "scope.task_id",
                        &format!("Task {} is {}: it accepts only a closing notice. Send publish_information, or ask the orchestrator for a new Task", t.id, if t.archived { "archived" } else if t.state == TaskState::Cancelled { "cancelled" } else { "done" }),
                    ),
                    Some(_) => Ok(Author::Agent { agent_id: agent_id.clone() }),
                    None if !working && notice && matches!(scope, Scope::Orchestrator { .. }) => {
                        Ok(Author::Agent { agent_id: agent_id.clone() })
                    }
                    None if !working => refuse(
                        "no_task",
                        "scope",
                        "This node has no Task: it can only inform with publish_information. Ask the orchestrator to assign you a Task first",
                    ),
                    None => refuse(
                        "not_assigned",
                        "scope.task_id",
                        "Only the node assigned to a Task sends cards for it: use your own Task's id",
                    ),
                }
            }
            Actor::Orchestrator { generation } => {
                if let Scope::Task { task_id } = scope
                    && !notice
                {
                    self.active(task_id)?;
                }
                Ok(Author::Orchestrator { generation: *generation })
            }
            Actor::System if source == Source::Problem => Ok(Author::System),
            _ => fail("forbidden", "Only assigned Agent or current orchestrator may communicate"),
        }
    }
    /// A card stays open until it is answered, read, accepted, replaced or withdrawn.
    pub fn open(&self, i: &Interaction) -> bool {
        if i.withdrawn {
            return false;
        }
        match &i.payload {
            InteractionPayload::Information { read_by } => read_by.is_empty(),
            InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. } => answer.is_none(),
            InteractionPayload::Delivery { acceptance, is_final, .. } => {
                !acceptance.as_ref().is_some_and(|a| a.valid)
                    && (!is_final
                        || matches!(&i.scope, Scope::Task { task_id }
                            if self.tasks.get(task_id).is_some_and(|t| t.final_presentation.as_ref() == Some(&i.id))))
            }
        }
    }
    fn retire(&mut self, id: &InteractionId) {
        if let Some(i) = self.interactions.get_mut(id) {
            i.withdrawn = true;
            i.revision = self.revision + 1;
        }
    }
    /// R8: the same notice, still unread, from the same author to the same place.
    fn repeated_notice(&self, author: &Author, scope: &Scope, to: &Recipient, body: &str, card: &CardSpec, source: Source) -> Option<InteractionId> {
        if source != Source::Information {
            return None;
        }
        self.interactions.values()
            .filter(|i| &i.author == author && &i.scope == scope && &i.recipient == to && self.open(i))
            .filter(|i| matches!(i.payload, InteractionPayload::Information { .. }))
            .max_by_key(|i| i.order)
            .filter(|i| i.body == body && i.card.need == card.need)
            .map(|i| i.id.clone())
    }
    /// R7: after the user asked for a clarification, the author's next card replaces that card.
    /// Plain text (a notice) is still allowed.
    fn clarification_answered(&self, author: &Author, task: Option<&TaskId>, card: &CardSpec) -> Result<()> {
        if card.kind == CardKind::Know {
            return Ok(());
        }
        let pending = self.interactions.values().find(|i| {
            &i.author == author
                && i.clarification.is_some()
                && self.open(i)
                && match (&i.scope, task) {
                    (Scope::Task { task_id }, Some(t)) => task_id == t,
                    (Scope::Orchestrator { .. }, None) => true,
                    _ => false,
                }
        });
        match pending {
            Some(x) if card.replaces.as_ref() != Some(&x.id) => refuse(
                "clarification_pending",
                "card.replaces",
                &format!("The user asked you to clarify card {}: send its corrected version with card.replaces = \"{}\", answer in text with publish_information, or withdraw it", x.id, x.id),
            ),
            _ => Ok(()),
        }
    }
    /// The user said "do it" to a proposal: it becomes a Request for the orchestrator, linked
    /// back to the Task it came from in its body.
    fn start_proposal(&mut self, proposal: &InteractionId, created: &mut Vec<String>, effects: &mut Vec<DomainEffect>) {
        let Some(card) = self.interactions.get(proposal).map(|i| (i.card.clone(), i.scope.clone())) else { return };
        let (card, scope) = card;
        let source = match &scope {
            Scope::Task { task_id } => {
                let title = self.tasks.get(task_id).map(|t| t.title.clone()).unwrap_or_default();
                format!("Proposed while working on task {task_id} (\"{title}\").")
            }
            Scope::Orchestrator { .. } => "Proposed by the orchestrator.".to_owned(),
        };
        let mut body = card.need.clone();
        if !card.why.trim().is_empty() {
            body = format!("{body}\n\n{}", card.why);
        }
        let body = format!("{body}\n\n{source}");
        let request_id = RequestId(format!("proposal-{proposal}"));
        if self.requests.contains_key(&request_id) {
            return;
        }
        self.requests.insert(request_id.clone(), Request::new(request_id.clone(), body));
        created.push(request_id.0.clone());
        effects.push(DomainEffect::NotifyOrchestrator { interaction_id: None, request_id: Some(request_id) });
    }
    /// R5: one blocking Decide, Do or Provide per node at a time.
    fn one_blocking_ask(&self, author: &Author, card: &CardSpec, blocking: bool) -> Result<()> {
        if !blocking || !card.kind.one_at_a_time() || !matches!(author, Author::Agent { .. }) {
            return Ok(());
        }
        let open = self.interactions.values().find(|i| {
            &i.author == author && i.blocking && i.card.kind.one_at_a_time() && self.open(i)
                && card.replaces.as_ref() != Some(&i.id)
        });
        match open {
            Some(x) => refuse(
                "ask_already_open",
                "blocking",
                &format!("Card {} already waits on the user: replace it with card.replaces = \"{}\" (one form holds up to 4 questions), withdraw it, or send this one with blocking false", x.id, x.id),
            ),
            None => Ok(()),
        }
    }
    /// R6: only an open card of the same author is replaced.
    fn replaceable(&self, author: &Author, old: &InteractionId) -> Result<()> {
        let Some(x) = self.interactions.get(old) else {
            return refuse("not_found", "card.replaces", &format!("Card {old} does not exist: replace one of your open cards"));
        };
        if &x.author != author {
            return refuse("forbidden", "card.replaces", &format!("Card {old} is not yours: replace only your own cards"));
        }
        if self.holds.contains_key(old) {
            return refuse("forbidden", "card.replaces", &format!("Card {old} is held by a Claude Code hook: it ends with its hook"));
        }
        if matches!(&x.payload, InteractionPayload::Question { answer: Some(_), .. } | InteractionPayload::ActionRequest { answer: Some(_), .. }) {
            return refuse("already_answered", "card.replaces", &format!("Card {old} is already answered: send a new card without card.replaces"));
        }
        if !self.open(x) {
            return refuse("already_withdrawn", "card.replaces", &format!("Card {old} is no longer open: send a new card without card.replaces"));
        }
        Ok(())
    }
    /// R14: the author's open plans, which a new plan replaces.
    fn open_plans(&self, author: &Author) -> Vec<InteractionId> {
        self.interactions.values()
            .filter(|i| &i.author == author && self.open(i))
            .filter(|i| i.card.content.as_ref().is_some_and(|c| c.kind == ContentKind::Plan))
            .map(|i| i.id.clone())
            .collect()
    }
    /// R15: a card holds back only an open PR, document or delivery of its Task or of a Task it
    /// depends on.
    fn card_blocks(&self, task: Option<&TaskId>, card: &CardSpec) -> Result<()> {
        if card.blocks.is_empty() {
            return Ok(());
        }
        let Some(task) = task.and_then(|t| self.tasks.get(t)) else {
            return refuse("unknown_target", "card.blocks", "Only a card of a Task can block something: remove card.blocks");
        };
        let related: BTreeSet<&TaskId> = std::iter::once(&task.id)
            .chain(&task.dependencies)
            .chain(task.stack.iter().map(|s| &s.upstream_task))
            .collect();
        for target in &card.blocks {
            let ok = match target {
                CardTarget::PullRequest { pr_id } => self.pull_requests.get(pr_id)
                    .is_some_and(|p| p.state == PrState::Open && related.contains(&p.task_id)),
                CardTarget::Document { document_id } => related.iter()
                    .any(|t| self.tasks.get(*t).is_some_and(|t| t.documents.contains_key(document_id))),
                CardTarget::Delivery { interaction_id } => self.interactions.get(interaction_id).is_some_and(|i| {
                    matches!(i.payload, InteractionPayload::Delivery { .. })
                        && matches!(&i.scope, Scope::Task { task_id } if related.contains(task_id))
                        && self.open(i)
                }),
            };
            if !ok {
                return refuse(
                    "unknown_target",
                    "card.blocks",
                    &format!("{} is not an open PR, document or delivery of this Task or of one it depends on: point card.blocks at one, or remove it", serde_json::to_string(target).unwrap_or_default()),
                );
            }
        }
        Ok(())
    }
    /// What an open card holds back, as the deliveries it stands in front of.
    pub(crate) fn blocked_deliveries(&self, i: &Interaction) -> Vec<InteractionId> {
        if !self.open(i) {
            return vec![];
        }
        let mut out = vec![];
        for target in &i.card.blocks {
            for d in self.interactions.values().filter(|d| self.open(d)) {
                let InteractionPayload::Delivery { targets, .. } = &d.payload else { continue };
                let hit = match target {
                    CardTarget::Delivery { interaction_id } => interaction_id == &d.id,
                    CardTarget::PullRequest { pr_id } => targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { pr_id: p, .. } if p == pr_id)),
                    CardTarget::Document { document_id } => targets.iter().any(|t| matches!(t, DeliveryTarget::DocumentRevision { document_id: d, .. } if d == document_id)),
                };
                if hit && d.id != i.id {
                    out.push(d.id.clone());
                }
            }
        }
        out
    }
    /// R17: what blocks a card never ends up blocked by it.
    fn blocks_acyclic(&self, start: &InteractionId) -> Result<()> {
        let mut stack = vec![start.clone()];
        let mut seen = BTreeSet::new();
        while let Some(id) = stack.pop() {
            let Some(i) = self.interactions.get(&id) else { continue };
            for next in self.blocked_deliveries(i) {
                if &next == start {
                    return refuse("dependency_cycle", "card.blocks", "These cards would block each other: remove the card.blocks entry that points back");
                }
                if seen.insert(next.clone()) {
                    stack.push(next);
                }
            }
        }
        Ok(())
    }
    /// R12: a delivery shows the current head of its own Task's registered PR.
    fn current_pr_targets(&self, task: &TaskId, targets: &[DeliveryTarget]) -> Result<()> {
        let owned = self.task(task)?.pr_id.clone();
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let Some(pr) = self.pull_requests.get(pr_id) else {
                return refuse("pr_mismatch", "targets.pr_id", &format!("PR {pr_id} is not registered: call register_pr first"));
            };
            if &pr.task_id != task || owned.as_ref() != Some(pr_id) {
                return refuse("pr_mismatch", "targets.pr_id", &format!("PR {pr_id} belongs to another Task: present only your own Task's PR"));
            }
            if &pr.head != head {
                return refuse("pr_mismatch", "targets.head", &format!("PR {pr_id} is at {}: present its current head", pr.head));
            }
        }
        Ok(())
    }
    /// R22: a Request closed with an answer tells the user, unless something already did.
    fn tell_the_answer(&mut self, actor: &Actor, request: &RequestId, answer: &str, title: Option<&str>,
        created: &mut Vec<String>, effects: &mut Vec<DomainEffect>) -> Result<()> {
        let tasks: BTreeSet<&TaskId> = self.tasks.values().filter(|t| &t.request_id == request).map(|t| &t.id).collect();
        let told = self.interactions.values().any(|i| {
            i.recipient == Recipient::User
                && matches!(i.payload, InteractionPayload::Information { .. })
                && match &i.scope {
                    Scope::Orchestrator { request_id } => request_id.as_ref() == Some(request),
                    Scope::Task { task_id } => tasks.contains(task_id),
                }
        });
        if told {
            return Ok(());
        }
        let mut id = InteractionId(format!("{request}-answer"));
        while self.interactions.contains_key(&id) {
            id.0.push('-');
        }
        let author = match actor {
            Actor::Orchestrator { generation } => Author::Orchestrator { generation: *generation },
            _ => Author::System,
        };
        let need = title.map(|t| clip(t, CardSpec::NEED_MAX)).unwrap_or_else(|| CardSpec::derived(CardKind::Know, answer).need);
        let mut card = CardSpec::new(CardKind::Know, need);
        if answer.trim().chars().count() > CardSpec::WHY_MAX {
            card = card.content(ContentKind::Report, answer.trim());
        } else {
            card = card.why(answer.trim());
        }
        let scope = Scope::Orchestrator { request_id: Some(request.clone()) };
        self.interactions.insert(id.clone(), Interaction {
            attachments: vec![],
            id: id.clone(),
            scope,
            author,
            recipient: Recipient::User,
            body: answer.into(),
            blocking: false,
            subject: None,
            payload: InteractionPayload::Information { read_by: BTreeSet::new() },
            revision: self.revision + 1,
            order: self.revision + 1,
            withdrawn: false,
            card,
            clarification: None,
        });
        created.push(id.0.clone());
        effects.push(DomainEffect::NotifyUser { task_id: None, interaction_id: id });
        Ok(())
    }
    /// R16: the open cards that hold back this delivery, oldest first.
    pub fn blocked_by(&self, delivery: &InteractionId) -> Vec<InteractionId> {
        let mut by: Vec<&Interaction> = self.interactions.values()
            .filter(|i| self.blocked_deliveries(i).contains(delivery))
            .collect();
        by.sort_by_key(|i| i.order);
        by.into_iter().map(|i| i.id.clone()).collect()
    }
    fn unblocked(&self, delivery: &InteractionId) -> Result<()> {
        match self.blocked_by(delivery).first() {
            Some(x) => refuse(
                "blocked",
                "presentation_id",
                &format!("Card {x} blocks this delivery: answer or withdraw it first"),
            ),
            None => Ok(()),
        }
    }
    /// §5: a decision on a delivery is judged against the presentation's
    /// revision, so every decision or invalidation on it must move that revision.
    fn decided(&mut self, presentation: &InteractionId) {
        if let Some(p) = self.interactions.get_mut(presentation) {
            p.revision = self.revision + 1;
        }
    }
    fn invalidate(&mut self, task: &TaskId) {
        let next = self.revision + 1;
        if let Some(presentation) = self.tasks.get(task).and_then(|t| t.final_presentation.clone()) {
            self.decided(&presentation);
        }
        for p in self.interactions.values_mut() {
            if p.scope
                == (Scope::Task {
                    task_id: task.clone(),
                })
            {
                if let InteractionPayload::Delivery {
                    acceptance: Some(a),
                    ..
                } = &mut p.payload
                {
                    if a.valid {
                        a.valid = false;
                        p.revision = next;
                    }
                }
            }
        }
        for a in self
            .merge_authorizations
            .values_mut()
            .filter(|a| &a.task_id == task)
        {
            a.valid = false;
        }
        if let Some(s) = self.stack_readiness.get_mut(task) {
            s.ready = false;
        }
    }
    fn notify(
        &self,
        task: &TaskId,
        interaction: Option<InteractionId>,
        message: &str,
        effects: &mut Vec<DomainEffect>,
    ) -> Result<()> {
        if let Some(agent_id) = &self.task(task)?.agent_id {
            effects.push(DomainEffect::NotifyAgent {
                agent_id: agent_id.clone(),
                task_id: Some(task.clone()),
                interaction_id: interaction,
                message: message.into(),
            });
        }
        Ok(())
    }
    fn dependency_graph(&self) -> Result<()> {
        fn visit(
            s: &DomainState,
            t: &TaskId,
            path: &mut BTreeSet<TaskId>,
            done: &mut BTreeSet<TaskId>,
        ) -> Result<()> {
            if done.contains(t) {
                return Ok(());
            }
            if !path.insert(t.clone()) {
                return fail("dependency_cycle", "Task dependencies contain a cycle");
            };
            let task = s.task(t)?;
            for d in task
                .dependencies
                .iter()
                .chain(task.stack.iter().map(|s| &s.upstream_task))
            {
                visit(s, d, path, done)?;
            }
            path.remove(t);
            done.insert(t.clone());
            Ok(())
        }
        let mut done = BTreeSet::new();
        for t in self.tasks.keys() {
            visit(self, t, &mut BTreeSet::new(), &mut done)?;
        }
        Ok(())
    }
    fn ready_to_start(&self, t: &Task) -> Result<()> {
        for dep in &t.dependencies {
            if self.task(dep)?.state != TaskState::Done {
                return fail("dependency_not_done", "Objective dependency is not done");
            }
        }
        if let Some(stack) = &t.stack {
            let up = self.task(&stack.upstream_task)?;
            if up.state == TaskState::Cancelled {
                return fail("stack_not_ready", "Upstream Task is cancelled");
            };
            let ready = self.stack_readiness.get(&stack.upstream_task);
            if !ready.is_some_and(|r| {
                r.ready
                    && r.presentation_id == stack.presentation_id
                    && r.pr_id == stack.pr_id
                    && r.head == stack.head
            }) || up.final_presentation.as_ref() != Some(&stack.presentation_id)
                || !self
                    .pull_requests
                    .get(&stack.pr_id)
                    .is_some_and(|p| p.task_id == stack.upstream_task && p.head == stack.head)
            {
                return fail("stack_not_ready", "Stack readiness is absent or stale");
            }
        }
        Ok(())
    }
    fn accepted(&self, p: &Interaction) -> bool {
        matches!(&p.payload,InteractionPayload::Delivery{acceptance:Some(a),..} if a.valid)
    }
    /// An invalid acceptance the user did not withdraw: no feedback on that
    /// delivery and no document rejection on its Task after it was given.
    pub(super) fn superseded(&self, p: &Interaction) -> Option<SupersededAcceptance> {
        let (
            Scope::Task { task_id },
            InteractionPayload::Delivery {
                acceptance: Some(a),
                feedback,
                ..
            },
        ) = (&p.scope, &p.payload)
        else {
            return None;
        };
        let withdrawn = feedback.iter().any(|f| f.at_revision > a.at_revision)
            || self.document_reviews.iter().any(|r| {
                &r.task_id == task_id
                    && r.decisions
                        .iter()
                        .any(|d| !d.accepted && d.at_revision > a.at_revision)
            });
        (!a.valid && !withdrawn).then(|| SupersededAcceptance {
            task_id: task_id.clone(),
            presentation_id: p.id.clone(),
            user_id: a.user_id.clone(),
            accepted_at: a.at_revision,
        })
    }
    /// D04: a package is accepted only when every piece carries its own current
    /// decision. A merge authorization covers its PR and the presentation text;
    /// a document approval covers only that revision; feedback dates both out.
    fn covered(&self, presentation: &InteractionId) -> bool {
        let Some(p) = self.interactions.get(presentation) else {
            return false;
        };
        let (Scope::Task { task_id }, InteractionPayload::Delivery { targets, feedback, .. }) =
            (&p.scope, &p.payload)
        else {
            return false;
        };
        let after = feedback.iter().map(|f| f.at_revision).max().unwrap_or(0);
        let mut text_covered = !targets
            .iter()
            .any(|t| matches!(t, DeliveryTarget::Text { .. }));
        for target in targets {
            match target {
                DeliveryTarget::Text { .. } => {}
                DeliveryTarget::DocumentRevision {
                    document_id,
                    revision_id,
                } => {
                    let decided = self
                        .document_reviews
                        .iter()
                        .find(|r| &r.task_id == task_id && &r.document_id == document_id)
                        .and_then(|r| r.decisions.last())
                        .is_some_and(|d| {
                            d.accepted
                                && &d.revision_id == revision_id
                                && d.presentation_id.as_ref() == Some(presentation)
                                && d.at_revision > after
                        });
                    if !decided {
                        return false;
                    }
                }
                DeliveryTarget::PullRequest { pr_id, head } => {
                    let authorized = self.merge_authorizations.values().any(|a| {
                        a.valid
                            && &a.presentation_id == presentation
                            && &a.pr_id == pr_id
                            && &a.head == head
                    });
                    if !authorized {
                        return false;
                    }
                    text_covered = true;
                }
            }
        }
        text_covered
    }
    fn settle(&mut self, presentation: &InteractionId, u: &str) {
        if self.covered(presentation) {
            self.record_acceptance(presentation, u);
        }
    }
    fn blockers(&self, t: &TaskId) -> bool {
        self.interactions.values().any(|i| {
            i.scope == (Scope::Task { task_id: t.clone() })
                && i.blocking
                && !i.withdrawn
                && matches!(
                    i.payload,
                    InteractionPayload::Question { answer: None, .. }
                        | InteractionPayload::ActionRequest { answer: None, .. }
                )
        })
    }
    /// What still keeps a Task with a settled final delivery from being done.
    fn completable(&self, t: &Task, targets: &[DeliveryTarget]) -> Result<()> {
        if self.blockers(&t.id) {
            return fail(
                "blocking_interaction",
                "Task has an unanswered blocking Interaction",
            );
        };
        for dep in &t.dependencies {
            if self.task(dep)?.state != TaskState::Done {
                return fail("dependency_not_done", "Objective dependency incomplete");
            }
        }
        for target in targets {
            if let DeliveryTarget::PullRequest { pr_id, head } = target {
                let pr = &self.pull_requests[pr_id];
                if pr.state != PrState::Merged || pr.merged_head.as_ref() != Some(head) {
                    return fail(
                        "merge_not_confirmed",
                        "Presented head is not confirmed merged",
                    );
                };
                // User conformity for the exact final delivery is mandatory. A PR
                // merged externally can be explicitly accepted after observation;
                // observation itself never grants that conformity.
            }
        }
        if let Some(pr_id) = &t.pr_id {
            let presented = targets
                .iter()
                .any(|x| matches!(x, DeliveryTarget::PullRequest { pr_id: p, .. } if p == pr_id));
            match self.pull_requests[pr_id].state {
                PrState::Open => {
                    return fail(
                        "merge_not_confirmed",
                        "The Task's registered PR is still open",
                    );
                }
                PrState::Merged if !presented => {
                    return fail(
                        "acceptance_missing",
                        "The merged PR was never presented for acceptance",
                    );
                }
                _ => {}
            }
        }
        Ok(())
    }
    /// Waiting and activity states are always recomputed from the facts; an
    /// import runs this once so the first live command never rewrites every Task.
    pub fn derive_states(&mut self) {
        let updates: Vec<_> = self
            .tasks
            .values()
            .filter(|t| !t.state.terminal())
            .map(|t| {
                let state = if self.ready_to_start(t).is_err() {
                    TaskState::WaitingDependency
                } else if self.blockers(&t.id) {
                    TaskState::NeedsYou
                } else if let Some(p) = t
                    .final_presentation
                    .as_ref()
                    .and_then(|p| self.interactions.get(p))
                {
                    if self.accepted(p) {
                        match &p.payload {
                            InteractionPayload::Delivery { targets, .. }
                                if self.completable(t, targets).is_ok() => TaskState::Done,
                            _ => TaskState::Closing,
                        }
                    } else if self.handed_back(p).is_some() {
                        TaskState::Running
                    } else {
                        TaskState::InReview
                    }
                } else if t.started {
                    TaskState::Running
                } else {
                    TaskState::NotStarted
                };
                (t.id.clone(), state)
            })
            .collect();
        for (id, state) in updates {
            self.tasks.get_mut(&id).unwrap().state = state;
        }
    }
    /// Tasks whose revision must move: their row, their decisions and
    /// deliveries, PR, authorizations, stack readiness, reviews or linked
    /// document versions differ in `next`. Information neither blocks nor dates
    /// a decision (R07), so it never moves a Task's revision.
    fn touched_tasks(&self, next: &DomainState) -> BTreeSet<TaskId> {
        let mut touched = BTreeSet::new();
        for (id, task) in &next.tasks {
            if self.tasks.get(id) != Some(task) {
                touched.insert(id.clone());
            }
        }
        for (id, interaction) in &next.interactions {
            if matches!(interaction.payload, InteractionPayload::Information { .. }) {
                continue;
            }
            if let Scope::Task { task_id } = &interaction.scope
                && self.interactions.get(id) != Some(interaction)
            {
                touched.insert(task_id.clone());
            }
        }
        for (id, pr) in &next.pull_requests {
            if self.pull_requests.get(id) != Some(pr) {
                touched.insert(pr.task_id.clone());
            }
        }
        for (id, authorization) in &next.merge_authorizations {
            if self.merge_authorizations.get(id) != Some(authorization) {
                touched.insert(authorization.task_id.clone());
            }
        }
        for (id, readiness) in &next.stack_readiness {
            if self.stack_readiness.get(id) != Some(readiness) {
                touched.insert(id.clone());
            }
        }
        for review in &next.document_reviews {
            if !self.document_reviews.contains(review) {
                touched.insert(review.task_id.clone());
            }
        }
        for (id, document) in &next.documents {
            if self.documents.get(id) != Some(document) {
                touched.extend(
                    next.tasks
                        .values()
                        .filter(|task| task.documents.contains_key(id))
                        .map(|task| task.id.clone()),
                );
            }
        }
        touched
    }
    fn schedule_unchanged(&self, id: &ScheduleId, expected: Option<i64>) -> Result<()> {
        let Some(expected) = expected else {
            return Ok(());
        };
        if self
            .schedule
            .schedules
            .get(id)
            .is_some_and(|s| s.revision != expected)
        {
            let mut e = DomainError::new("stale_revision", "Schedule changed; refresh before applying");
            e.current_revision = Some(self.revision);
            return Err(e);
        }
        Ok(())
    }
    /// The revision a command's expected_revision must reach: its Task for
    /// Task-directed commands, the Interaction for decisions on one, and the
    /// whole state for commands without a narrower subject.
    fn subject_revision(&self, c: &Command) -> Option<Revision> {
        let task = |id: &TaskId| Some(self.tasks.get(id).map_or(0, |t| t.revision));
        let interaction =
            |id: &InteractionId| Some(self.interactions.get(id).map_or(0, |i| i.revision));
        match c {
            Command::AssignTask { task_id, .. }
            | Command::RetitleTask { task_id, .. }
            | Command::SetDependencies { task_id, .. }
            | Command::StartTask { task_id }
            | Command::CreateDocument { task_id, .. }
            | Command::ReviseDocument { task_id, .. }
            | Command::LinkTaskDocument { task_id, .. }
            | Command::DecideDocumentReview { task_id, .. }
            | Command::RegisterPr { task_id, .. }
            | Command::PresentResult { task_id, .. }
            | Command::PublishStackReadiness { task_id, .. }
            | Command::CompleteTask { task_id, .. }
            | Command::CancelTask { task_id }
            | Command::ReopenTask { task_id }
            | Command::ArchiveTask { task_id, .. }
            | Command::HoldHook { task_id, .. } => task(task_id),
            Command::PublishInformation { scope, .. }
            | Command::AskQuestion { scope, .. }
            | Command::RequestAction { scope, .. }
            | Command::RaiseProblem { scope, .. } => match scope {
                Scope::Task { task_id } => task(task_id),
                Scope::Orchestrator { .. } => None,
            },
            Command::MarkRead { interaction_id }
            | Command::Withdraw { interaction_id }
            | Command::RequestClarification { interaction_id, .. }
            | Command::AnswerQuestion { interaction_id, .. }
            | Command::LinkInteractionSubject { interaction_id, .. }
            | Command::SubmitFeedback {
                presentation_id: interaction_id,
                ..
            }
            | Command::AcceptDelivery {
                presentation_id: interaction_id,
                ..
            }
            | Command::AuthorizeMerge {
                presentation_id: interaction_id,
                ..
            } => interaction(interaction_id),
            Command::ClaimMergeAuthorization {
                authorization_id, ..
            }
            | Command::ObserveMergeAttempt {
                authorization_id, ..
            } => Some(
                self.merge_authorizations
                    .get(authorization_id)
                    .and_then(|a| self.tasks.get(&a.task_id))
                    .map_or(0, |t| t.revision),
            ),
            Command::ObservePrState { pr_id, .. } => Some(
                self.pull_requests
                    .get(pr_id)
                    .and_then(|p| self.tasks.get(&p.task_id))
                    .map_or(0, |t| t.revision),
            ),
            // A Schedule's own revision is its subject: Update carries the next
            // one, Delete and Claim the one they read. The rule checks it.
            Command::CreateSchedule { .. } | Command::UpdateSchedule { .. } => Some(0),
            Command::DeleteSchedule {
                expected_schedule_revision: Some(_),
                ..
            }
            | Command::ClaimScheduleRun {
                expected_schedule_revision: Some(_),
                ..
            } => Some(0),
            _ => None,
        }
    }
    /// Clone, validate, swap: even an error discovered after preparing several changes is atomic.
    /// The current state was validated when it was loaded or committed; only the result is checked.
    /// Semantic receipts belong to the SQL transaction and are looked up before this method.
    pub fn execute(&mut self, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome> {
        if envelope.protocol_version != PROTOCOL_VERSION {
            return fail("incompatible_protocol", "Protocol version 2 is required");
        };
        if envelope.data_epoch != self.data_epoch {
            return fail("stale_epoch", "Business data epoch changed");
        };
        id(&envelope.command_id.0)?;
        self.fence(actor)?;
        // §5: staleness is judged against the subject being decided. A client
        // may act on a snapshot that predates unrelated changes, never on one
        // that predates the last change to this subject or the future.
        let floor = self
            .subject_revision(&envelope.command)
            .unwrap_or(self.revision);
        if envelope.expected_revision < floor || envelope.expected_revision > self.revision {
            let mut e = DomainError::new("stale_revision", "Refresh before applying this decision");
            e.current_revision = Some(self.revision);
            return Err(e);
        };
        if self.revision >= MAX_REVISION {
            return fail("invalid_input", "Revision limit reached");
        };
        let mut next = self.clone();
        let mut effects = vec![];
        let mut created = vec![];
        next.apply(actor, &envelope.command, &mut effects, &mut created)?;
        next.derive_states();
        for id in self.touched_tasks(&next) {
            next.tasks.get_mut(&id).unwrap().revision = self.revision + 1;
        }
        let changed = next != *self;
        if changed {
            next.revision += 1;
        } else {
            effects.clear();
        }
        next.validate()?;
        let superseded_acceptances = self
            .interactions
            .values()
            .filter(|p| self.accepted(p))
            .filter_map(|p| next.superseded(&next.interactions[&p.id]))
            .collect();
        let outcome = CommandOutcome {
            revision: next.revision,
            changed,
            effects,
            created_ids: created,
            superseded_acceptances,
        };
        *self = next;
        Ok(outcome)
    }
    fn apply(
        &mut self,
        actor: &Actor,
        c: &Command,
        effects: &mut Vec<DomainEffect>,
        created: &mut Vec<String>,
    ) -> Result<()> {
        match c {
            Command::CreateSchedule { schedule } => {
                system(actor)?;
                id(&schedule.id.0)?;
                schedule.validate()?;
                if schedule.revision != 1 || schedule.deleted_at.is_some() {
                    return fail("invalid_input", "New Schedule needs revision one and no deletion");
                }
                if self.schedule.schedules.contains_key(&schedule.id) {
                    return fail("idempotency_conflict", "Schedule ID already exists");
                }
                self.schedule
                    .schedules
                    .insert(schedule.id.clone(), schedule.clone());
                created.push(schedule.id.0.clone());
            }
            Command::UpdateSchedule { schedule } => {
                system(actor)?;
                id(&schedule.id.0)?;
                schedule.validate()?;
                let current = self
                    .schedule
                    .schedules
                    .get(&schedule.id)
                    .ok_or_else(|| DomainError::new("not_found", "Schedule does not exist"))?;
                if current.deleted_at.is_some() || schedule.deleted_at.is_some() {
                    return fail("not_found", "Schedule was deleted");
                }
                if self.schedule.runs.values().any(|run| {
                    run.schedule_id == schedule.id && run.state == ScheduleRunState::Launching
                }) {
                    return fail("conflict", "Schedule run is being sent");
                }
                if current.revision.checked_add(1) != Some(schedule.revision) {
                    return fail("stale_revision", "Schedule revision must advance by one");
                }
                self.schedule
                    .schedules
                    .insert(schedule.id.clone(), schedule.clone());
            }
            Command::DeleteSchedule {
                schedule_id,
                deleted_at,
                expected_schedule_revision,
            } => {
                system(actor)?;
                if *deleted_at < 0 {
                    return fail("invalid_input", "Invalid deletion time");
                }
                self.schedule_unchanged(schedule_id, *expected_schedule_revision)?;
                let schedule = self
                    .schedule
                    .schedules
                    .get_mut(schedule_id)
                    .ok_or_else(|| DomainError::new("not_found", "Schedule does not exist"))?;
                if schedule.deleted_at.is_some() {
                    return fail("not_found", "Schedule was deleted");
                }
                if self.schedule.runs.values().any(|run| {
                    &run.schedule_id == schedule_id && run.state == ScheduleRunState::Launching
                }) {
                    return fail("conflict", "Schedule run is being sent");
                }
                if schedule.revision >= MAX_REVISION as i64 {
                    return fail("invalid_input", "Schedule revision limit reached");
                }
                schedule.revision += 1;
                schedule.enabled = false;
                schedule.next_run = None;
                schedule.deleted_at = Some(*deleted_at);
            }
            Command::ClaimScheduleRun {
                schedule_id,
                run_id,
                origin,
                now,
                next_after_now,
                expected_schedule_revision,
            } => {
                system(actor)?;
                id(&run_id.0)?;
                self.schedule_unchanged(schedule_id, *expected_schedule_revision)?;
                self.schedule.claim(
                    schedule_id,
                    run_id.clone(),
                    *origin,
                    *now,
                    *next_after_now,
                )?;
                created.push(run_id.0.clone());
            }
            Command::RecordScheduleRunSent {
                run_id,
                request,
                task,
                launch,
                finished_at,
            } => {
                system(actor)?;
                id(&request.id.0)?;
                id(&task.id.0)?;
                nonempty(&request.body)?;
                nonempty(&task.title)?;
                let run = self
                    .schedule
                    .runs
                    .get(run_id)
                    .ok_or_else(|| DomainError::new("not_found", "Schedule run not found"))?;
                let definition = run
                    .definition_snapshot
                    .as_ref()
                    .ok_or_else(|| DomainError::new("invalid_input", "Run has no claimed definition"))?;
                launch.validate(definition)?;
                if request.body != definition.text
                    || task.title != definition.title
                    || task.request_id != request.id
                    || request.closed
                    || task.agent_id.is_some()
                    || task.state != TaskState::NotStarted
                    || task.started
                    || task.final_presentation.is_some()
                    || task.pr_id.is_some()
                    || !task.dependencies.is_empty()
                    || task.stack.is_some()
                    || !task.documents.is_empty()
                    || task.archived
                {
                    return fail("invalid_input", "Request and Task must match the claimed Schedule definition and initial Task state");
                }
                if self.requests.contains_key(&request.id) || self.tasks.contains_key(&task.id) {
                    return fail("idempotency_conflict", "Scheduled Request or Task ID already exists");
                }
                self.schedule.record_sent(
                    run_id,
                    request.id.clone(),
                    task.id.clone(),
                    launch.clone(),
                    *finished_at,
                )?;
                self.requests.insert(request.id.clone(), request.clone());
                self.tasks.insert(task.id.clone(), task.clone());
                created.push(request.id.0.clone());
                created.push(task.id.0.clone());
            }
            Command::ClaimScheduleLaunch { run_id, now } => {
                system(actor)?;
                let task_id = self.schedule.runs.get(run_id)
                    .and_then(|run| run.task_id.as_ref())
                    .ok_or_else(|| DomainError::new("conflict", "Scheduled Run has no Task"))?;
                let task = self.tasks.get(task_id)
                    .ok_or_else(|| DomainError::new("conflict", "Scheduled Task is missing"))?;
                if task.agent_id.is_some()
                    || task.state != TaskState::NotStarted
                    || task.started
                    || !task.dependencies.is_empty()
                    || task.stack.is_some()
                {
                    return fail("conflict", "Scheduled Task is no longer ready to launch");
                }
                self.schedule.claim_launch(run_id, *now)?;
            }
            Command::RecordScheduleLaunchHanded { run_id, session_id, agent_id, finished_at } => {
                system(actor)?;
                if let Some(agent_id) = agent_id {
                    if !self.agents.contains_key(agent_id) {
                        return fail("not_found", "Agent missing");
                    }
                    let task_id = self.schedule.runs.get(run_id)
                        .and_then(|run| run.task_id.clone())
                        .ok_or_else(|| DomainError::new("conflict", "Scheduled Run has no Task"))?;
                    let task = self.active(&task_id)?;
                    if task.agent_id.is_some() {
                        return fail("conflict", "Scheduled Task is already assigned");
                    }
                    self.schedule.record_launch_handed(run_id, session_id.clone(), Some(agent_id.clone()), *finished_at)?;
                    self.tasks.get_mut(&task_id).unwrap().agent_id = Some(agent_id.clone());
                } else {
                    self.schedule.record_launch_handed(run_id, session_id.clone(), None, *finished_at)?;
                }
            }
            Command::RecordScheduleRunFailed {
                run_id,
                reason,
                finished_at,
            } => {
                system(actor)?;
                self.schedule
                    .record_failed(run_id, reason.clone(), *finished_at)?;
                if let Some(task) = self.schedule.runs[run_id].task_id.as_ref().and_then(|id| self.tasks.get(id)).cloned() {
                    effects.push(DomainEffect::ScheduleLaunchFailed {
                        task_id: task.id.clone(),
                        title: task.title.clone(),
                        reason: reason.trim().to_owned(),
                    });
                    let problem = InteractionId(format!("problem-launch-{}", run_id.0));
                    let card = CardSpec::new(CardKind::Problem, clip(&format!("Scheduled task \"{}\" could not start", task.title), CardSpec::NEED_MAX))
                        .why(clip(reason, CardSpec::WHY_MAX))
                        .content(ContentKind::Diagnosis, reason.trim());
                    self.insert_interaction(
                        actor,
                        &problem,
                        &Scope::Task { task_id: task.id.clone() },
                        &Recipient::User,
                        reason.trim(),
                        false,
                        &[],
                        InteractionPayload::Question {
                            options: vec![QuestionOption { id: "seen".into(), label: "Got it".into(), description: Some("It stays not started; ask the orchestrator to start it again".into()), inputs: vec![] }],
                            answer: None,
                        },
                        &card,
                        Source::Problem,
                    )?;
                    created.push(problem.0.clone());
                    effects.push(DomainEffect::NotifyUser { task_id: Some(task.id), interaction_id: problem });
                }
            }
            Command::InterruptScheduleRunsOnRestart => {
                system(actor)?;
                self.schedule.interrupt_on_restart();
            }
            Command::CreateRequest { request_id, body, auto_merge } => {
                self.manager(actor)?;
                id(&request_id.0)?;
                nonempty(body)?;
                if self.requests.contains_key(request_id) {
                    return fail("invalid_input", "Request already exists");
                };
                self.requests.insert(
                    request_id.clone(),
                    Request { auto_merge: *auto_merge, ..Request::new(request_id.clone(), body.clone()) },
                );
                created.push(request_id.0.clone());
                // 8ccd131 handed every new request of the user to the orchestrator.
                if matches!(actor, Actor::User { .. }) {
                    effects.push(DomainEffect::NotifyOrchestrator { interaction_id: None, request_id: Some(request_id.clone()) });
                }
            }
            Command::CloseRequest { request_id, answer, withdrawn, title } => {
                self.manager(actor)?;
                if self
                    .tasks
                    .values()
                    .any(|t| &t.request_id == request_id && !t.state.terminal())
                {
                    return fail("task_not_active", "Request still has unfinished Tasks");
                };
                for text in answer.iter().chain(title) {
                    nonempty(text)?;
                }
                let request = self
                    .requests
                    .get_mut(request_id)
                    .ok_or_else(|| DomainError::new("not_found", "Request missing"))?;
                request.closed = true;
                request.withdrawn = *withdrawn;
                if answer.is_some() {
                    request.answer = answer.clone();
                }
                if title.is_some() {
                    request.title = title.clone();
                }
                if let Some(answer) = answer {
                    self.tell_the_answer(actor, request_id, answer, title.as_deref(), created, effects)?;
                }
            }
            Command::RegisterAgent { agent_id, name } => {
                system(actor)?;
                id(&agent_id.0)?;
                nonempty(name)?;
                if self.agents.contains_key(agent_id) {
                    return fail("invalid_input", "Agent instance already exists");
                };
                self.agents.insert(
                    agent_id.clone(),
                    Agent {
                        id: agent_id.clone(),
                        name: name.clone(),
                        connected: false,
                    },
                );
                created.push(agent_id.0.clone());
            }
            Command::ObserveAgent {
                agent_id,
                connected,
            } => {
                system(actor)?;
                self.agents
                    .get_mut(agent_id)
                    .ok_or_else(|| DomainError::new("not_found", "Agent missing"))?
                    .connected = *connected;
            }
            Command::AdvanceOrchestratorGeneration { generation } => {
                system(actor)?;
                if *generation <= self.orchestrator_generation || *generation > MAX_REVISION {
                    return fail("stale_generation", "Generation must advance monotonically");
                };
                self.orchestrator_generation = *generation;
            }
            Command::CreateTask {
                task_id,
                request_id,
                title,
                agent_id,
                review_prs,
                dependencies,
                stack,
            } => {
                self.manager(actor)?;
                id(&task_id.0)?;
                nonempty(title)?;
                if self.tasks.contains_key(task_id) {
                    return fail("invalid_input", "Task already exists");
                };
                if !self.requests.get(request_id).is_some_and(|r| !r.closed) {
                    return fail("not_found", "Open Request required");
                };
                if agent_id
                    .as_ref()
                    .is_some_and(|a| !self.agents.contains_key(a))
                {
                    return fail("not_found", "Agent missing");
                };
                if let Some(agent) = agent_id {
                    self.free(agent, task_id)?;
                }
                self.tasks.insert(
                    task_id.clone(),
                    Task {
                        id: task_id.clone(),
                        request_id: request_id.clone(),
                        agent_id: agent_id.clone(),
                        title: title.clone(),
                        state: TaskState::NotStarted,
                        revision: self.revision + 1,
                        started: false,
                        review_prs: *review_prs,
                        final_presentation: None,
                        pr_id: None,
                        dependencies: dependencies.clone(),
                        stack: stack.clone(),
                        documents: BTreeMap::new(),
                        archived: false,
                    },
                );
                self.dependency_graph()?;
                created.push(task_id.0.clone());
            }
            Command::AssignTask { task_id, agent_id } => {
                self.manager(actor)?;
                self.active(task_id)?;
                if self.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(task_id)
                    && run.launch.as_ref().is_some_and(|launch| matches!(launch.state,
                        ScheduleLaunchState::Pending | ScheduleLaunchState::Dispatching))) {
                    return fail("conflict", "Scheduled Task launch is pending or dispatching");
                }
                if !self.agents.contains_key(agent_id) {
                    return fail("not_found", "Agent missing");
                };
                if self.task(task_id)?.agent_id.as_ref() != Some(agent_id) {
                    self.free(agent_id, task_id)?;
                    self.invalidate(task_id);
                    self.tasks.get_mut(task_id).unwrap().agent_id = Some(agent_id.clone());
                }
            }
            Command::TitleRequest { request_id, title } => {
                self.manager(actor)?;
                nonempty(title)?;
                if title.chars().count() > REQUEST_TITLE_LIMIT {
                    return fail("invalid_input", "Request title is too long");
                }
                self.requests
                    .get_mut(request_id)
                    .ok_or_else(|| DomainError::new("not_found", "Request missing"))?
                    .title = Some(title.clone());
            }
            Command::RetitleTask { task_id, title } => {
                self.manager(actor)?;
                nonempty(title)?;
                self.task(task_id)?;
                self.tasks.get_mut(task_id).unwrap().title = title.clone();
            }
            Command::SetDependencies {
                task_id,
                dependencies,
                stack,
            } => {
                self.manager(actor)?;
                self.active(task_id)?;
                if self.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(task_id)
                    && run.launch.as_ref().is_some_and(|launch| matches!(launch.state,
                        ScheduleLaunchState::Pending | ScheduleLaunchState::Dispatching))) {
                    return fail("conflict", "Scheduled Task launch is pending or dispatching");
                }
                self.invalidate(task_id);
                let t = self.tasks.get_mut(task_id).unwrap();
                t.dependencies = dependencies.clone();
                t.stack = stack.clone();
                self.dependency_graph()?;
            }
            Command::StartTask { task_id } => {
                self.task_editor(actor, task_id)?;
                let t = self.task(task_id)?;
                if t.agent_id.is_none() {
                    return fail("not_assigned", "Task requires an Agent");
                };
                self.ready_to_start(t)?;
                self.tasks.get_mut(task_id).unwrap().started = true;
            }
            Command::PublishInformation {
                attachments,
                interaction_id,
                scope,
                recipient,
                body,
                card,
            } => {
                if let Some(existing) = self.insert_interaction(
                    actor,
                    interaction_id,
                    scope,
                    recipient,
                    body,
                    false,
                    attachments,
                    InteractionPayload::Information {
                        read_by: BTreeSet::new(),
                    },
                    card,
                    Source::Information,
                )? {
                    created.push(existing.0);
                    return Ok(());
                }
                created.push(interaction_id.0.clone());
                if matches!(recipient, Recipient::User) {
                    effects.push(DomainEffect::NotifyUser {
                        task_id: match scope {
                            Scope::Task { task_id } => Some(task_id.clone()),
                            Scope::Orchestrator { .. } => None,
                        },
                        interaction_id: interaction_id.clone(),
                    });
                }
            }
            Command::AskQuestion {
                attachments,
                interaction_id,
                scope,
                recipient,
                body,
                blocking,
                options,
                card,
            }
            | Command::RequestAction {
                attachments,
                interaction_id,
                scope,
                recipient,
                body,
                blocking,
                options,
                card,
            } => {
                validate_options(options)?;
                let payload = if matches!(c, Command::AskQuestion { .. }) {
                    InteractionPayload::Question {
                        options: options.clone(),
                        answer: None,
                    }
                } else {
                    InteractionPayload::ActionRequest {
                        options: options.clone(),
                        answer: None,
                    }
                };
                let source = if matches!(c, Command::AskQuestion { .. }) { Source::Question } else { Source::Action };
                self.insert_interaction(
                    actor,
                    interaction_id,
                    scope,
                    recipient,
                    body,
                    *blocking,
                    attachments,
                    payload,
                    card,
                    source,
                )?;
                created.push(interaction_id.0.clone());
                if matches!(recipient, Recipient::User) {
                    effects.push(DomainEffect::NotifyUser {
                        task_id: match scope {
                            Scope::Task { task_id } => Some(task_id.clone()),
                            Scope::Orchestrator { .. } => None,
                        },
                        interaction_id: interaction_id.clone(),
                    });
                }
                if matches!(recipient, Recipient::Orchestrator) {
                    effects.push(DomainEffect::NotifyOrchestrator {
                        interaction_id: Some(interaction_id.clone()),
                        request_id: None,
                    });
                }
            }
            Command::HoldHook {
                interaction_id,
                task_id,
                agent_id,
                hold,
                body,
                options,
                card,
            } => {
                if !matches!(actor, Actor::System) {
                    return fail("forbidden", "Only Alondra holds a hook");
                }
                validate_options(options)?;
                let scope = Scope::Task { task_id: task_id.clone() };
                self.assigned(&Actor::Agent { agent_id: agent_id.clone() }, task_id)?;
                self.insert_interaction(
                    &Actor::Agent { agent_id: agent_id.clone() },
                    interaction_id,
                    &scope,
                    &Recipient::User,
                    body,
                    true,
                    &[],
                    InteractionPayload::Question {
                        options: options.clone(),
                        answer: None,
                    },
                    card,
                    Source::Hold,
                )?;
                self.holds.insert(interaction_id.clone(), *hold);
                created.push(interaction_id.0.clone());
                effects.push(DomainEffect::NotifyUser {
                    task_id: Some(task_id.clone()),
                    interaction_id: interaction_id.clone(),
                });
            }
            Command::Withdraw { interaction_id } => {
                let i = self.interaction(interaction_id)?;
                match (&i.author, actor) {
                    (Author::Agent { agent_id }, Actor::Agent { agent_id: a }) if agent_id == a => {}
                    (Author::Orchestrator { .. }, Actor::Orchestrator { .. }) => {}
                    (_, Actor::System) if self.holds.contains_key(interaction_id) => {}
                    (Author::System, Actor::System) => {}
                    _ => return refuse("forbidden", "interaction_id", "Only a card's author withdraws it: withdraw only your own cards"),
                }
                if matches!(&i.payload, InteractionPayload::Question { answer: Some(_), .. } | InteractionPayload::ActionRequest { answer: Some(_), .. }) {
                    return refuse("question_already_answered", "interaction_id", "The user already answered this card: it is final, act on the answer");
                }
                if matches!(&i.payload, InteractionPayload::Delivery { acceptance: Some(a), .. } if a.valid) {
                    return refuse("invalid_action_for_kind", "interaction_id", "The user already accepted this delivery: it cannot be withdrawn");
                }
                if !i.withdrawn {
                    let presented = match (&i.scope, &i.payload) {
                        (Scope::Task { task_id }, InteractionPayload::Delivery { .. })
                            if self.tasks.get(task_id).is_some_and(|t| t.final_presentation.as_ref() == Some(interaction_id)) => Some(task_id.clone()),
                        _ => None,
                    };
                    self.retire(interaction_id);
                    if let Some(task) = presented {
                        self.invalidate(&task);
                        self.tasks.get_mut(&task).unwrap().final_presentation = None;
                    }
                }
            }
            Command::RequestClarification { interaction_id, reason, text } => {
                let u = user(actor)?;
                if let Some(text) = text {
                    nonempty(text)?;
                }
                let i = self.interaction(interaction_id)?;
                if i.recipient != Recipient::User {
                    return refuse("forbidden", "interaction_id", "Ask for a clarification only on a card addressed to you");
                }
                if self.holds.contains_key(interaction_id) || matches!(i.author, Author::System) {
                    return refuse("invalid_action_for_kind", "interaction_id", "Alondra wrote this card from a Claude Code prompt: answer it or reply to the node instead");
                }
                if !self.open(i) {
                    return refuse("invalid_action_for_kind", "interaction_id", "This card is no longer open: there is nothing to clarify");
                }
                let message = format!(
                    "The user asks you to clarify card {interaction_id} ({}){}. Send its corrected version with card.replaces = \"{interaction_id}\", answer in text with publish_information, or withdraw it.",
                    match reason { ClarificationReason::Unclear => "it is unclear", ClarificationReason::Contradicts => "it contradicts something", ClarificationReason::MissingContext => "it lacks context" },
                    text.as_ref().map(|t| format!(": {}", t.trim())).unwrap_or_default(),
                );
                let (author, scope) = (i.author.clone(), i.scope.clone());
                let i = self.interactions.get_mut(interaction_id).unwrap();
                i.clarification = Some(Clarification { reason: *reason, text: text.clone(), user_id: u, at_revision: self.revision + 1 });
                i.revision = self.revision + 1;
                match (author, scope) {
                    (Author::Agent { agent_id }, Scope::Task { task_id }) => effects.push(DomainEffect::NotifyAgent {
                        agent_id, task_id: Some(task_id), interaction_id: Some(interaction_id.clone()), message,
                    }),
                    (Author::Orchestrator { .. }, _) => effects.push(DomainEffect::NotifyOrchestrator {
                        interaction_id: Some(interaction_id.clone()), request_id: None,
                    }),
                    _ => {}
                }
            }
            Command::RaiseProblem { attachments, interaction_id, scope, body, options, card } => {
                if !matches!(actor, Actor::System | Actor::Orchestrator { .. }) {
                    return refuse("forbidden", "card.kind", "Only Alondra and the orchestrator raise a problem: describe it with ask or publish_information instead");
                }
                validate_options(options)?;
                self.insert_interaction(
                    actor,
                    interaction_id,
                    scope,
                    &Recipient::User,
                    body,
                    false,
                    attachments,
                    InteractionPayload::Question { options: options.clone(), answer: None },
                    card,
                    Source::Problem,
                )?;
                created.push(interaction_id.0.clone());
                effects.push(DomainEffect::NotifyUser {
                    task_id: match scope {
                        Scope::Task { task_id } => Some(task_id.clone()),
                        Scope::Orchestrator { .. } => None,
                    },
                    interaction_id: interaction_id.clone(),
                });
            }
            Command::MarkRead { interaction_id } => {
                let u = user(actor)?;
                let i = self
                    .interactions
                    .get_mut(interaction_id)
                    .ok_or_else(|| DomainError::new("not_found", "Interaction missing"))?;
                let InteractionPayload::Information { read_by } = &mut i.payload else {
                    return fail(
                        "invalid_action_for_kind",
                        "Only information can be marked read",
                    );
                };
                if read_by.is_empty() {
                    read_by.insert(u);
                    i.revision = self.revision + 1;
                }
            }
            Command::AnswerQuestion {
                attachments,
                interaction_id,
                option_id,
                inputs,
            } => {
                let i = self.interaction(interaction_id)?.clone();
                let responder = match (&i.recipient, actor) {
                    (Recipient::User, Actor::User { user_id }) => user_id.clone(),
                    (Recipient::Agent { agent_id }, Actor::Agent { agent_id: a })
                        if agent_id == a =>
                    {
                        a.0.clone()
                    }
                    (Recipient::Orchestrator, Actor::Orchestrator { .. }) => {
                        "alondra-orchestrator".into()
                    }
                    _ => return fail("forbidden", "Only the intended recipient can answer"),
                };
                if let (Scope::Task { task_id }, false) = (&i.scope, i.card.kind == CardKind::Propose) {
                    self.active(task_id)?;
                }
                let (options, answer) = match &i.payload {
                    InteractionPayload::Question { options, answer }
                    | InteractionPayload::ActionRequest { options, answer } => (options, answer),
                    _ => {
                        return fail(
                            "invalid_action_for_kind",
                            "This Interaction cannot be answered",
                        );
                    }
                };
                if answer.is_some() {
                    return refuse("question_already_answered", "interaction_id", "This card is already answered and its answer is final");
                };
                if i.withdrawn {
                    return fail("withdrawn", "Its author withdrew this question");
                }
                validate_answer(options, option_id, inputs)?;
                validate_attachments(attachments)?;
                let a = Answer {
                    attachments: attachments.clone(),
                    option_id: option_id.clone(),
                    inputs: inputs.clone(),
                    responder,
                    at_revision: self.revision + 1,
                };
                let i = self.interactions.get_mut(interaction_id).unwrap();
                match &mut i.payload {
                    InteractionPayload::Question { answer, .. }
                    | InteractionPayload::ActionRequest { answer, .. } => *answer = Some(a),
                    _ => unreachable!(),
                };
                i.revision = self.revision + 1;
                if i.card.kind == CardKind::Propose && option_id == PROPOSAL_ACCEPTED {
                    self.start_proposal(interaction_id, created, effects);
                }
                let i = self.interactions.get_mut(interaction_id).unwrap();
                // D05: the answer reaches whoever holds the Task now, not the
                // session that asked and may since have been replaced.
                match (i.author.clone(), i.scope.clone()) {
                    _ if self.holds.contains_key(interaction_id) => {}
                    (Author::Agent { .. }, Scope::Task { task_id }) => {
                        self.notify(&task_id, Some(interaction_id.clone()), "Question answered", effects)?;
                    }
                    (Author::Orchestrator { .. }, _) => effects.push(DomainEffect::NotifyOrchestrator {
                        interaction_id: Some(interaction_id.clone()),
                        request_id: None,
                    }),
                    _ => {}
                }
            }
            Command::CreateDocument {
                task_id,
                document_id,
                title,
                revision_id,
                content_digest,
                content_reference,
                role,
            } => {
                self.assigned(actor, task_id)?;
                id(&document_id.0)?;
                id(&revision_id.0)?;
                nonempty(title)?;
                nonempty(role)?;
                digest(content_digest)?;
                nonempty(content_reference)?;
                if self.documents.contains_key(document_id)
                    || self.document_revisions.contains_key(revision_id)
                {
                    return fail("invalid_input", "Document or revision already exists");
                };
                self.documents.insert(
                    document_id.clone(),
                    Document {
                        id: document_id.clone(),
                        title: title.clone(),
                        current_revision: revision_id.clone(),
                    },
                );
                self.document_revisions.insert(
                    revision_id.clone(),
                    DocumentRevision {
                        id: revision_id.clone(),
                        document_id: document_id.clone(),
                        number: 1,
                        content_digest: content_digest.clone(),
                        content_reference: content_reference.clone(),
                    },
                );
                self.tasks
                    .get_mut(task_id)
                    .unwrap()
                    .documents
                    .insert(document_id.clone(), role.clone());
                self.document_reviews.push(DocumentReview {
                    task_id: task_id.clone(),
                    document_id: document_id.clone(),
                    decisions: vec![],
                });
                created.extend([document_id.0.clone(), revision_id.0.clone()]);
            }
            Command::LinkTaskDocument {
                task_id,
                document_id,
                role,
            } => {
                // A document can be shared across Tasks. Only a manager may grant
                // that cross-Task link; knowing a document ID is not authority.
                self.active(task_id)?;
                self.manager(actor)?;
                nonempty(role)?;
                if !self.documents.contains_key(document_id) {
                    return fail("not_found", "Document missing");
                };
                self.tasks
                    .get_mut(task_id)
                    .unwrap()
                    .documents
                    .insert(document_id.clone(), role.clone());
                if !self
                    .document_reviews
                    .iter()
                    .any(|r| &r.task_id == task_id && &r.document_id == document_id)
                {
                    self.document_reviews.push(DocumentReview {
                        task_id: task_id.clone(),
                        document_id: document_id.clone(),
                        decisions: vec![],
                    });
                }
            }
            Command::ReviseDocument {
                task_id,
                document_id,
                revision_id,
                content_digest,
                content_reference,
            } => {
                self.assigned(actor, task_id)?;
                if !self.task(task_id)?.documents.contains_key(document_id) {
                    return fail("forbidden", "Document not linked to Task");
                };
                id(&revision_id.0)?;
                digest(content_digest)?;
                nonempty(content_reference)?;
                if self.document_revisions.contains_key(revision_id) {
                    return fail("invalid_input", "Revision is immutable");
                };
                let doc = self
                    .documents
                    .get(document_id)
                    .ok_or_else(|| DomainError::new("not_found", "Document missing"))?;
                let number = self.document_revisions[&doc.current_revision]
                    .number
                    .checked_add(1)
                    .filter(|n| *n <= MAX_REVISION)
                    .ok_or_else(|| DomainError::new("invalid_input", "Revision overflow"))?;
                self.document_revisions.insert(
                    revision_id.clone(),
                    DocumentRevision {
                        id: revision_id.clone(),
                        document_id: document_id.clone(),
                        number,
                        content_digest: content_digest.clone(),
                        content_reference: content_reference.clone(),
                    },
                );
                self.documents
                    .get_mut(document_id)
                    .unwrap()
                    .current_revision = revision_id.clone();
                let affected: Vec<_> = self
                    .tasks
                    .values()
                    .filter(|t| t.documents.contains_key(document_id))
                    .map(|t| t.id.clone())
                    .collect();
                for t in affected {
                    self.invalidate(&t);
                }
                created.push(revision_id.0.clone());
            }
            Command::DecideDocumentReview {
                task_id,
                document_id,
                revision_id,
                accepted,
            } => {
                let u = user(actor)?;
                self.active(task_id)?;
                if !self.task(task_id)?.documents.contains_key(document_id) {
                    return fail("forbidden", "Document not linked to Task");
                };
                if !self
                    .documents
                    .get(document_id)
                    .is_some_and(|d| &d.current_revision == revision_id)
                {
                    return fail("acceptance_stale", "Document revision changed");
                };
                let presentation = self.task(task_id)?.final_presentation.clone().filter(|id| {
                    self.presentation(id).is_ok_and(|(_, _, targets)| {
                        targets.contains(&DeliveryTarget::DocumentRevision {
                            document_id: document_id.clone(),
                            revision_id: revision_id.clone(),
                        })
                    })
                });
                self.document_decision(task_id, document_id, revision_id, presentation.clone(), *accepted, &u)?;
                if !accepted {
                    self.invalidate(task_id);
                } else if let Some(presentation) = &presentation {
                    self.decided(presentation);
                    self.settle(presentation, &u);
                }
                self.notify(
                    task_id,
                    None,
                    if *accepted {
                        "The user accepted the document."
                    } else {
                        "The user requested changes to the document."
                    },
                    effects,
                )?;
            }
            Command::RegisterPr {
                task_id,
                pr_id,
                host,
                repository,
                number,
                head: sha,
                base,
                allowed_methods,
            } => {
                self.assigned(actor, task_id)?;
                id(&pr_id.0)?;
                canonical_pr(host, repository, *number)?;
                if !host.contains('.') {
                    return refuse(
                        "invalid_input",
                        "host",
                        &format!("host \"{host}\" is not a real host: use github.com (or your GitHub Enterprise host)"),
                    );
                }
                head(sha)?;
                nonempty(base)?;
                if allowed_methods.is_empty() {
                    return fail("invalid_input", "Merge methods required");
                };
                if let Some(existing) = self.task(task_id)?.pr_id.clone() {
                    let p = &self.pull_requests[&existing];
                    if p.repository != *repository || p.number != *number {
                        return fail("task_already_has_pr", "Task already owns a lifetime PR");
                    }
                    if existing == *pr_id
                        && p.host == *host
                        && p.head == *sha
                        && p.base == *base
                        && p.allowed_methods == *allowed_methods
                    {
                        return Ok(());
                    }
                    if p.state != PrState::Open {
                        return fail("task_already_has_pr", "Task already owns a lifetime PR");
                    }
                    let corrected = self.pull_requests.get_mut(&existing).unwrap();
                    corrected.host = host.clone();
                    corrected.head = sha.clone();
                    corrected.base = base.clone();
                    corrected.allowed_methods = allowed_methods.clone();
                    return Ok(());
                };
                if self.pull_requests.values().any(|p| {
                    p.id == *pr_id
                        || (p.host == *host && p.repository == *repository && p.number == *number)
                }) {
                    return fail("pr_owned_by_other_task", "PR is already registered");
                };
                let review = self
                    .task(task_id)?
                    .review_prs
                    .then(|| PullRequestReview { decisions: vec![] });
                self.pull_requests.insert(
                    pr_id.clone(),
                    PullRequest {
                        id: pr_id.clone(),
                        task_id: task_id.clone(),
                        host: host.clone(),
                        repository: repository.clone(),
                        number: *number,
                        head: sha.clone(),
                        base: base.clone(),
                        state: PrState::Open,
                        merged_head: None,
                        allowed_methods: allowed_methods.clone(),
                        review,
                    },
                );
                self.tasks.get_mut(task_id).unwrap().pr_id = Some(pr_id.clone());
                created.push(pr_id.0.clone());
            }
            Command::LinkInteractionSubject {
                interaction_id,
                subject,
            } => {
                let i = self.interaction(interaction_id)?;
                let Scope::Task { task_id } = &i.scope else {
                    return fail("invalid_input", "Subject link requires Task scope");
                };
                self.assigned(actor, task_id)?;
                if !matches!((&i.author,actor),(Author::Agent{agent_id:a},Actor::Agent{agent_id:b}) if a==b)
                {
                    return fail(
                        "forbidden",
                        "Only the Interaction author can link its subject",
                    );
                };
                self.subject(task_id, subject)?;
                if let Some(existing) = &i.subject {
                    if existing != subject {
                        return fail("subject_already_linked", "Interaction subject is immutable");
                    }
                    return Ok(());
                }
                let i = self.interactions.get_mut(interaction_id).unwrap();
                i.subject = Some(subject.clone());
                i.revision = self.revision + 1;
            }
            Command::PresentResult {
                attachments,
                task_id,
                interaction_id,
                body,
                targets,
                is_final,
                card,
            } => {
                self.assigned(actor, task_id)?;
                self.current_pr_targets(task_id, targets)?;
                self.targets(task_id, targets)?;
                if *is_final && self.blockers(task_id) {
                    let open = self.interactions.values()
                        .filter(|i| i.scope == (Scope::Task { task_id: task_id.clone() }) && i.blocking && self.open(i))
                        .map(|i| i.id.0.clone()).collect::<Vec<_>>().join(", ");
                    return refuse("blocking_open", "is_final", &format!("Card {open} still waits on the user: wait for its answer or withdraw it before the final delivery"));
                }
                let mut card = card.clone();
                if targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { pr_id, .. }
                    if self.pull_requests.get(pr_id).is_some_and(|p| p.state != PrState::Open)))
                {
                    card.kind = CardKind::Know;
                }
                let previous = self.task(task_id)?.final_presentation.clone()
                    .filter(|p| *is_final && self.interactions.get(p).is_some_and(|p| !self.accepted(p)));
                self.insert_interaction(
                    actor,
                    interaction_id,
                    &Scope::Task {
                        task_id: task_id.clone(),
                    },
                    &Recipient::User,
                    body,
                    false,
                    attachments,
                    InteractionPayload::Delivery {
                        targets: targets.clone(),
                        is_final: *is_final,
                        acceptance: None,
                        feedback: vec![],
                    },
                    &card,
                    Source::Delivery,
                )?;
                if let Some(previous) = previous {
                    self.retire(&previous);
                }
                if *is_final {
                    self.invalidate(task_id);
                    self.tasks.get_mut(task_id).unwrap().final_presentation =
                        Some(interaction_id.clone());
                    self.blocks_acyclic(interaction_id)?;
                }
                created.push(interaction_id.0.clone());
                effects.push(DomainEffect::NotifyUser {
                    task_id: Some(task_id.clone()),
                    interaction_id: interaction_id.clone(),
                });
            }
            Command::SubmitFeedback {
                attachments,
                presentation_id,
                targets,
                body,
                instruction,
            } => {
                let u = match (actor, instruction) {
                    (Actor::System, Instruction::Conflicts) => Feedback::ALONDRA.to_owned(),
                    _ => user(actor)?,
                };
                content(body, attachments)?;
                let (_, t, current) = self.presentation(presentation_id)?;
                if current != targets {
                    return fail("acceptance_stale", "Feedback targets changed");
                };
                let task = t.id.clone();
                self.invalidate(&task);
                let p = self.interactions.get_mut(presentation_id).unwrap();
                if let InteractionPayload::Delivery { feedback, .. } = &mut p.payload {
                    feedback.push(Feedback {
                        attachments: attachments.clone(),
                        user_id: u,
                        body: body.clone(),
                        at_revision: self.revision + 1,
                        instruction: *instruction,
                    });
                }
                p.revision = self.revision + 1;
                self.notify(&task, Some(presentation_id.clone()), &instruction.message(body), effects)?;
            }
            Command::AcceptDelivery {
                presentation_id,
                targets,
            } => {
                let u = user(actor)?;
                let (_, _, current) = self.presentation(presentation_id)?;
                if current != targets {
                    return fail("acceptance_stale", "Delivery targets changed");
                };
                self.unblocked(presentation_id)?;
                for target in targets {
                    if let DeliveryTarget::PullRequest { pr_id, head } = target {
                        let pr = &self.pull_requests[pr_id];
                        let already_merged =
                            pr.state == PrState::Merged && pr.merged_head.as_ref() == Some(head);
                        let locally_authorized = self.merge_authorizations.values().any(|a| {
                            a.valid
                                && a.presentation_id == *presentation_id
                                && a.pr_id == *pr_id
                                && a.head == *head
                        });
                        if !already_merged && !locally_authorized {
                            return fail(
                                "acceptance_missing",
                                "Open PR needs explicit merge authorization",
                            );
                        }
                    }
                }
                self.accept(presentation_id, &u)?;
            }
            Command::AuthorizeMerge {
                authorization_id,
                presentation_id,
                pr_id,
                expected_head,
                method,
            } => {
                let u = user(actor)?;
                id(&authorization_id.0)?;
                let (_, t, targets) = self.presentation(presentation_id)?;
                self.unblocked(presentation_id)?;
                if !targets.contains(&DeliveryTarget::PullRequest {
                    pr_id: pr_id.clone(),
                    head: expected_head.clone(),
                }) {
                    return fail("stale_head", "PR/head not in current delivery");
                };
                let task = t.id.clone();
                let pr = &self.pull_requests[pr_id];
                if pr.state != PrState::Open {
                    return fail(
                        "invalid_action_for_kind",
                        "Only an open PR can be authorized",
                    );
                };
                if !pr.allowed_methods.contains(method) {
                    return fail("invalid_input", "Merge method not allowed");
                };
                if self.merge_authorizations.contains_key(authorization_id) {
                    return fail("invalid_input", "Authorization ID already exists");
                };
                if self.merge_authorizations.values().any(|a| {
                    a.pr_id == *pr_id
                        && matches!(
                            a.attempt,
                            MergeAttemptState::Claimed | MergeAttemptState::Uncertain
                        )
                }) {
                    return fail(
                        "delivery_uncertain",
                        "Reconcile existing merge attempt before authorizing again",
                    );
                };
                for a in self
                    .merge_authorizations
                    .values_mut()
                    .filter(|a| a.pr_id == *pr_id)
                {
                    a.valid = false;
                }
                self.merge_authorizations.insert(
                    authorization_id.clone(),
                    MergeAuthorization {
                        id: authorization_id.clone(),
                        task_id: task.clone(),
                        pr_id: pr_id.clone(),
                        presentation_id: presentation_id.clone(),
                        head: expected_head.clone(),
                        method: *method,
                        user_id: u.clone(),
                        valid: true,
                        attempt: MergeAttemptState::Unclaimed,
                        claimed_by: None,
                        at_revision: self.revision + 1,
                    },
                );
                if let Some(r) = &mut self.pull_requests.get_mut(pr_id).unwrap().review {
                    r.decisions.push(PrDecision {
                        presentation_id: presentation_id.clone(),
                        head: expected_head.clone(),
                        accepted: true,
                        user_id: u.clone(),
                        at_revision: self.revision + 1,
                    });
                }
                self.decided(presentation_id);
                self.settle(presentation_id, &u);
                self.notify(
                    &task,
                    Some(presentation_id.clone()),
                    "The user authorized merging the presented pull request.",
                    effects,
                )?;
                created.push(authorization_id.0.clone());
            }
            Command::ClaimMergeAuthorization {
                authorization_id,
                remote_head,
                remote_base,
                base_verified,
            } => {
                let a = self
                    .merge_authorizations
                    .get(authorization_id)
                    .ok_or_else(|| DomainError::new("not_found", "Authorization missing"))?
                    .clone();
                self.assigned(actor, &a.task_id)?;
                self.presentation(&a.presentation_id)?;
                if !a.valid {
                    return fail("acceptance_stale", "Authorization was invalidated");
                };
                if a.attempt != MergeAttemptState::Unclaimed {
                    return fail(
                        "delivery_uncertain",
                        "Merge attempt already claimed; reconcile instead of retry",
                    );
                };
                let pr = &self.pull_requests[&a.pr_id];
                if pr.state != PrState::Open || pr.head != a.head || remote_head != &a.head {
                    return fail("stale_head", "Approved and remote head differ");
                };
                if !base_verified || remote_base != &pr.base {
                    return fail("stack_not_ready", "Remote base must be verified");
                };
                let t = self.task(&a.task_id)?;
                self.ready_to_start(t)?;
                if let Some(s) = &t.stack {
                    let upstream = self.pull_requests.get(&s.pr_id).ok_or_else(|| {
                        DomainError::new("stack_not_ready", "Upstream PR missing")
                    })?;
                    if upstream.state != PrState::Merged
                        || upstream.merged_head.as_ref() != Some(&s.head)
                        || upstream.base != pr.base
                    {
                        return fail(
                            "stack_not_ready",
                            "Upstream merge and retarget must be confirmed",
                        );
                    }
                }
                let a = self.merge_authorizations.get_mut(authorization_id).unwrap();
                a.attempt = MergeAttemptState::Claimed;
                a.claimed_by = self.tasks[&a.task_id].agent_id.clone();
            }
            Command::ObserveMergeAttempt {
                authorization_id,
                state,
            } => {
                system(actor)?;
                let a = self
                    .merge_authorizations
                    .get_mut(authorization_id)
                    .ok_or_else(|| DomainError::new("not_found", "Authorization missing"))?;
                if !matches!(
                    state,
                    MergeAttemptState::Uncertain | MergeAttemptState::Failed
                ) || !matches!(
                    a.attempt,
                    MergeAttemptState::Claimed | MergeAttemptState::Uncertain
                ) {
                    return fail("invalid_input", "Invalid merge attempt transition");
                };
                a.attempt = *state;
            }
            Command::ObservePrState {
                pr_id,
                head: sha,
                base,
                state,
                merged_head,
            } => {
                system(actor)?;
                head(sha)?;
                nonempty(base)?;
                if (*state == PrState::Merged) != merged_head.is_some() {
                    return fail(
                        "invalid_input",
                        "Merged observation requires exact merged head",
                    );
                };
                if let Some(h) = merged_head {
                    head(h)?;
                }
                let p = self
                    .pull_requests
                    .get(pr_id)
                    .ok_or_else(|| DomainError::new("not_found", "PR missing"))?;
                if p.state == PrState::Merged
                    && (*state != PrState::Merged || p.merged_head != *merged_head)
                {
                    return fail(
                        "invalid_input",
                        "Confirmed merge cannot be undone by observation",
                    );
                };
                let task = p.task_id.clone();
                if p.head != *sha {
                    self.invalidate(&task);
                }
                let p = self.pull_requests.get_mut(pr_id).unwrap();
                p.head = sha.clone();
                p.base = base.clone();
                p.state = *state;
                p.merged_head = merged_head.clone();
                if *state == PrState::Merged {
                    for a in self.merge_authorizations.values_mut().filter(|a| {
                        a.pr_id == *pr_id
                            && Some(&a.head) == merged_head.as_ref()
                            && matches!(
                                a.attempt,
                                MergeAttemptState::Claimed | MergeAttemptState::Uncertain
                            )
                    }) {
                        a.attempt = MergeAttemptState::Confirmed;
                    }
                }
            }
            Command::PublishStackReadiness {
                task_id,
                presentation_id,
                pr_id,
                head: sha,
                ready,
            } => {
                self.assigned(actor, task_id)?;
                let (_, t, targets) = self.presentation(presentation_id)?;
                if &t.id != task_id
                    || !targets.contains(&DeliveryTarget::PullRequest {
                        pr_id: pr_id.clone(),
                        head: sha.clone(),
                    })
                {
                    return fail(
                        "stack_not_ready",
                        "Stack must identify this Task's current PR delivery",
                    );
                };
                self.stack_readiness.insert(
                    task_id.clone(),
                    StackReadiness {
                        task_id: task_id.clone(),
                        presentation_id: presentation_id.clone(),
                        pr_id: pr_id.clone(),
                        head: sha.clone(),
                        ready: *ready,
                    },
                );
            }
            Command::CompleteTask {
                task_id,
                presentation_id,
            } => {
                self.assigned(actor, task_id)?;
                let (p, t, targets) = self.presentation(presentation_id)?;
                if &t.id != task_id {
                    return fail("forbidden", "Presentation belongs to another Task");
                };
                let merged_by_agent = self.requests.get(&t.request_id).is_some_and(|r| r.auto_merge)
                    && matches!(&p.payload, InteractionPayload::Delivery { acceptance: None, feedback, .. } if feedback.is_empty())
                    && targets.iter().any(|x| matches!(x, DeliveryTarget::PullRequest { .. }));
                if !self.accepted(p) && !merged_by_agent {
                    return fail(
                        if matches!(&p.payload, InteractionPayload::Delivery { acceptance: Some(a), feedback, .. }
                            if !a.valid && feedback.last().is_some_and(|f| f.at_revision > a.at_revision))
                        {
                            "acceptance_stale"
                        } else {
                            "acceptance_missing"
                        },
                        "Current delivery needs valid user acceptance",
                    );
                };
                self.completable(t, targets)?;
                self.tasks.get_mut(task_id).unwrap().state = TaskState::Done;
            }
            Command::CancelTask { task_id } => {
                self.manager(actor)?;
                self.active(task_id)?;
                self.invalidate(task_id);
                self.tasks.get_mut(task_id).unwrap().state = TaskState::Cancelled;
            }
            Command::ReopenTask { task_id } => {
                self.manager(actor)?;
                let task = self.task(task_id)?;
                if !task.state.terminal() {
                    return fail("task_not_active", "Only terminal Tasks can be reopened");
                };
                if !self
                    .requests
                    .get(&task.request_id)
                    .is_some_and(|r| !r.closed)
                {
                    return fail("not_found", "Open Request required to reopen Task");
                }
                let released = task.agent_id.as_ref().is_some_and(|a| self.busy_with(a, task_id).is_some());
                self.invalidate(task_id);
                let t = self.tasks.get_mut(task_id).unwrap();
                t.state = TaskState::Running;
                t.started = true;
                t.archived = false;
                if released {
                    t.agent_id = None;
                }
            }
            Command::ArchiveTask { task_id, archived } => {
                user(actor)?;
                let task = self.task(task_id)?;
                if *archived && !task.state.terminal() {
                    return fail("task_not_active", "Only terminal Tasks can be archived");
                };
                self.tasks.get_mut(task_id).unwrap().archived = *archived;
            }
        }
        Ok(())
    }
    fn subject(&self, task: &TaskId, subject: &InteractionSubject) -> Result<()> {
        match subject {
            InteractionSubject::PullRequest { pr_id } => {
                if !self
                    .pull_requests
                    .get(pr_id)
                    .is_some_and(|p| &p.task_id == task)
                {
                    return fail("forbidden", "PR belongs to another Task");
                }
            }
            InteractionSubject::DocumentReview {
                document_id,
                revision_id,
            } => {
                if !self.task(task)?.documents.contains_key(document_id)
                    || !self
                        .document_revisions
                        .get(revision_id)
                        .is_some_and(|r| &r.document_id == document_id)
                {
                    return fail("forbidden", "Document revision is not linked to this Task");
                }
            }
        }
        Ok(())
    }
    fn document_decision(
        &mut self,
        task: &TaskId,
        doc: &DocumentId,
        rev: &DocumentRevisionId,
        presentation: Option<InteractionId>,
        accepted: bool,
        u: &str,
    ) -> Result<()> {
        let r = self
            .document_reviews
            .iter_mut()
            .find(|r| &r.task_id == task && &r.document_id == doc)
            .ok_or_else(|| DomainError::new("not_found", "Document review missing"))?;
        r.decisions.push(DocumentDecision {
            revision_id: rev.clone(),
            presentation_id: presentation,
            accepted,
            user_id: u.into(),
            at_revision: self.revision + 1,
        });
        Ok(())
    }
    fn accept(&mut self, presentation: &InteractionId, u: &str) -> Result<()> {
        let (_, t, targets) = self.presentation(presentation)?;
        let task = t.id.clone();
        let targets = targets.clone();
        for target in targets {
            if let DeliveryTarget::DocumentRevision {
                document_id,
                revision_id,
            } = target
            {
                self.document_decision(
                    &task,
                    &document_id,
                    &revision_id,
                    Some(presentation.clone()),
                    true,
                    u,
                )?;
            }
        }
        self.record_acceptance(presentation, u);
        Ok(())
    }
    fn record_acceptance(&mut self, presentation: &InteractionId, u: &str) {
        let p = self.interactions.get_mut(presentation).unwrap();
        if let InteractionPayload::Delivery { acceptance, .. } = &mut p.payload {
            *acceptance = Some(Acceptance {
                user_id: u.into(),
                at_revision: self.revision + 1,
                valid: true,
            });
        }
        p.revision = self.revision + 1;
    }
    /// Structural validation is also required after loading relational rows.
    /// Historical presentations retain old document revisions and heads deliberately.
    pub fn validate(&self) -> Result<()> {
        id(&self.data_epoch.0)?;
        if self.revision > MAX_REVISION || self.orchestrator_generation > MAX_REVISION {
            return fail("invalid_input", "Unsafe revision or generation");
        };
        self.schedule.validate()?;
        for run in self.schedule.runs.values() {
            if let (Some(request_id), Some(task_id)) = (&run.request_id, &run.task_id) {
                if !self.requests.contains_key(request_id)
                    || !self
                        .tasks
                        .get(task_id)
                        .is_some_and(|task| &task.request_id == request_id)
                {
                    return fail("invalid_input", "Schedule run has mismatched Request or Task");
                }
            }
        }
        for (k, r) in &self.requests {
            id(&k.0)?;
            if k != &r.id {
                return fail("invalid_input", "Request identity mismatch");
            };
            nonempty(&r.body)?;
        }
        for (k, a) in &self.agents {
            id(&k.0)?;
            if k != &a.id {
                return fail("invalid_input", "Agent identity mismatch");
            };
            nonempty(&a.name)?;
        }
        for (k, t) in &self.tasks {
            id(&k.0)?;
            nonempty(&t.title)?;
            if k != &t.id
                || !self.requests.contains_key(&t.request_id)
                || t.revision > self.revision
                || t.agent_id
                    .as_ref()
                    .is_some_and(|a| !self.agents.contains_key(a))
            {
                return fail("invalid_input", "Invalid Task relation or revision");
            };
            if t.archived && !t.state.terminal() {
                return fail("invalid_input", "Archived Task is still active");
            };
            for (d, role) in &t.documents {
                nonempty(role)?;
                if !self.documents.contains_key(d) {
                    return fail("not_found", "Linked document missing");
                }
            }
            if let Some(p) = &t.pr_id {
                if !self.pull_requests.get(p).is_some_and(|p| &p.task_id == k) {
                    return fail("invalid_input", "Task PR ownership mismatch");
                }
            }
            if let Some(i) = &t.final_presentation {
                let i = self.interaction(i)?;
                if i.scope != (Scope::Task { task_id: k.clone() })
                    || !matches!(
                        i.payload,
                        InteractionPayload::Delivery { is_final: true, .. }
                    )
                {
                    return fail(
                        "invalid_input",
                        "Final presentation must be a final delivery of the same Task",
                    );
                }
            }
            if let Some(s) = &t.stack {
                let up = self.task(&s.upstream_task)?;
                if up.pr_id.as_ref()!=Some(&s.pr_id) || !self.interactions.get(&s.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:s.upstream_task.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:s.pr_id.clone(),head:s.head.clone()}))){return fail("invalid_input","Stack references wrong upstream presentation")}
            }
        }
        self.dependency_graph()?;
        let mut numbers = BTreeSet::new();
        for (k, r) in &self.document_revisions {
            id(&k.0)?;
            if k != &r.id
                || !self.documents.contains_key(&r.document_id)
                || r.number == 0
                || r.number > MAX_REVISION
                || !numbers.insert((r.document_id.clone(), r.number))
            {
                return fail("invalid_input", "Invalid document revision identity");
            };
            digest(&r.content_digest)?;
            nonempty(&r.content_reference)?;
        }
        for (k, d) in &self.documents {
            id(&k.0)?;
            nonempty(&d.title)?;
            if k != &d.id
                || !self
                    .document_revisions
                    .get(&d.current_revision)
                    .is_some_and(|r| &r.document_id == k)
            {
                return fail("invalid_input", "Document current revision mismatch");
            }
        }
        let mut reviews = BTreeSet::new();
        for r in &self.document_reviews {
            if !reviews.insert((r.task_id.clone(), r.document_id.clone()))
                || !self
                    .task(&r.task_id)?
                    .documents
                    .contains_key(&r.document_id)
            {
                return fail("invalid_input", "Invalid or duplicate document review");
            };
            for d in &r.decisions {
                if !self
                    .document_revisions
                    .get(&d.revision_id)
                    .is_some_and(|v| v.document_id == r.document_id)
                    || d.at_revision > self.revision
                {
                    return fail("invalid_input", "Document decision revision mismatch");
                };
                id(&d.user_id)?;
                if let Some(p) = &d.presentation_id {
                    let i = self.interaction(p)?;
                    if i.scope
                        != (Scope::Task {
                            task_id: r.task_id.clone(),
                        })
                        || !matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::DocumentRevision{document_id:r.document_id.clone(),revision_id:d.revision_id.clone()}))
                    {
                        return fail("invalid_input", "Document decision presentation mismatch");
                    }
                }
            }
        }
        for t in self.tasks.values() {
            for d in t.documents.keys() {
                if !reviews.contains(&(t.id.clone(), d.clone())) {
                    return fail("invalid_input", "Linked document lacks review");
                }
            }
        }
        let mut identities = BTreeSet::new();
        let mut owners = BTreeSet::new();
        for (k, p) in &self.pull_requests {
            id(&k.0)?;
            canonical_pr(&p.host, &p.repository, p.number)?;
            head(&p.head)?;
            nonempty(&p.base)?;
            let t = self.task(&p.task_id)?;
            if k != &p.id
                || t.pr_id.as_ref() != Some(k)
                || !owners.insert(p.task_id.clone())
                || !identities.insert((&p.host, &p.repository, p.number))
                || p.review.is_some() != t.review_prs
                || p.allowed_methods.is_empty()
            {
                return fail(
                    "invalid_input",
                    "Invalid lifetime PR identity or review policy",
                );
            };
            if (p.state == PrState::Merged) != p.merged_head.is_some() {
                return fail("invalid_input", "Merge state lacks evidence");
            };
            if let Some(h) = &p.merged_head {
                head(h)?;
            }
            if let Some(r) = &p.review {
                for d in &r.decisions {
                    id(&d.user_id)?;
                    head(&d.head)?;
                    if d.at_revision>self.revision || !self.interactions.get(&d.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:p.task_id.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:k.clone(),head:d.head.clone()}))){return fail("invalid_input","PR review decision does not match presentation")}
                }
            }
        }
        for (k, i) in &self.interactions {
            id(&k.0)?;
            content(&i.body, &i.attachments)?;
            nonempty(&i.card.need)?;
            if k != &i.id || i.revision > self.revision || i.order > self.revision {
                return fail("invalid_input", "Invalid Interaction identity/revision");
            };
            self.validate_scope(&i.scope)?;
            self.recipient(&i.recipient)?;
            match &i.author {
                Author::Agent { agent_id } => {
                    if !self.agents.contains_key(agent_id)
                        || !(matches!(i.scope, Scope::Task { .. })
                            || matches!(i.payload, InteractionPayload::Information { .. }))
                    {
                        return fail("invalid_input", "Agent authored Interaction without Task");
                    }
                }
                Author::Orchestrator { generation }
                    if *generation > self.orchestrator_generation =>
                {
                    return fail("stale_generation", "Future author generation");
                }
                _ => {}
            };
            if let Some(s) = &i.subject {
                let Scope::Task { task_id } = &i.scope else {
                    return fail("invalid_input", "Subject requires Task scope");
                };
                self.subject(task_id, s)?;
            }
            match &i.payload {
                InteractionPayload::Information { read_by } => {
                    if i.blocking {
                        return fail("invalid_input", "Information cannot block");
                    };
                    for u in read_by {
                        id(u)?;
                    }
                }
                InteractionPayload::Question { options, answer }
                | InteractionPayload::ActionRequest { options, answer } => {
                    validate_options(options)?;
                    if let Some(a) = answer {
                        validate_attachments(&a.attachments)?;
                        validate_answer(options, &a.option_id, &a.inputs)?;
                        id(&a.responder)?;
                        if a.at_revision > self.revision {
                            return fail("invalid_input", "Answer revision from future");
                        }
                    }
                }
                InteractionPayload::Delivery {
                    targets,
                    acceptance,
                    feedback,
                    ..
                } => {
                    let Scope::Task { task_id } = &i.scope else {
                        return fail("invalid_input", "Delivery must have Task scope");
                    };
                    if i.blocking
                        || i.recipient != Recipient::User
                        || targets.is_empty()
                        || targets.len() > 64
                    {
                        return fail("invalid_input", "Invalid delivery payload");
                    };
                    let t = self.task(task_id)?;
                    let mut seen = BTreeSet::new();
                    for target in targets {
                        if !seen.insert(serde_json::to_string(target).unwrap()) {
                            return fail("invalid_input", "Duplicate target");
                        };
                        match target {
                            DeliveryTarget::Text { text } => nonempty(text)?,
                            DeliveryTarget::DocumentRevision {
                                document_id,
                                revision_id,
                            } => {
                                if !t.documents.contains_key(document_id)
                                    || !self
                                        .document_revisions
                                        .get(revision_id)
                                        .is_some_and(|r| &r.document_id == document_id)
                                {
                                    return fail(
                                        "invalid_input",
                                        "Historical document target mismatch",
                                    );
                                }
                            }
                            DeliveryTarget::PullRequest { pr_id, head: sha } => {
                                head(sha)?;
                                if !self
                                    .pull_requests
                                    .get(pr_id)
                                    .is_some_and(|p| &p.task_id == task_id)
                                {
                                    return fail("invalid_input", "Historical PR target mismatch");
                                }
                            }
                        }
                    }
                    if let Some(a) = acceptance {
                        id(&a.user_id)?;
                        if a.at_revision > self.revision {
                            return fail("invalid_input", "Acceptance revision from future");
                        };
                        if a.valid && t.final_presentation.as_ref() != Some(k) {
                            return fail(
                                "invalid_input",
                                "Noncurrent delivery has valid acceptance",
                            );
                        }
                    }
                    for f in feedback {
                        id(&f.user_id)?;
                        content(&f.body, &f.attachments)?;
                        if f.at_revision > self.revision {
                            return fail("invalid_input", "Feedback revision from future");
                        }
                    }
                }
            }
        }
        for (k, a) in &self.merge_authorizations {
            id(&k.0)?;
            id(&a.user_id)?;
            head(&a.head)?;
            if k!=&a.id || a.at_revision>self.revision || !self.pull_requests.get(&a.pr_id).is_some_and(|p|p.task_id==a.task_id) || !self.interactions.get(&a.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:a.task_id.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:a.pr_id.clone(),head:a.head.clone()}))){return fail("invalid_input","Merge authorization relation mismatch")};
            if let Some(agent) = &a.claimed_by {
                if !self.agents.contains_key(agent) {
                    return fail("not_found", "Merge attempt Agent missing");
                }
            }
            if matches!(
                a.attempt,
                MergeAttemptState::Claimed
                    | MergeAttemptState::Uncertain
                    | MergeAttemptState::Confirmed
                    | MergeAttemptState::Failed
            ) && a.claimed_by.is_none()
            {
                return fail("invalid_input", "Merge attempt lacks claimed Agent");
            }
        }
        for (k, s) in &self.stack_readiness {
            if k!=&s.task_id || !self.pull_requests.get(&s.pr_id).is_some_and(|p|p.task_id==s.task_id) || !self.interactions.get(&s.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:k.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:s.pr_id.clone(),head:s.head.clone()}))){return fail("invalid_input","Invalid stack readiness relation")}
        }
        Ok(())
    }
}
fn validate_options(options: &[QuestionOption]) -> Result<()> {
    if options.len() > 32 {
        return fail(
            "invalid_input",
            "A question supports at most 32 stable options",
        );
    };
    let mut ids = BTreeSet::new();
    for option in options {
        id(&option.id)?;
        nonempty(&option.label)?;
        if let Some(description) = &option.description {
            nonempty(description)?;
        }
        if !ids.insert(&option.id) {
            return fail("invalid_input", "Duplicate option ID");
        };
        if option.inputs.len() > 32 {
            return fail("invalid_input", "Too many input fields");
        };
        let mut fields = BTreeSet::new();
        for input in &option.inputs {
            id(&input.id)?;
            if !fields.insert(&input.id) {
                return fail("invalid_input", "Duplicate input ID");
            }
        }
    }
    Ok(())
}
fn validate_answer(
    options: &[QuestionOption],
    option: &str,
    inputs: &BTreeMap<String, InputValue>,
) -> Result<()> {
    if options.is_empty() || option.is_empty() {
        if !option.is_empty()
            || inputs.len() != 1
            || !matches!(inputs.get("text"), Some(InputValue::Text(text)) if !text.trim().is_empty() && text.len() <= 1_048_576)
        {
            return fail(
                "invalid_input",
                "Free response requires empty option_id and nonempty text input",
            );
        }
        return Ok(());
    }
    let spec = options
        .iter()
        .find(|o| o.id == option)
        .ok_or_else(|| DomainError::new("invalid_input", "Unknown option ID"))?;
    for k in inputs.keys() {
        if !spec.inputs.iter().any(|s| &s.id == k) {
            return fail("invalid_input", "Unknown answer field");
        }
    }
    for s in &spec.inputs {
        match inputs.get(&s.id) {
            None if s.required => return fail("invalid_input", "Required answer field missing"),
            None => {}
            Some(v) => {
                let valid = match (s.kind, v) {
                    (InputKind::Text, InputValue::Text(text)) => {
                        text.len() <= 1_048_576 && (!s.required || !text.trim().is_empty())
                    }
                    (InputKind::Boolean, InputValue::Boolean(_)) => true,
                    (InputKind::Integer, InputValue::Integer(n)) => {
                        n.unsigned_abs() <= MAX_REVISION
                    }
                    _ => false,
                };
                if !valid {
                    return fail("invalid_input", "Answer field type or value invalid");
                }
            }
        }
    }
    Ok(())
}
fn canonical_pr(host: &str, repo: &str, number: u64) -> Result<()> {
    if number == 0
        || number > MAX_REVISION
        || host.is_empty()
        || host != host.to_ascii_lowercase()
        || !host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        || repo != repo.to_ascii_lowercase()
        || repo.split('/').count() != 2
        || repo.split('/').any(|s| {
            s.is_empty()
                || s == "."
                || s == ".."
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        })
    {
        fail(
            "invalid_input",
            "Canonical lowercase host/owner/repository and positive PR number required",
        )
    } else {
        Ok(())
    }
}

impl AttachmentRef {
    /// The bounds every command puts on the files it carries.
    pub fn validate_list(attachments: &[AttachmentRef]) -> std::result::Result<(), DomainError> {
        validate_attachments(attachments)
    }
}
fn validate_attachments(attachments: &[AttachmentRef]) -> Result<()> {
    if attachments.len() > 32 {
        return fail("payload_too_large", "Too many attachments");
    }
    let mut seen = BTreeSet::new();
    let mut bytes = 0u64;
    for attachment in attachments {
        id(&attachment.id)?;
        nonempty(&attachment.name)?;
        nonempty(&attachment.content_reference)?;
        digest(&attachment.content_digest)?;
        if attachment.name.len() > 1024
            || attachment.content_reference.len() > 8192
            || attachment.name.chars().any(char::is_control)
            || !seen.insert(&attachment.id)
        {
            return fail("invalid_input", "Invalid or duplicate attachment reference");
        }
        bytes = bytes
            .checked_add(attachment.bytes)
            .ok_or_else(|| DomainError::new("payload_too_large", "Attachment size overflow"))?;
        if attachment.bytes > 64 * 1024 * 1024 || bytes > 128 * 1024 * 1024 {
            return fail("payload_too_large", "Attachment content exceeds limits");
        }
    }
    Ok(())
}
fn content(body: &str, attachments: &[AttachmentRef]) -> Result<()> {
    validate_attachments(attachments)?;
    if body.len() > 1_048_576 || body.trim().is_empty() && attachments.is_empty() {
        return fail(
            "invalid_input",
            "Content or attachment required within limits",
        );
    }
    Ok(())
}
