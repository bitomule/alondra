//! The MCP servers of each node that runs through the Agent SDK, as its Claude reported them, and
//! the sign-ins Claude opened for them. Claude keeps every credential: Alondra keeps only how each
//! server is and where the user signs in. The browser's return from a sign-in carries a one-time
//! code; it reaches Claude's own listener and is never recorded here.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_MCP_SERVERS: usize = 64;
const MAX_NAME_BYTES: usize = 256;
const MAX_SERVER_URL_BYTES: usize = 2048;
const MAX_SIGN_IN_URL_BYTES: usize = 8192;

/// How Claude reports a server; a status this release does not know is kept as `Unknown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpStatus {
    Connected,
    NeedsAuth,
    Failed,
    Pending,
    Disabled,
    #[serde(other)]
    Unknown,
}

impl McpStatus {
    pub fn of(native: &str) -> Self {
        serde_json::from_value(serde_json::Value::String(native.to_owned())).unwrap_or(Self::Unknown)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerState {
    pub name: String,
    pub status: McpStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Where it is configured (user, project, local, plugin, claudeai…), as Claude says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// How Claude reaches it (stdio, http, sse, claudeai-proxy…), as Claude says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    /// A remote server's address, without credentials, query or fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whose it is, where the provider found it and how the user connects it, as the provider says.
    #[serde(default)]
    pub traits: alondra_foundation::McpTraits,
}

impl McpServerState {
    /// The agent keeps a sign-in to it that it can forget.
    pub fn signs_in(&self) -> bool {
        self.traits.sign_out
    }
}

/// How a sign-in Claude opened goes on, as its execution answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum McpSignIn {
    /// The user signs in at `url`. With `callback_port` the browser comes back to Claude's listener
    /// at `http://localhost:<callback_port>/callback` carrying `state`; without it the sign-in ends
    /// on claude.ai and Claude connects again afterwards.
    Browser {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        callback_port: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
    },
    /// Claude's stored sign-in still works: nothing to do in a browser.
    Connected,
    Refused { error: String },
}

/// A sign-in the user has not finished yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSignIn {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// What a node's current SDK execution reported of its MCP servers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeMcp {
    pub generation: u64,
    pub servers: Vec<McpServerState>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sign_ins: BTreeMap<String, OpenSignIn>,
    /// Why Claude refused the last sign-in to each server, and the revision that recorded it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub refused: BTreeMap<String, (String, u64)>,
}

impl NodeMcp {
    pub fn server(&self, name: &str) -> Option<&McpServerState> {
        self.servers.iter().find(|server| server.name == name)
    }

    /// The servers as `reported` says they are now. A report that leaves out how a server is
    /// reached (init lists only names and statuses) keeps what an earlier one said of it, and so
    /// does the error of a server whose status did not change.
    pub fn merged(&self, reported: &[McpServerState]) -> Vec<McpServerState> {
        reported
            .iter()
            .map(|server| {
                let mut merged = server.clone();
                if let Some(previous) = self.server(&server.name) {
                    merged.source = merged.source.or_else(|| previous.source.clone());
                    merged.transport = merged.transport.or_else(|| previous.transport.clone());
                    merged.url = merged.url.or_else(|| previous.url.clone());
                    if server.transport.is_none() && previous.transport.is_some() {
                        merged.traits = previous.traits;
                    }
                    if previous.status == server.status {
                        merged.error = merged.error.or_else(|| previous.error.clone());
                    }
                }
                merged
            })
            .collect()
    }
}

/// What an open task's node reported of its MCP servers, and the sign-ins it has open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpNodeReading {
    pub task: TaskId,
    pub agent: AgentId,
    /// The node's SDK execution the servers are of.
    pub generation: u64,
    pub servers: Vec<McpServerState>,
    pub signing_in: Vec<String>,
}

fn bounded(field: &str, value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return refuse("invalid_input", field, &format!("give a non-empty value of at most {max} bytes without control characters"));
    }
    Ok(())
}

