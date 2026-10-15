//! What every screen reads of the domain, decided once for each committed revision: who holds
//! what back, what each task asks of the user and what its node does, and Waiting for You in its
//! order with the buttons of every card in it. Built with the same rules the state answers on
//! demand, so a screen reading the index and one asking the state never disagree.
use super::*;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// What a task shows, from the domain alone: the client's language and the live data of its node
/// (what it is doing, a pause the machine imposed) are the screen's to add.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskReading {
    pub attention: Attention,
    pub activity: Option<TaskActivity>,
    pub handed_back: Option<Instruction>,
    pub merge_failure: Option<(u64, String)>,
    /// The open tasks that hold it back, by id.
    pub open_blockers: Vec<TaskId>,
    /// The open tasks it still waits for.
    pub open_dependencies: Vec<TaskId>,
    /// Its cards in Waiting for You, in the queue's order.
    pub waiting: Vec<InteractionId>,
    /// It waits for an answer, an action or a review: a card only to be read never holds it.
    pub waits_for_user: bool,
    /// All it waits for is the review of its delivery.
    pub waits_for_review: bool,
}

/// What a node shows, from the domain alone.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeReading {
    pub phase: NodePhase,
    /// Its tasks that are not archived, by id.
    pub tasks: Vec<TaskId>,
    /// The task it works for: its latest open one, or else its latest.
    pub task: Option<TaskId>,
    /// It merges a PR Alondra authorized and that is still open.
    pub merging: bool,
    /// The cards it wrote that wait for the user, in the queue's order.
    pub waiting: Vec<InteractionId>,
    /// What it said it still does after its task's PR merged.
    pub post_merge_work: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadIndex {
    data_epoch: DataEpoch,
    revision: Revision,
    tasks_by_request: HashMap<RequestId, Vec<TaskId>>,
    cards_by_task: HashMap<TaskId, Vec<InteractionId>>,
    cards_by_node: HashMap<AgentId, Vec<InteractionId>>,
    cards_by_pr: HashMap<PrId, Vec<InteractionId>>,
    cards_by_document_revision: HashMap<DocumentRevisionId, Vec<InteractionId>>,
    blocked_by: HashMap<InteractionId, Vec<InteractionId>>,
    blocks: HashMap<InteractionId, Vec<InteractionId>>,
    tasks: HashMap<TaskId, TaskReading>,
    nodes: HashMap<AgentId, NodeReading>,
    acting: Vec<(TaskId, CardRef)>,
    queue: Vec<InteractionId>,
    waiting: Vec<InboxItem>,
    waiting_at: HashMap<InteractionId, usize>,
    counts: InboxCounts,
}

