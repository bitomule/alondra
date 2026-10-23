//! The Agents screen: each Claude Code instance, whether it is connected, the tasks assigned to
//! it and, when alondrad reports its runtime, what the session is doing. Finishing one task
//! never finishes the agent or its other tasks.

use std::collections::BTreeMap;
use std::time::Duration;

use alondra_core::domain::{Agent, AgentId, Task};
use alondra_core::now_millis;
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::Value;

use super::keys::{self, Cmd, Motion};
use super::{
    App, Flow, Msg, Pane, Remote, Search, THREADS_EVERY_MS, Target, input::TextInput,
};
use crate::client::Client;
use crate::ops::{self, MessageKind, Recovery};
use crate::runtime::{Activity, RuntimeNode};

/// A row of the agents list: a project's header, or an agent.
pub enum AgentRow {
    Group { name: String, total: usize },
    Agent(AgentId),
}

impl App {
    /// The runtime's view of an agent's session, when alondrad reports it.
    pub fn node(&self, id: &AgentId) -> Option<&RuntimeNode> {
        self.runtime.as_ref()?.node(id)
    }

    /// What each thread of the agent does, from its own route; nothing from an alondrad without it.
    pub fn agent_activity(&self, id: &AgentId) -> Vec<Activity> {
        match &self.threads {
            Some((agent, threads)) if agent == id => threads.clone(),
            _ => vec![],
        }
    }
    /// The selected node's Views document carries its activity and runtime context.
    pub(super) fn load_threads(&mut self) {
        self.load_selected_view();
    }
}

/// What a session last said: its last message, else what the daemon says it needs or does.
pub fn said(n: &RuntimeNode) -> String {
    n.last_message
        .clone()
        .or(n.daemon_needs.clone())
        .or(n.daemon_detail.clone())
        .unwrap_or_default()
}
