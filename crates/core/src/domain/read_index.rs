//! What every screen reads of the domain, decided once for each committed revision: who holds
//! what back, what each task asks of the user and what its node does, and Waiting for You in its
//! order with the buttons of every card in it. Built with the same rules the state answers on
//! demand, so a screen reading the index and one asking the state never disagree.
use super::*;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// What a task shows, from the domain alone: the client's language and the live data of its node
/// (what it is doing, a pause the machine imposed) are the screen's to add.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskReading {
    pub attention: Attention,
    pub activity: Option<TaskActivity>,
    /// What it is doing, or who it waits on, as its lifecycle says; `None` before it starts and
    /// once it ended.
    pub live: Option<alondra_domain::LiveState>,
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
    /// Every task it ever had, archived ones too, by id.
    pub all_tasks: Vec<TaskId>,
    /// The task it works for: its latest open one, or else its latest.
    pub task: Option<TaskId>,
    /// It merges a PR Alondra authorized and that is still open.
    pub merging: bool,
    /// The cards it wrote that wait for the user, in the queue's order.
    pub waiting: Vec<InteractionId>,
    /// What it said it still does after its task's PR merged.
    pub post_merge_work: Option<String>,
    /// Its task's execution runs through the Agent SDK: no terminal to attach to, and what it
    /// is told reaches it through its cards.
    pub through_sdk: bool,
    /// Its SDK execution is connected and runs a turn, or work of its own: what an interrupt stops.
    pub sdk_running: bool,
    /// How far stopping the turn its connected SDK execution runs went: once Claude accepted the
    /// interrupt, stopping it ends Claude's process; once that was asked, nothing more stops it.
    pub sdk_turn_stop: Option<alondra_domain::TurnStop>,
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
    /// The open cards in front of each delivery, without the upstream delivery of its stack.
    cards_before: HashMap<InteractionId, Vec<InteractionId>>,
    blocks: HashMap<InteractionId, Vec<InteractionId>>,
    reviewers: HashMap<TaskId, Vec<TaskId>>,
    dependents: HashMap<TaskId, Vec<TaskId>>,
    prs_by_task: HashMap<TaskId, Vec<PrId>>,
    groups: HashMap<String, Vec<InteractionId>>,
    scheduled: HashSet<TaskId>,
    failed_launches: HashMap<TaskId, String>,
    schedule_titles: ScheduleTitles,
    tasks: HashMap<TaskId, TaskReading>,
    nodes: HashMap<AgentId, NodeReading>,
    acting: Vec<(TaskId, CardRef)>,
    queue: Vec<InteractionId>,
    queue_at: HashMap<InteractionId, usize>,
    waiting: Vec<InboxItem>,
    asks_outside_tasks: bool,
    waiting_at: HashMap<InteractionId, usize>,
    counts: InboxCounts,
    mcp_waiting: Vec<McpWaiting>,
    mcp_nodes: Vec<McpNodeReading>,
    accounts: Vec<crate::accounts::AccountStatus>,
    account_reserve: crate::usage::Share,
    account_policy: crate::accounts::Policy,
    account_moves: bool,
    merge_method: super::MergeMethod,
    account_usage: Vec<crate::usage::AccountUsage>,
    account_nodes: Vec<(crate::accounts::AccountId, usize)>,
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
    /// Where a card stands in the queue Waiting for You is drawn from, if it is in it.
    pub fn queue_position(&self, card: &InteractionId) -> Option<usize> {
        self.queue_at.get(card).copied()
    }
    /// A card with its buttons as the inbox lists it: the one in Waiting for You, or else decided
    /// now from what the index holds, by the same rules.
    pub fn item<'a>(&'a self, state: &DomainState, card: &Interaction) -> Cow<'a, InboxItem> {
        if let Some(item) = self.waiting_card(&card.id) {
            return Cow::Borrowed(item);
        }
        let pending = !self.queue_at.contains_key(&card.id) && state.inbox_pending(card, |t| {
            self.task(&t.id).map_or_else(|| state.attention(t), |read| read.attention.clone())
        });
        Cow::Owned(state.item_given(card, pending, self.blocked_by(&card.id).to_vec(), self.blocks(&card.id).to_vec(), self))
    }
    /// Every card of the inbox, in queue order, each with its buttons.
    pub fn queued<'a>(&'a self, state: &'a DomainState) -> impl Iterator<Item = Cow<'a, InboxItem>> + 'a {
        self.queue.iter().filter_map(|id| state.interactions.get(id)).map(move |card| self.item(state, card))
    }
    /// A task's cancel, reopen and archive buttons, by the guards the commands run.
    pub fn task_actions(&self, state: &DomainState, task: &TaskId) -> Option<Vec<AvailableAction>> {
        state.task_actions_given(task, self)
    }
    /// A card that waits for the user's answer belongs to no task: the orchestrator asks it.
    pub fn asks_outside_tasks(&self) -> bool {
        self.asks_outside_tasks
    }
    /// The tasks that review `task`, open or not, by id.
    pub fn reviewers(&self, task: &TaskId) -> &[TaskId] {
        self.reviewers.get(task).map_or(&[], Vec::as_slice)
    }
    /// The tasks that depend on `task`, by id.
    pub fn dependents(&self, task: &TaskId) -> &[TaskId] {
        self.dependents.get(task).map_or(&[], Vec::as_slice)
    }
    /// The PRs a task registered, by id.
    pub fn prs_of_task(&self, task: &TaskId) -> &[PrId] {
        self.prs_by_task.get(task).map_or(&[], Vec::as_slice)
    }
    /// `DomainState::group_members`: the questions asked together with `card`, in their order.
    pub fn group_members<'a>(&self, state: &'a DomainState, card: &'a Interaction) -> Vec<&'a Interaction> {
        let Some(group) = &card.card.group else { return vec![card] };
        self.groups.get(&group.id).into_iter().flatten()
            .filter_map(|id| state.interactions.get(id))
            .filter(|other| other.author == card.author)
            .collect()
    }
    /// `DomainState::group_complete`, from the index.
    pub fn group_complete(&self, state: &DomainState, card: &Interaction) -> bool {
        self.group_members(state, card).iter().all(|member| !member.withdrawn && DomainState::answered_question(member))
    }
    /// The open tasks that wait for an MCP server, oldest card first.
    pub fn mcp_waiting(&self) -> &[McpWaiting] {
        &self.mcp_waiting
    }
    /// The tasks that wait for `server`, oldest card first.
    pub fn mcp_waiting_for<'a>(&'a self, server: &'a McpServerKey) -> impl Iterator<Item = &'a McpWaiting> + 'a {
        self.mcp_waiting.iter().filter(move |waiting| waiting.server == *server)
    }
    /// The MCP servers each open task's node reported, by task.
    pub fn mcp_nodes(&self) -> &[McpNodeReading] {
        &self.mcp_nodes
    }
    /// What a task waits for, when it waits for an MCP server.
    pub fn mcp_wait_of_task(&self, task: &TaskId) -> Option<&McpWaiting> {
        self.mcp_waiting.iter().find(|waiting| waiting.task == *task)
    }
    /// How this Mac merges PRs, when their repository allows it.
    pub fn merge_method(&self) -> super::MergeMethod {
        self.merge_method
    }
    /// The provider accounts in the user's order, each with its state and plan.
    pub fn accounts(&self) -> &[crate::accounts::AccountStatus] {
        &self.accounts
    }
    /// How new work is spread over the accounts.
    pub fn account_policy(&self) -> crate::accounts::Policy {
        self.account_policy
    }
    /// Whether a node whose account reaches its limit goes on with another account.
    pub fn account_moves(&self) -> bool {
        self.account_moves
    }
    /// How many open tasks' nodes run on each account that has some.
    pub fn nodes_on_account(&self, account: &crate::accounts::AccountId) -> usize {
        self.account_nodes.iter().find(|(on, _)| on == account).map_or(0, |(_, count)| *count)
    }
    /// What each account last said of its usage.
    pub fn account_usage(&self) -> &[crate::usage::AccountUsage] {
        &self.account_usage
    }
    /// Each account's margin and their combined margin at `now`, on the local `today`.
    pub fn margins(&self, now: crate::model::Millis, today: &str) -> crate::usage::Margins {
        crate::usage::margins(&self.accounts, &self.account_usage, self.account_reserve, now, today)
    }
    /// The accounts' usage together, as the usage gauges and the pace card read it.
    pub fn usage(&self, now: crate::model::Millis, today: &str) -> Option<crate::usage::Usage> {
        crate::usage::summary(&self.accounts, &self.account_usage, self.account_reserve, now, today)
    }
    /// A scheduled run created the task.
    pub fn scheduled(&self, task: &TaskId) -> bool {
        self.scheduled.contains(task)
    }
    /// The error of the latest scheduled run that failed to launch the task, empty when it gave none.
    pub fn failed_launch(&self, task: &TaskId) -> Option<&str> {
        self.failed_launches.get(task).map(String::as_str)
    }
    /// The title of the schedule whose run launched a node, by the node or the name it was launched with.
    pub fn schedule_title(&self, node: &AgentId, name: Option<&str>) -> Option<&str> {
        let by_node = self.schedule_titles.by_node.get(node);
        let by_name = name.and_then(|name| self.schedule_titles.by_name.get(name));
        [by_node, by_name].into_iter().flatten().min_by_key(|(at, _)| *at).and_then(|(_, title)| title.as_deref())
    }
}

