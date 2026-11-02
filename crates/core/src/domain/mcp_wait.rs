//! A task whose node needs an MCP server that is not connected waits for it on a card of its own,
//! as every wait does. Waiting for You shows one card per server however many tasks wait for it:
//! the oldest. Connect opens the sign-in on a node that waits; Go on without it sends every
//! waiting task on. Only facts release a task otherwise: its own node reporting the server
//! connected.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The card of a task that waits for an MCP server; the server it waits for is in its body.
pub const MCP_WAIT_PREFIX: &str = "mcp-sign-in-";
/// The answer that sends every task waiting for the server on without it.
pub const MCP_GO_ON_WITHOUT: &str = "go_on_without";
/// What the MCP servers screen names a server by, for its actions and its sign-in.
pub const MCP_SERVER_PREFIX: &str = "mcp-server-";

/// A server as Alondra tells it apart: by its name and, for a remote one, its address. Two
/// projects with a server of the same name at different addresses wait on two cards.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct McpServerKey {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl McpServerKey {
    pub fn of(server: &McpServerState) -> Self {
        Self { name: server.name.clone(), url: server.url.clone() }
    }

    /// The server an MCP wait's card waits for.
    pub fn of_card(card: &Interaction) -> Option<Self> {
        if card.author != Author::System || !card.id.0.starts_with(MCP_WAIT_PREFIX) {
            return None;
        }
        serde_json::from_str(&card.body).ok()
    }

    /// The opaque id the MCP servers screen names it by: its name and address may hold anything.
    pub fn target(&self) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(serde_json::to_vec(self).expect("a server key serializes"));
        let hex: String = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
        format!("{MCP_SERVER_PREFIX}{hex}")
    }

    /// Whether a node reports this server: the same name, and the same address when both know it.
    pub fn names(&self, server: &McpServerState) -> bool {
        self.name == server.name && (self.url.is_none() || server.url.is_none() || self.url == server.url)
    }
}

/// A task that waits for an MCP server, and the card it waits on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpWaiting {
    pub task: TaskId,
    pub card: InteractionId,
    pub server: McpServerKey,
    pub order: Revision,
}

fn connected_text(name: &str) -> String {
    format!("{name} is connected: go on with your task from where it stopped.")
}

fn without_text(name: &str) -> String {
    format!("{name} is not available: go on with your task without it.")
}

impl DomainState {
    /// The open tasks that wait for an MCP server, oldest card first.
    pub fn mcp_waiting(&self) -> Vec<McpWaiting> {
        let mut waiting: Vec<McpWaiting> = self
            .interactions
            .values()
            .filter(|card| card.id.0.starts_with(MCP_WAIT_PREFIX) && self.open(card))
            .filter_map(|card| {
                let Scope::Task { task_id } = &card.scope else { return None };
                if self.tasks.get(task_id).is_none_or(Task::over) {
                    return None;
                }
                Some(McpWaiting { task: task_id.clone(), card: card.id.clone(), server: McpServerKey::of_card(card)?, order: card.order })
            })
            .collect();
        waiting.sort_by_key(|waiting| waiting.order);
        waiting
    }

    /// The tasks that wait for `server`, oldest card first.
    pub fn mcp_waiting_for(&self, server: &McpServerKey) -> Vec<McpWaiting> {
        self.mcp_waiting().into_iter().filter(|waiting| waiting.server == *server).collect()
    }

    /// The cards Waiting for You shows of the MCP waits: the oldest of each server.
    pub fn shown_mcp_cards(&self) -> BTreeSet<InteractionId> {
        let mut oldest: BTreeMap<McpServerKey, InteractionId> = BTreeMap::new();
        for waiting in self.mcp_waiting() {
            oldest.entry(waiting.server).or_insert(waiting.card);
        }
        oldest.into_values().collect()
    }

    /// An MCP wait's card that Waiting for You leaves out: another of its server stands for it.
    pub(crate) fn mcp_grouped_away(&self, i: &Interaction) -> bool {
        McpServerKey::of_card(i).is_some() && !self.shown_mcp_cards().contains(&i.id)
    }

    /// What the node of the task reported of the server it waits for.
    pub fn mcp_server_of(&self, task: &TaskId, server: &McpServerKey) -> Option<&McpServerState> {
        let agent = self.tasks.get(task)?.agent_id.as_ref()?;
        self.mcp.get(agent)?.servers.iter().find(|reported| server.names(reported))
    }