impl ReadIndex {
    pub fn build(state: &DomainState) -> Self {
        Builder::new(state).build()
    }
    pub fn data_epoch(&self) -> &DataEpoch {
        &self.data_epoch
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    /// Whether it was built from this state's revision.
    pub fn describes(&self, state: &DomainState) -> bool {
        self.data_epoch == state.data_epoch && self.revision == state.revision
    }
    pub fn tasks_of_request(&self, request: &RequestId) -> &[TaskId] {
        self.tasks_by_request.get(request).map_or(&[], Vec::as_slice)
    }
    /// The task a node works for.
    pub fn task_of_node(&self, node: &AgentId) -> Option<&TaskId> {
        self.nodes.get(node)?.task.as_ref()
    }
    /// Every card of a task, by id.
    pub fn cards_of_task(&self, task: &TaskId) -> &[InteractionId] {
        self.cards_by_task.get(task).map_or(&[], Vec::as_slice)
    }
    /// Every card a node wrote, by id.
    pub fn cards_of_node(&self, node: &AgentId) -> &[InteractionId] {
        self.cards_by_node.get(node).map_or(&[], Vec::as_slice)
    }
    /// Every card that delivers or is about a PR, by id.
    pub fn cards_of_pr(&self, pr: &PrId) -> &[InteractionId] {
        self.cards_by_pr.get(pr).map_or(&[], Vec::as_slice)
    }
    /// Every card that delivers or reviews a document revision, by id.
    pub fn cards_of_document_revision(&self, revision: &DocumentRevisionId) -> &[InteractionId] {
        self.cards_by_document_revision.get(revision).map_or(&[], Vec::as_slice)
    }
    /// What holds a delivery back: the open cards in front of it, oldest first, then the delivery
    /// of the PR it is stacked on while that one still waits.
    pub fn blocked_by(&self, delivery: &InteractionId) -> &[InteractionId] {
        self.blocked_by.get(delivery).map_or(&[], Vec::as_slice)
    }
    /// The deliveries an open card holds back.
    pub fn blocks(&self, card: &InteractionId) -> &[InteractionId] {
        self.blocks.get(card).map_or(&[], Vec::as_slice)
    }
    pub fn task(&self, task: &TaskId) -> Option<&TaskReading> {
        self.tasks.get(task)
    }
    pub fn node(&self, node: &AgentId) -> Option<&NodeReading> {
        self.nodes.get(node)
    }
    /// The card the user can act on for each task that has one, by task.
    pub fn acting(&self) -> &[(TaskId, CardRef)] {
        &self.acting
    }
    /// Every card of the user's inbox, in the order Waiting for You lists them.
    pub fn queue(&self) -> &[InteractionId] {
        &self.queue
    }
    /// Waiting for You: the cards that wait for the user, in order, each with its buttons.
    pub fn waiting(&self) -> &[InboxItem] {
        &self.waiting
    }
    pub fn waiting_card(&self, card: &InteractionId) -> Option<&InboxItem> {
        self.waiting.get(*self.waiting_at.get(card)?)
    }
    pub fn counts(&self) -> &InboxCounts {
        &self.counts
    }
}

/// A committed state and the index built from it: one revision of one epoch, never two.
#[derive(Clone, Debug)]
pub struct ReadSnapshot {
    state: Arc<DomainState>,
    index: Arc<ReadIndex>,
}

impl ReadSnapshot {
    pub fn build(state: Arc<DomainState>) -> Self {
        let index = Arc::new(ReadIndex::build(&state));
        Self { state, index }
    }
    /// Pairs a state with an index built before, refusing one built from another revision.
    pub fn pair(state: Arc<DomainState>, index: Arc<ReadIndex>) -> std::result::Result<Self, DomainError> {
        if !index.describes(&state) {
            let mut error = DomainError::new(
                "stale_revision",
                &format!(
                    "The read index describes revision {} of {}, the state is revision {} of {}",
                    index.revision, index.data_epoch.0, state.revision, state.data_epoch.0
                ),
            );
            error.current_revision = Some(state.revision);
            return Err(error);
        }
        Ok(Self { state, index })
    }
    pub fn state(&self) -> &Arc<DomainState> {
        &self.state
    }
    pub fn index(&self) -> &Arc<ReadIndex> {
        &self.index
    }
}

impl Holds for Builder<'_> {
    fn first_blocker(&self, task: &TaskId) -> Option<TaskId> {
        self.open_blockers.get(task).and_then(|b| b.first()).cloned()
    }
    fn first_card(&self, delivery: &InteractionId) -> Option<InteractionId> {
        self.cards_before.get(delivery).and_then(|c| c.first()).cloned()
    }
}

struct Builder<'a> {
    state: &'a DomainState,
    open_blockers: HashMap<&'a TaskId, Vec<TaskId>>,
    /// The open cards in front of each delivery, without the upstream delivery of its stack.
    cards_before: HashMap<&'a InteractionId, Vec<InteractionId>>,
    blocks: HashMap<InteractionId, Vec<InteractionId>>,
}

impl<'a> Builder<'a> {
    fn new(state: &'a DomainState) -> Self {
        Self { state, open_blockers: HashMap::new(), cards_before: HashMap::new(), blocks: HashMap::new() }
    }