/// The first run, by its place among the runs, that launched each node, with its schedule's title.
#[derive(Clone, Debug, Default, PartialEq)]
struct ScheduleTitles {
    by_node: HashMap<AgentId, (usize, Option<String>)>,
    by_name: HashMap<String, (usize, Option<String>)>,
}

impl Holds for ReadIndex {
    fn first_blocker(&self, task: &TaskId) -> Option<TaskId> {
        self.task(task).and_then(|read| read.open_blockers.first()).cloned()
    }
    fn first_card(&self, delivery: &InteractionId) -> Option<InteractionId> {
        self.cards_before.get(delivery).and_then(|cards| cards.first()).cloned()
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
            for reviewed in &task.blocks {
                index.reviewers.entry(reviewed.clone()).or_default().push(task.id.clone());
            }
            for dependency in &task.dependencies {
                index.dependents.entry(dependency.clone()).or_default().push(task.id.clone());
            }
        }
        for pr in state.pull_requests.values() {
            index.prs_by_task.entry(pr.task_id.clone()).or_default().push(pr.id.clone());
        }
        Self::schedules(state, &mut index);
        index.mcp_waiting = state.mcp_waiting();
        index.mcp_nodes = state.mcp_nodes();
        index.accounts = state.accounts.clone();
        index.account_reserve = state.account_reserve;
        index.account_policy = state.account_policy;
        index.account_moves = state.account_moves;
        index.merge_method = state.merge_method;
        index.account_usage = state.account_usage.clone();
        index.account_nodes = state.accounts.iter().map(|account| (account.id.clone(), state.tasks_on_account(&account.id)))
            .filter(|(_, count)| *count > 0).collect();
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
                live: state.live_state(&t.id),
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
        index.cards_before = self.cards_before.into_iter().map(|(delivery, cards)| (delivery.clone(), cards)).collect();
        index.blocks = self.blocks;
        index
    }

    fn schedules(state: &DomainState, index: &mut ReadIndex) {
        let mut failed: HashMap<&TaskId, &ScheduleRun> = HashMap::new();
        for (at, run) in state.schedule.runs.values().enumerate() {
            if let Some(task) = &run.task_id {
                index.scheduled.insert(task.clone());
                if run.state == ScheduleRunState::Failed
                    && failed.get(task).is_none_or(|latest| run.finished_at >= latest.finished_at) {
                    failed.insert(task, run);
                }
            }
            let Some(launch) = &run.launch else { continue };
            let title = || run.definition_snapshot.as_ref().map(|snapshot| snapshot.title.clone())
                .or_else(|| state.schedule.schedules.get(&run.schedule_id).map(|schedule| schedule.title.clone()));
            if let Some(agent) = &launch.agent_id {
                index.schedule_titles.by_node.entry(agent.clone()).or_insert_with(|| (at, title()));
            }
            index.schedule_titles.by_name.entry(launch.intent.name.clone()).or_insert_with(|| (at, title()));
        }
        index.failed_launches = failed.into_iter().map(|(task, run)| {
            (task.clone(), run.error.as_deref().map(str::trim).filter(|error| !error.is_empty()).unwrap_or("").to_owned())
        }).collect();
    }

    fn cards(&mut self, index: &mut ReadIndex) {
        for i in self.state.interactions.values() {
            if let Scope::Task { task_id } = &i.scope {
                index.cards_by_task.entry(task_id.clone()).or_default().push(i.id.clone());
            }
            if let Author::Agent { agent_id } = &i.author {
                index.cards_by_node.entry(agent_id.clone()).or_default().push(i.id.clone());
            }
            if let Some(group) = &i.card.group {
                index.groups.entry(group.id.clone()).or_default().push(i.id.clone());
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
        let place = |id: &InteractionId| self.state.interactions.get(id).and_then(|i| i.card.group.as_ref()).map_or(0, |g| g.index);
        for members in index.groups.values_mut() {
            members.sort_by_key(place);
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
            index.queue_at.insert(i.id.clone(), index.queue.len());
            index.queue.push(i.id.clone());
            if pending {
                let blocks = self.blocks.get(&i.id).cloned().unwrap_or_default();
                index.waiting_at.insert(i.id.clone(), index.waiting.len());
                index.waiting.push(state.item_given(i, true, blocked_by, blocks, self));
            }
        }
        index.asks_outside_tasks = index.waiting.iter().any(|item| item.task_id.is_none() && item.kind != "information");
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
            let (sdk_running, sdk_turn_stop) = task.filter(|t| admitted_through_sdk(state, &t.id, &agent.id))
                .map_or((false, None), |t| sdk_stopping(state, &t.id));
            index.nodes.insert(agent.id.clone(), NodeReading {
                phase: agent.phase.clone(),
                tasks: mine().map(|t| t.id.clone()).collect(),
                all_tasks: all.iter().map(|t| t.id.clone()).collect(),
                task: task.map(|t| t.id.clone()),
                merging: merging.contains(&agent.id),
                waiting: waiting.remove(&agent.id).unwrap_or_default(),
                post_merge_work: all.iter().filter(|t| !t.state.terminal())
                    .find_map(|t| t.wrap_up.post_merge_work.as_ref().map(|work| work.reason.clone())),
                through_sdk: task.is_some_and(|t| admitted_through_sdk(state, &t.id, &agent.id)),
                sdk_running,
                sdk_turn_stop,
            });
        }
    }
}