fn valid_servers(servers: &[McpServerState]) -> Result<()> {
    if servers.len() > MAX_MCP_SERVERS {
        return refuse("invalid_input", "servers", &format!("at most {MAX_MCP_SERVERS} MCP servers"));
    }
    for server in servers {
        bounded("servers.name", &server.name, MAX_NAME_BYTES)?;
        for (field, value) in [("servers.error", &server.error), ("servers.source", &server.source), ("servers.transport", &server.transport)] {
            if let Some(value) = value {
                bounded(field, value, MAX_NAME_BYTES)?;
            }
        }
        if let Some(url) = &server.url {
            bounded("servers.url", url, MAX_SERVER_URL_BYTES)?;
            if url.contains(['?', '#', '@']) {
                return refuse("invalid_input", "servers.url", "keep only the scheme, host and path of the address");
            }
        }
    }
    Ok(())
}

fn valid_sign_in(sign_in: &McpSignIn) -> Result<()> {
    match sign_in {
        McpSignIn::Browser { url, callback_port, state } => {
            bounded("sign_in.url", url, MAX_SIGN_IN_URL_BYTES)?;
            if !["https://", "http://localhost", "http://127.0.0.1"].iter().any(|scheme| url.starts_with(scheme)) {
                return refuse("invalid_input", "sign_in.url", "the user signs in only on an https page");
            }
            if *callback_port == Some(0) || callback_port.is_some() != state.is_some() {
                return refuse("invalid_input", "sign_in.callback_port", "a sign-in that comes back to Claude has both its port and its state");
            }
            if let Some(state) = state {
                bounded("sign_in.state", state, MAX_NAME_BYTES)?;
            }
            Ok(())
        }
        McpSignIn::Connected => Ok(()),
        McpSignIn::Refused { error } => bounded("sign_in.error", error, MAX_NAME_BYTES),
    }
}

impl DomainState {
    /// The MCP servers each open task's node reported, by task.
    pub fn mcp_nodes(&self) -> Vec<McpNodeReading> {
        self.tasks.values().filter(|task| !task.over()).filter_map(|task| {
            let agent = task.agent_id.clone()?;
            let node = self.mcp.get(&agent)?;
            Some(McpNodeReading { task: task.id.clone(), agent, generation: node.generation, servers: node.servers.clone(), signing_in: node.sign_ins.keys().cloned().collect() })
        }).collect()
    }

    /// The node of the task's current SDK execution of `generation`.
    fn mcp_node(&self, task_id: &TaskId, generation: u64) -> Result<AgentId> {
        self.sdk_lifecycle_at(task_id, generation)?;
        self.task(task_id)?.agent_id.clone().ok_or_else(|| DomainError::new("no_task", format!("Task {task_id} has no node")))
    }

    /// What the node of the task's current SDK execution reported of `server`.
    pub(super) fn mcp_server(&self, task_id: &TaskId, server: &str) -> Result<&McpServerState> {
        let agent = self.task(task_id)?.agent_id.as_ref().ok_or_else(|| DomainError::new("no_task", format!("Task {task_id} has no node")))?;
        self.mcp
            .get(agent)
            .and_then(|node| node.server(server))
            .ok_or_else(|| DomainError::new("not_found", format!("Task {task_id}'s Claude has no MCP server named {server}")))
    }

    /// Claude connects again a server its execution reported.
    pub(super) fn mcp_reconnect_guard(&self, actor: &Actor, task_id: &TaskId, server: &str) -> Result<LifecycleRecord> {
        let record = self.control_guard(actor, task_id, ControlRecord::ReconnectMcp { server: server.into() })?;
        self.mcp_server(task_id, server)?;
        Ok(record)
    }

    /// Claude opens a sign-in to a server its execution reported, unless it is connected or off.
    pub(super) fn mcp_sign_in_guard(&self, actor: &Actor, task_id: &TaskId, server: &str) -> Result<LifecycleRecord> {
        let record = self.control_guard(actor, task_id, ControlRecord::StartMcpSignIn { server: server.into() })?;
        match self.mcp_server(task_id, server)?.status {
            McpStatus::Connected => fail("conflict", &format!("{server} is already connected")),
            McpStatus::Disabled => fail("conflict", &format!("{server} is turned off in Claude Code")),
            _ => Ok(record),
        }
    }