    fn build(mut self) -> ReadIndex {
        let state = self.state;
        let mut index = ReadIndex { data_epoch: state.data_epoch.clone(), revision: state.revision, ..Default::default() };
        for task in state.tasks.values() {
            index.tasks_by_request.entry(task.request_id.clone()).or_default().push(task.id.clone());
        }
        self.cards(&mut index);
        self.blockers();
        for d in state.interactions.values().filter(|d| matches!(d.payload, InteractionPayload::Delivery { .. })) {
            let by = self.delivery_blocked_by(d);
            if !by.is_empty() {
                index.blocked_by.insert(d.id.clone(), by);
            }
        }
        let attention: HashMap<&TaskId, Attention> = state.tasks.values().map(|t| (&t.id, self.attention(t))).collect();
        index.acting = state.tasks.values().filter_map(|t| match &attention[&t.id] {
            Attention::Act(card) => Some((t.id.clone(), card.clone())),
            _ => None,
        }).collect();
        self.inbox(&mut index, &attention);
        let mut waiting_of_task: HashMap<&TaskId, Vec<&InboxItem>> = HashMap::new();
        let mut waiting_of_node: HashMap<AgentId, Vec<InteractionId>> = HashMap::new();
        for item in &index.waiting {
            let InboxSubject::Interaction { interaction_id } = &item.subject;
            if let Some(task) = &item.task_id {
                waiting_of_task.entry(task).or_default().push(item);
            }
            if let Some(agent) = &item.agent_id {
                waiting_of_node.entry(agent.clone()).or_default().push(interaction_id.clone());
            }
        }
        for t in state.tasks.values() {
            let waiting = waiting_of_task.remove(&t.id).unwrap_or_default();
            let waits_for_user = waiting.iter().any(|item| item.kind != "information");
            let facts = TaskReading {
                attention: attention[&t.id].clone(),
                activity: state.task_activity(t),
                handed_back: state.task_handed_back(&t.id),
                merge_failure: state.task_merge_failure(&t.id),
                open_blockers: self.open_blockers.get(&t.id).cloned().unwrap_or_default(),
                open_dependencies: t.dependencies.iter().chain(&t.delivered_dependencies)
                    .filter(|id| **id != t.id && state.tasks.get(id).is_some_and(|d| !d.state.terminal()))
                    .cloned()
                    .collect(),
                waits_for_user,
                waits_for_review: waits_for_user
                    && waiting.iter().filter(|item| item.kind != "information").all(|item| item.kind == "delivery"),
                waiting: waiting.iter().map(|item| {
                    let InboxSubject::Interaction { interaction_id } = &item.subject;
                    interaction_id.clone()
                }).collect(),
            };
            index.tasks.insert(t.id.clone(), facts);
        }
        self.nodes(&mut index, waiting_of_node);
        index.blocks = self.blocks;
        index
    }

    fn cards(&mut self, index: &mut ReadIndex) {
        for i in self.state.interactions.values() {
            if let Scope::Task { task_id } = &i.scope {
                index.cards_by_task.entry(task_id.clone()).or_default().push(i.id.clone());
            }
            if let Author::Agent { agent_id } = &i.author {
                index.cards_by_node.entry(agent_id.clone()).or_default().push(i.id.clone());
            }
            let mut prs = BTreeSet::new();
            let mut revisions = BTreeSet::new();
            if let InteractionPayload::Delivery { targets, .. } = &i.payload {
                for target in targets {
                    match target {
                        DeliveryTarget::PullRequest { pr_id, .. } => {
                            prs.insert(pr_id);
                        }
                        DeliveryTarget::DocumentRevision { revision_id, .. } => {
                            revisions.insert(revision_id);
                        }
                        DeliveryTarget::Text { .. } => {}
                    }
                }
            }
            match &i.subject {
                Some(InteractionSubject::PullRequest { pr_id }) => {
                    prs.insert(pr_id);
                }
                Some(InteractionSubject::DocumentReview { revision_id, .. }) => {
                    revisions.insert(revision_id);
                }
                None => {}
            }
            for pr in prs {
                index.cards_by_pr.entry(pr.clone()).or_default().push(i.id.clone());
            }
            for revision in revisions {
                index.cards_by_document_revision.entry(revision.clone()).or_default().push(i.id.clone());
            }
        }
    }