/// The inbox as the index lists it, each card with its buttons.
#[cfg(test)]
pub(crate) fn inbox_items(state: &DomainState) -> Vec<InboxItem> {
    ReadIndex::build(state).queued(state).map(Cow::into_owned).collect()
}

/// Whether the latest admission of `task` gave `node` an execution of its own: only the SDK's
/// carry one, `claude --bg` nodes are admitted without.
pub(super) fn admitted_through_sdk(state: &DomainState, task: &TaskId, node: &AgentId) -> bool {
    state.lifecycle_journal.get(task).and_then(|journal| journal.iter().rev().find_map(|record| match record {
        LifecycleRecord::Admitted { node: admitted, execution } => Some(admitted == node && execution.is_some()),
        _ => None,
    })) == Some(true)
}

/// Whether the SDK execution of `task` is connected and runs a turn or work of its own, and how
/// far stopping its turn went.
fn sdk_stopping(state: &DomainState, task: &TaskId) -> (bool, Option<alondra_domain::TurnStop>) {
    let Ok(lifecycle) = state.lifecycle_of(task) else { return (false, None) };
    if lifecycle.resource() != Some(alondra_domain::ResourceEvidence::Available) {
        return (false, None);
    }
    let running = matches!(lifecycle.activity(), alondra_domain::ActivityView::Working | alondra_domain::ActivityView::OwnWork);
    (running, lifecycle.turn_stop().filter(|_| lifecycle.activity() == alondra_domain::ActivityView::Working))
}

