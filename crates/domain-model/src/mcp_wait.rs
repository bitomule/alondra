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
        if card.author != Author::System || card.system_card != Some(SystemCard::McpWait) {
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



impl DomainState {
    /// The open tasks that wait for an MCP server, oldest card first.
    pub fn mcp_waiting(&self) -> Vec<McpWaiting> {
        let mut waiting: Vec<McpWaiting> = self
            .interactions
            .values()
            .filter(|card| card.system_card == Some(SystemCard::McpWait) && self.open(card))
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
    pub fn mcp_grouped_away(&self, i: &Interaction) -> bool {
        McpServerKey::of_card(i).is_some() && !self.shown_mcp_cards().contains(&i.id)
    }

    /// What the node of the task reported of the server it waits for.
    pub fn mcp_server_of(&self, task: &TaskId, server: &McpServerKey) -> Option<&McpServerState> {
        let agent = self.tasks.get(task)?.agent_id.as_ref()?;
        self.mcp.get(agent)?.servers.iter().find(|reported| server.names(reported))
    }

    /// The node waits for a server its Claude reported and that is not connected: on its own card,
    /// paused, as the user decides or the server connects.
    pub fn mcp_wait_guard(&self, actor: &Actor, task_id: &TaskId, server: &str) -> Result<(McpServerKey, LifecycleRecord)> {
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





}