    /// R16 for every card at once: the open deliveries each open card stands in front of, by the
    /// delivery, PR or document it names, and the open tasks that hold each task back.
    fn blockers(&mut self) {
        let state = self.state;
        let mut by_pr: HashMap<&PrId, Vec<&InteractionId>> = HashMap::new();
        let mut by_document: HashMap<&DocumentId, Vec<&InteractionId>> = HashMap::new();
        for d in state.interactions.values().filter(|d| state.open(d)) {
            let InteractionPayload::Delivery { targets, .. } = &d.payload else { continue };
            let prs: BTreeSet<&PrId> = targets.iter().filter_map(|target| match target {
                DeliveryTarget::PullRequest { pr_id, .. } => Some(pr_id),
                _ => None,
            }).collect();
            let documents: BTreeSet<&DocumentId> = targets.iter().filter_map(|target| match target {
                DeliveryTarget::DocumentRevision { document_id, .. } => Some(document_id),
                _ => None,
            }).collect();
            for pr in prs {
                by_pr.entry(pr).or_default().push(&d.id);
            }
            for document in documents {
                by_document.entry(document).or_default().push(&d.id);
            }
        }
        let mut cards_before: HashMap<&InteractionId, Vec<&Interaction>> = HashMap::new();
        for i in state.interactions.values().filter(|i| !i.card.blocks.is_empty() && state.open(i)) {
            let mut blocked = vec![];
            for target in &i.card.blocks {
                let delivery = match target {
                    CardTarget::Delivery { interaction_id } => state.interactions.get_key_value(interaction_id)
                        .filter(|(_, d)| state.open(d) && matches!(d.payload, InteractionPayload::Delivery { .. }))
                        .map(|(id, _)| vec![id]).unwrap_or_default(),
                    CardTarget::PullRequest { pr_id } => by_pr.get(pr_id).cloned().unwrap_or_default(),
                    CardTarget::Document { document_id } => by_document.get(document_id).cloned().unwrap_or_default(),
                };
                blocked.extend(delivery.into_iter().filter(|d| **d != i.id));
            }
            if blocked.is_empty() {
                continue;
            }
            let mut once = BTreeSet::new();
            for d in &blocked {
                if once.insert(*d) {
                    cards_before.entry(*d).or_default().push(i);
                }
            }
            self.blocks.insert(i.id.clone(), blocked.into_iter().cloned().collect());
        }
        self.cards_before = cards_before.into_iter().map(|(d, mut by)| {
            by.sort_by_key(|i| i.order);
            (d, by.into_iter().map(|i| i.id.clone()).collect())
        }).collect();
        for b in state.tasks.values().filter(|b| state.holds_back(b)) {
            for t in &b.blocks {
                self.open_blockers.entry(t).or_default().push(b.id.clone());
            }
        }
    }

    fn attention(&self, t: &Task) -> Attention {
        self.state.attention_given(t, self)
    }

    /// `delivery_blocked_by`, from the cards already found in front of each delivery.
    fn delivery_blocked_by(&self, delivery: &Interaction) -> Vec<InteractionId> {
        let state = self.state;
        let mut by = self.cards_before.get(&delivery.id).cloned().unwrap_or_default();
        let upstream = match &delivery.scope {
            Scope::Task { task_id } => state.tasks.get(task_id).and_then(|t| t.stack.as_ref()),
            _ => None,
        };
        if let Some(stack) = upstream
            && let Some(upstream_delivery) = state.interactions.get(&stack.presentation_id)
            && state.open(upstream_delivery)
            && upstream_delivery.id != delivery.id
            && state.pull_requests.get(&stack.pr_id).is_none_or(|pr| pr.state == PrState::Open)
            && !by.contains(&upstream_delivery.id)
        {
            by.push(upstream_delivery.id.clone());
        }
        by
    }

    fn inbox(&self, index: &mut ReadIndex, attention: &HashMap<&TaskId, Attention>) {
        let state = self.state;
        let entries = state.select_inbox(|i| {
            let blocked_by = index.blocked_by.get(&i.id).cloned().unwrap_or_default();
            let pending = state.inbox_pending(i, |t| attention[&t.id].clone());
            (i, blocked_by, pending)
        });
        let sequence = {
            let facts: Vec<_> = entries.iter().map(|(i, blocked_by, _)| InboxOrderFacts {
                id: &i.id.0,
                interaction_id: &i.id,
                order: i.order,
                kind: i.card.kind,
                blocking: i.blocking,
                blocked_by,
            }).collect();
            DomainState::inbox_sequence(&facts)
        };
        let mut slots: Vec<_> = entries.into_iter().map(Some).collect();
        for at in sequence {
            let Some((i, blocked_by, pending)) = slots[at].take() else { continue };
            index.queue.push(i.id.clone());
            if pending {
                let blocks = self.blocks.get(&i.id).cloned().unwrap_or_default();
                index.waiting_at.insert(i.id.clone(), index.waiting.len());
                index.waiting.push(state.item_given(i, true, blocked_by, blocks, self));
            }
        }
        index.counts = InboxCounts {
            roots: index.queue.len() as u64,
            pending: index.waiting.len() as u64,
            unread_information: state.interactions.values().filter(|i| {
                i.recipient == Recipient::User
                    && matches!(&i.payload, InteractionPayload::Information { read_by } if read_by.is_empty())
                    && !matches!(&i.scope, Scope::Task { task_id } if state.tasks.get(task_id).is_some_and(|t| t.archived))
            }).count() as u64,
        };
    }