/// Every state a core test commits is read both ways: the index must say what the state answers
/// on demand, card by card and task by task.
#[cfg(test)]
pub(super) fn assert_index_matches(state: &DomainState) {
    if let Err(divergence) = state.kept_lifecycles_agree_with_journals() {
        panic!("the lifecycles the domain keeps are the ones its journals replay to: {divergence}");
    }
    for (task, _) in state.lifecycle_journal.iter() {
        let lifecycle = state.lifecycle_of(task).expect("a journal replays");
        let live = !matches!(lifecycle.phase(), alondra_domain::TaskPhase::Unstarted | alondra_domain::TaskPhase::Completed | alondra_domain::TaskPhase::Cancelled);
        assert_eq!(state.live_state(task).is_some(), live, "every live task, and only a live one, is working or waits on someone: {task} in {:?}", lifecycle.phase());
    }
    for task in state.tasks.values() {
        let owed = state.lifecycle_journal.get(&task.id)
            .and_then(|journal| super::replay(&task.id, journal).ok())
            .is_some_and(|lifecycle| lifecycle.post_merge().is_some());
        assert_eq!(task.wrap_up.post_merge_work.is_some(), owed, "the work after the merge {} shows matches what its lifecycle owes", task.id);
    }
    let index = ReadIndex::build(state);
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
    let inbox: Vec<InboxItem> = orders.iter().map(|id| state.item(&state.interactions[id])).collect();
    let waiting: Vec<InboxItem> = inbox.iter().filter(|item| item.pending).cloned().collect();
    assert_eq!(index.waiting(), waiting.as_slice(), "Waiting for You, with its buttons");
    for item in &waiting {
        let InboxSubject::Interaction { interaction_id } = &item.subject;
        assert_eq!(index.waiting_card(interaction_id), Some(item));
    }
    let unread_information = state.interactions.values().filter(|i| {
        i.recipient == Recipient::User
            && matches!(&i.payload, InteractionPayload::Information { read_by } if read_by.is_empty())
            && !matches!(&i.scope, Scope::Task { task_id } if state.tasks.get(task_id).is_some_and(|t| t.archived))
    }).count() as u64;
    let counts = InboxCounts { roots: inbox.len() as u64, pending: waiting.len() as u64, unread_information };
    assert_eq!(index.counts(), &counts, "the counts");
    assert_eq!(index.mcp_waiting(), state.mcp_waiting().as_slice(), "the tasks that wait for an MCP server");
    let reported: Vec<(TaskId, Vec<McpServerState>)> = state.tasks.values().filter(|task| !task.over())
        .filter_map(|task| Some((task.id.clone(), state.mcp.get(task.agent_id.as_ref()?)?.servers.clone()))).collect();
    let indexed: Vec<(TaskId, Vec<McpServerState>)> = index.mcp_nodes().iter().map(|node| (node.task.clone(), node.servers.clone())).collect();
    assert_eq!(indexed, reported, "the MCP servers each open task's node reported");
    assert_eq!(index.accounts(), state.accounts.as_slice(), "the provider accounts");
    assert_eq!(index.account_usage(), state.account_usage.as_slice(), "the accounts' usage");
    assert_eq!(index.account_reserve, state.account_reserve, "the accounts' reserve");
    assert_eq!(index.account_policy(), state.account_policy, "how work is spread over the accounts");
    assert_eq!(index.account_moves(), state.account_moves, "whether nodes move between accounts");
    for account in &state.accounts {
        assert_eq!(index.nodes_on_account(&account.id), state.tasks_on_account(&account.id), "the nodes on account {}", account.id);
    }
    assert_eq!(index.merge_method, state.merge_method, "the merge method");
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
        assert_eq!(read.live, state.live_state(&t.id), "what {} is doing", t.id);
        assert_eq!(read.handed_back, state.task_handed_back(&t.id));
        assert_eq!(read.merge_failure, state.task_merge_failure(&t.id));
        let on_task: Vec<InboxItem> = waiting.iter().filter(|item| item.task_id.as_ref() == Some(&t.id)).cloned().collect();
        let answers = || on_task.iter().filter(|item| item.kind != "information");
        assert_eq!(read.waiting, ids(&on_task), "the cards {} waits on", t.id);
        assert_eq!(read.waits_for_user, answers().next().is_some());
        assert_eq!(read.waits_for_review, answers().next().is_some() && answers().all(|item| item.kind == "delivery"));
        let open: Vec<TaskId> = t.dependencies.iter().chain(&t.delivered_dependencies)
            .filter(|id| **id != t.id)
            .filter_map(|id| state.tasks.get(id))
            .filter(|d| !d.state.terminal())
            .map(|d| d.id.clone())
            .collect();
        assert_eq!(read.open_dependencies, open);
    }
    let mut queued = 0;
    for i in state.interactions.values() {
        assert_eq!(index.item(state, i).as_ref(), &state.item(i), "the card {} with its buttons", i.id);
        assert_eq!(index.group_members(state, i), state.group_members(i), "the questions asked with {}", i.id);
        assert_eq!(index.group_complete(state, i), state.group_complete(i));
        if let Some(at) = index.queue_position(&i.id) {
            assert_eq!(index.queue()[at], i.id);
            queued += 1;
        }
    }
    assert_eq!(queued, index.queue().len(), "every queued card has its place");
    assert_eq!(index.asks_outside_tasks(),
        waiting.iter().any(|item| item.task_id.is_none() && item.kind != "information"));
    for t in state.tasks.values() {
        let by = |keep: &dyn Fn(&Task) -> bool| -> Vec<TaskId> { state.tasks.values().filter(|o| keep(o)).map(|o| o.id.clone()).collect() };
        assert_eq!(index.reviewers(&t.id), by(&|o| o.blocks.contains(&t.id)).as_slice(), "who reviews {}", t.id);
        assert_eq!(index.dependents(&t.id), by(&|o| o.dependencies.contains(&t.id)).as_slice(), "who depends on {}", t.id);
        let prs: Vec<PrId> = state.pull_requests.values().filter(|pr| pr.task_id == t.id).map(|pr| pr.id.clone()).collect();
        assert_eq!(index.prs_of_task(&t.id), prs.as_slice());
        assert_eq!(index.task_actions(state, &t.id), state.task_actions(&t.id), "the buttons of {}", t.id);
        let runs = || state.schedule.runs.values().filter(|run| run.task_id.as_ref() == Some(&t.id));
        assert_eq!(index.scheduled(&t.id), runs().next().is_some());
        let failed = runs().filter(|run| run.state == ScheduleRunState::Failed).max_by_key(|run| run.finished_at)
            .map(|run| run.error.as_deref().map(str::trim).filter(|error| !error.is_empty()).unwrap_or(""));
        assert_eq!(index.failed_launch(&t.id), failed, "the failed launch of {}", t.id);
    }
    for agent in state.agents.values() {
        let read = index.node(&agent.id).expect("every node is read");
        let all: Vec<TaskId> = state.tasks.values().filter(|t| t.agent_id.as_ref() == Some(&agent.id)).map(|t| t.id.clone()).collect();
        assert_eq!(read.all_tasks, all);
        let name = Some(agent.name.as_str());
        let title = state.schedule.runs.values().find(|run| run.launch.as_ref().is_some_and(|launch| {
            launch.agent_id.as_ref() == Some(&agent.id) || Some(launch.intent.name.as_str()) == name
        })).and_then(|run| run.definition_snapshot.as_ref().map(|snapshot| snapshot.title.clone())
            .or_else(|| state.schedule.schedules.get(&run.schedule_id).map(|schedule| schedule.title.clone())));
        assert_eq!(index.schedule_title(&agent.id, name), title.as_deref(), "the schedule that launched {}", agent.id);
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
        let through_sdk = task.is_some_and(|t| {
            let admissions = state.lifecycle_journal[&t.id].iter().filter_map(|record| match record {
                LifecycleRecord::Admitted { node, execution } => Some((node, execution)),
                _ => None,
            });
            admissions.last().is_some_and(|(node, execution)| {
                *node == agent.id && execution.as_ref().is_some_and(|identity| identity.host != alondra_domain::CLAUDE_BG_HOST)
            })
        });
        assert_eq!(read.through_sdk, through_sdk, "whether {} runs through the SDK", agent.id);
        let running = through_sdk && task.is_some_and(|t| {
            let lifecycle = replay(&t.id, &state.lifecycle_journal[&t.id]).unwrap();
            lifecycle.resource() == Some(alondra_domain::ResourceEvidence::Available)
                && matches!(lifecycle.activity(), alondra_domain::ActivityView::Working | alondra_domain::ActivityView::OwnWork)
        });
        assert_eq!(read.sdk_running, running, "whether {} runs something an interrupt stops", agent.id);
        let force_stop = CommandEnvelope { protocol_version: crate::domain::PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: "index-check".into(), expected_revision: state.revision, command: Command::ForceStopTurn { agent_id: agent.id.clone() } };
        assert_eq!(read.sdk_turn_stop == Some(alondra_domain::TurnStop::InterruptAccepted), through_sdk && state.guard(&Actor::System, &force_stop).is_ok(),
            "whether {} offers to force its turn to stop exactly when its guard lets it", agent.id);
        let stop = through_sdk.then(|| task.and_then(|t| {
            let lifecycle = replay(&t.id, &state.lifecycle_journal[&t.id]).unwrap();
            (lifecycle.resource() == Some(alondra_domain::ResourceEvidence::Available) && lifecycle.activity() == alondra_domain::ActivityView::Working)
                .then(|| lifecycle.turn_stop()).flatten()
        })).flatten();
        assert_eq!(read.sdk_turn_stop, stop, "how far stopping the turn of {} went", agent.id);
        let asking: Vec<InboxItem> = waiting.iter().filter(|item| item.agent_id.as_ref() == Some(&agent.id)).cloned().collect();
        assert_eq!(read.waiting, ids(&asking));
        let work = state.tasks.values().filter(|t| t.agent_id.as_ref() == Some(&agent.id) && !t.state.terminal())
            .find_map(|t| t.wrap_up.post_merge_work.as_ref().map(|work| work.reason.clone()));
        assert_eq!(read.post_merge_work, work);
    }
}