    /// The node waits for a server its Claude reported and that is not connected: on its own card,
    /// paused, as the user decides or the server connects.
    pub(super) fn mcp_wait_guard(&self, actor: &Actor, task_id: &TaskId, server: &str) -> Result<(McpServerKey, LifecycleRecord)> {
        self.assigned(actor, task_id)?;
        let lifecycle = self.sdk_lifecycle(task_id)?;
        let reported = self.mcp_server(task_id, server)?;
        if reported.status == McpStatus::Connected {
            return fail("conflict", &format!("{server} is connected: use its tools and go on"));
        }
        if self.mcp_waiting().iter().any(|waiting| waiting.task == *task_id) {
            return fail("conflict", "Your task already waits for an MCP server: end your turn");
        }
        let card = InteractionId(format!("{MCP_WAIT_PREFIX}{task_id}-{}", self.revision + 1));
        let record = self.judged(task_id, lifecycle, LifecycleRecord::Paused { card, owner: Some(PauseOwner::User) })?;
        Ok((McpServerKey::of(reported), record))
    }

    /// The user hears of a server only when no other task waits for it.
    pub(super) fn wait_for_mcp(&mut self, actor: &Actor, task_id: &TaskId, server: &str, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let (key, record) = self.mcp_wait_guard(actor, task_id, server)?;
        let LifecycleRecord::Paused { card, .. } = &record else { unreachable!("the guard pauses the task") };
        let card = card.clone();
        let announce = self.mcp_waiting_for(&key).is_empty();
        let revision = self.revision + 1;
        self.interactions.insert(card.clone(), mcp_card(card.clone(), task_id, &key, revision));
        if announce {
            effects.push(DomainEffect::NotifyUser { task_id: Some(task_id.clone()), interaction_id: card });
        }
        self.decide(lifecycles, task_id, record)
    }

    /// The task's node reported its servers: each of its waits for a server now connected ends,
    /// and the task goes on.
    pub(super) fn mcp_reported(&mut self, task_id: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
        let released: Vec<McpWaiting> = self
            .mcp_waiting()
            .into_iter()
            .filter(|waiting| waiting.task == *task_id)
            .filter(|waiting| self.mcp_server_of(task_id, &waiting.server).is_some_and(|server| server.status == McpStatus::Connected))
            .collect();
        for waiting in released {
            self.retire(&waiting.card);
            self.resume_mcp(&waiting, connected_text(&waiting.server.name), lifecycles)?;
        }
        Ok(())
    }

    /// Only Go on without it answers an MCP wait's card: Connect is the sign-in itself.
    pub(super) fn mcp_answer_guard(&self, card: &Interaction, option_id: &str) -> Result<()> {
        if McpServerKey::of_card(card).is_some() && option_id != MCP_GO_ON_WITHOUT {
            return refuse("invalid_input", "option_id", &format!("This card is answered only with {MCP_GO_ON_WITHOUT}: Connect opens the sign-in"));
        }
        Ok(())
    }

    /// The user went on without the server: every task that waits for it goes on, and the other
    /// cards of the server are withdrawn. False for any other card.
    pub(super) fn settle_mcp_card(&mut self, card: &InteractionId, lifecycles: &mut Lifecycles) -> Result<bool> {
        let Some(key) = self.interactions.get(card).and_then(McpServerKey::of_card) else { return Ok(false) };
        let Some(Scope::Task { task_id }) = self.interactions.get(card).map(|i| i.scope.clone()) else { return Ok(true) };
        let order = self.interactions[card].order;
        let answered = McpWaiting { task: task_id, card: card.clone(), server: key.clone(), order };
        let waiting: Vec<McpWaiting> = std::iter::once(answered).chain(self.mcp_waiting_for(&key)).collect();
        for waiting in waiting {
            if waiting.card != *card {
                self.retire(&waiting.card);
            }
            if self.tasks.get(&waiting.task).is_some_and(|task| !task.over()) {
                self.resume_mcp(&waiting, without_text(&key.name), lifecycles)?;
            }
        }
        Ok(true)
    }

    fn resume_mcp(&mut self, waiting: &McpWaiting, text: String, lifecycles: &mut Lifecycles) -> Result<()> {
        let record = LifecycleRecord::Resumed { card: waiting.card.clone(), input: self.next_input(&waiting.task), text: Some(text) };
        self.decide(lifecycles, &waiting.task, record)
    }
}

fn mcp_card(id: InteractionId, task: &TaskId, server: &McpServerKey, revision: u64) -> Interaction {
    let need = clip(&format!("Sign in to {}: a task waits for it", server.name), CardSpec::NEED_MAX);
    let why = format!("The task needs the MCP server {} and Claude cannot use it until you sign in.", server.name);
    let card = CardSpec::new(CardKind::Problem, need.clone()).why(clip(&why, CardSpec::WHY_MAX));
    let option = QuestionOption {
        id: MCP_GO_ON_WITHOUT.into(),
        label: format!("Go on without {}", server.name),
        description: Some("Every task that waits for it goes on without it".into()),
    };
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: serde_json::to_string(server).expect("a server key serializes"),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question { options: vec![option], answer: None },
        revision,
        order: revision,
        withdrawn: false,
        card,
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, shortened: false,
    }
}