    fn nodes(&self, index: &mut ReadIndex, mut waiting: HashMap<AgentId, Vec<InteractionId>>) {
        let state = self.state;
        let mut tasks: HashMap<&AgentId, Vec<&Task>> = HashMap::new();
        for t in state.tasks.values() {
            if let Some(agent) = &t.agent_id {
                tasks.entry(agent).or_default().push(t);
            }
        }
        let merging: BTreeSet<&AgentId> = state.merge_authorizations.values()
            .filter(|a| matches!(a.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain)
                && state.pull_requests.get(&a.pr_id).is_some_and(|pr| pr.state == PrState::Open))
            .filter_map(|a| a.claimed_by.as_ref())
            .collect();
        for agent in state.agents.values() {
            let all = tasks.remove(&agent.id).unwrap_or_default();
            let mine = || all.iter().copied().filter(|t| !t.archived);
            let task = mine().filter(|t| !t.state.terminal()).max_by_key(|t| t.revision)
                .or_else(|| mine().max_by_key(|t| t.revision));
            index.nodes.insert(agent.id.clone(), NodeReading {
                phase: agent.phase.clone(),
                tasks: mine().map(|t| t.id.clone()).collect(),
                task: task.map(|t| t.id.clone()),
                merging: merging.contains(&agent.id),
                waiting: waiting.remove(&agent.id).unwrap_or_default(),
                post_merge_work: all.iter().filter(|t| !t.state.terminal())
                    .find_map(|t| t.wrap_up.post_merge_work.as_ref().map(|work| work.reason.clone())),
            });
        }
    }
}