    /// Claude forgets its sign-in to a server it reaches over HTTP: only those keep one Alondra
    /// can clear. A server Claude reaches through claude.ai is signed out on claude.ai.
    pub(super) fn mcp_sign_out_guard(&self, actor: &Actor, task_id: &TaskId, server: &str) -> Result<LifecycleRecord> {
        let record = self.control_guard(actor, task_id, ControlRecord::ClearMcpSignIn { server: server.into() })?;
        let reported = self.mcp_server(task_id, server)?;
        if !reported.signs_in() {
            return fail("conflict", &format!("{server} keeps no sign-in Alondra can clear"));
        }
        match reported.status {
            McpStatus::Disabled => fail("conflict", &format!("{server} is turned off in Claude Code")),
            McpStatus::NeedsAuth => fail("conflict", &format!("{server} has no sign-in to clear")),
            _ => Ok(record),
        }
    }

    /// The browser's return from the open sign-in to `server` is for that sign-in: the node's
    /// Claude waits for it on its listener, under `state`.
    pub(super) fn mcp_complete_guard(&self, actor: &Actor, task_id: &TaskId, server: &str, state: &str) -> Result<u16> {
        system(actor)?;
        self.sdk_lifecycle(task_id)?;
        let agent = self.task(task_id)?.agent_id.as_ref().ok_or_else(|| DomainError::new("no_task", format!("Task {task_id} has no node")))?;
        let Some(open) = self.mcp.get(agent).and_then(|node| node.sign_ins.get(server)) else {
            return fail("not_found", &format!("No sign-in to {server} is open: start it again"));
        };
        let Some(port) = open.callback_port else {
            return fail("conflict", &format!("The sign-in to {server} does not come back to Claude"));
        };
        if open.state.as_deref() != Some(state) {
            return fail("conflict", &format!("This return is from another sign-in to {server}: start the sign-in again"));
        }
        Ok(port)
    }

    pub(super) fn record_mcp_servers(&mut self, actor: &Actor, task_id: &TaskId, generation: u64, servers: &[McpServerState]) -> Result<()> {
        system(actor)?;
        valid_servers(servers)?;
        let agent = self.mcp_node(task_id, generation)?;
        let node = self.mcp.entry(agent).or_default();
        if node.generation != generation {
            *node = NodeMcp { generation, ..NodeMcp::default() };
        }
        node.servers = node.merged(servers);
        let open: Vec<String> = node.sign_ins.keys().cloned().collect();
        for name in open {
            if node.server(&name).is_none_or(|server| server.status == McpStatus::Connected) {
                node.sign_ins.remove(&name);
            }
        }
        Ok(())
    }

    pub(super) fn record_mcp_sign_in(&mut self, actor: &Actor, task_id: &TaskId, generation: u64, server: &str, sign_in: &McpSignIn) -> Result<()> {
        system(actor)?;
        bounded("server", server, MAX_NAME_BYTES)?;
        valid_sign_in(sign_in)?;
        let agent = self.mcp_node(task_id, generation)?;
        let node = self.mcp.entry(agent).or_default();
        if node.generation != generation {
            *node = NodeMcp { generation, ..NodeMcp::default() };
        }
        node.refused.remove(server);
        match sign_in {
            McpSignIn::Browser { url, callback_port, state } => {
                node.sign_ins.insert(server.into(), OpenSignIn { url: url.clone(), callback_port: *callback_port, state: state.clone() });
            }
            McpSignIn::Connected => {
                node.sign_ins.remove(server);
            }
            McpSignIn::Refused { error } => {
                node.sign_ins.remove(server);
                node.refused.insert(server.into(), (error.clone(), self.revision + 1));
            }
        }
        Ok(())
    }

    /// The browser's return reached Claude's listener: the sign-in is no longer open.
    pub(super) fn complete_mcp_sign_in(&mut self, actor: &Actor, task_id: &TaskId, server: &str, state: &str) -> Result<()> {
        self.mcp_complete_guard(actor, task_id, server, state)?;
        if let Some(node) = self.task(task_id)?.agent_id.clone().and_then(|agent| self.mcp.get_mut(&agent)) {
            node.sign_ins.remove(server);
        }
        Ok(())
    }
}