/// Every state a core test commits is read both ways: the index must say what the state answers
/// on demand, card by card and task by task.
#[cfg(test)]
pub(super) fn assert_index_matches(state: &DomainState) {
    let index = ReadIndex::build(state);
    let projection = state.projection();
    let ids = |items: &[InboxItem]| -> Vec<InteractionId> {
        items.iter().map(|item| {
            let InboxSubject::Interaction { interaction_id } = &item.subject;
            interaction_id.clone()
        }).collect()
    };
    let orders: Vec<InteractionId> = state.inbox_orders().into_iter()
        .map(|(id, _)| id.strip_prefix("interaction:").expect("inbox ids name their card").into())
        .collect();
    assert_eq!(index.queue(), orders.as_slice(), "the queue's order");
    assert_eq!(ids(&projection.inbox), orders, "the queue lists what the inbox lists");
    let waiting: Vec<InboxItem> = projection.inbox.iter().filter(|item| item.pending).cloned().collect();
    assert_eq!(index.waiting(), waiting.as_slice(), "Waiting for You, with its buttons");
    for item in &waiting {
        let InboxSubject::Interaction { interaction_id } = &item.subject;
        assert_eq!(index.waiting_card(interaction_id), Some(item));
    }
    assert_eq!(index.counts(), &projection.counts, "the counts");
    let acting: Vec<(TaskId, CardRef)> = state.acting().into_iter().collect();
    assert_eq!(index.acting(), acting.as_slice(), "the cards that act");
    for i in state.interactions.values() {
        let blocked_by = match &i.payload {
            InteractionPayload::Delivery { .. } => state.delivery_blocked_by(i),
            _ => vec![],
        };
        assert_eq!(index.blocked_by(&i.id), blocked_by.as_slice(), "what holds {} back", i.id);
        assert_eq!(index.blocks(&i.id), state.blocked_deliveries(i).as_slice(), "what {} holds back", i.id);
    }
    let cards = |keep: &dyn Fn(&Interaction) -> bool| -> Vec<InteractionId> {
        state.interactions.values().filter(|i| keep(i)).map(|i| i.id.clone()).collect()
    };
    for request in state.requests.keys() {
        let tasks: Vec<TaskId> = state.tasks.values().filter(|t| &t.request_id == request).map(|t| t.id.clone()).collect();
        assert_eq!(index.tasks_of_request(request), tasks.as_slice());
    }
    for pr in state.pull_requests.keys() {
        let about = cards(&|i| {
            matches!(&i.subject, Some(InteractionSubject::PullRequest { pr_id }) if pr_id == pr)
                || matches!(&i.payload, InteractionPayload::Delivery { targets, .. }
                    if targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { pr_id, .. } if pr_id == pr)))
        });
        assert_eq!(index.cards_of_pr(pr), about.as_slice(), "the cards of {pr}");
    }
    for revision in state.document_revisions.keys() {
        let about = cards(&|i| {
            matches!(&i.subject, Some(InteractionSubject::DocumentReview { revision_id, .. }) if revision_id == revision)
                || matches!(&i.payload, InteractionPayload::Delivery { targets, .. }
                    if targets.iter().any(|t| matches!(t, DeliveryTarget::DocumentRevision { revision_id, .. } if revision_id == revision)))
        });
        assert_eq!(index.cards_of_document_revision(revision), about.as_slice(), "the cards of {revision}");
    }
    for t in state.tasks.values() {
        let read = index.task(&t.id).expect("every task is read");
        assert_eq!(index.cards_of_task(&t.id), cards(&|i| i.scope == Scope::Task { task_id: t.id.clone() }).as_slice());
        assert_eq!(read.attention, state.attention(t), "the attention of {}", t.id);
        assert_eq!(read.open_blockers, state.open_blockers(&t.id), "what holds {} back", t.id);
        assert_eq!(read.activity, state.task_activity(t));
        assert_eq!(read.handed_back, state.task_handed_back(&t.id));
        assert_eq!(read.merge_failure, state.task_merge_failure(&t.id));
        assert_eq!(read.waiting, ids(&projection.waiting_cards(&t.id).cloned().collect::<Vec<_>>()), "the cards {} waits on", t.id);
        assert_eq!(read.waits_for_user, projection.waits_for_user(&t.id));
        assert_eq!(read.waits_for_review, projection.waits_for_review(&t.id));
        let open: Vec<TaskId> = t.dependencies.iter().chain(&t.delivered_dependencies)
            .filter(|id| **id != t.id)
            .filter_map(|id| state.tasks.get(id))
            .filter(|d| !d.state.terminal())
            .map(|d| d.id.clone())
            .collect();
        assert_eq!(read.open_dependencies, open);
    }
    for agent in state.agents.values() {
        let read = index.node(&agent.id).expect("every node is read");
        let mine = || state.tasks.values().filter(|t| t.agent_id.as_ref() == Some(&agent.id) && !t.archived);
        let task = mine().filter(|t| !t.state.terminal()).max_by_key(|t| t.revision).or_else(|| mine().max_by_key(|t| t.revision));
        assert_eq!(index.task_of_node(&agent.id), task.map(|t| &t.id), "the task of {}", agent.id);
        assert_eq!(read.phase, agent.phase);
        assert_eq!(read.tasks, mine().map(|t| t.id.clone()).collect::<Vec<_>>());
        assert_eq!(index.cards_of_node(&agent.id), cards(&|i| i.author == Author::Agent { agent_id: agent.id.clone() }).as_slice());
        let merging = state.merge_authorizations.values().any(|a| a.claimed_by.as_ref() == Some(&agent.id)
            && matches!(a.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain)
            && state.pull_requests.get(&a.pr_id).is_some_and(|pr| pr.state == PrState::Open));
        assert_eq!(read.merging, merging, "whether {} merges", agent.id);
        let waiting: Vec<InboxItem> = projection.inbox.iter().filter(|item| item.pending && item.agent_id.as_ref() == Some(&agent.id)).cloned().collect();
        assert_eq!(read.waiting, ids(&waiting));
        let work = state.tasks.values().filter(|t| t.agent_id.as_ref() == Some(&agent.id) && !t.state.terminal())
            .find_map(|t| t.wrap_up.post_merge_work.as_ref().map(|work| work.reason.clone()));
        assert_eq!(read.post_merge_work, work);
    }
}
