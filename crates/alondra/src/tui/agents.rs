//! The Agents screen: each Claude Code instance, whether it is connected, the tasks assigned to
//! it and, when alondrad reports its runtime, what the session is doing. Finishing one task
//! never finishes the agent or its other tasks.

use std::collections::BTreeMap;
use std::time::Duration;

use alondra_core::domain_v2::{Agent, AgentId, Task};
use alondra_core::now_millis;
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::Value;

use super::keys::{self, Cmd, Motion};
use super::{
    App, Closed, Flow, Msg, Pane, Remote, Search, THREADS_EVERY_MS, Target, input::TextInput,
};
use crate::client::Client;
use crate::ops::{self, MessageKind, Recovery};
use crate::runtime::{Activity, FleetNode};

/// A row of the agents list: a project's header, or an agent.
pub enum AgentRow {
    Group { name: String, total: usize },
    Agent(AgentId),
}

impl App {
    /// The runtime's view of an agent's session, when alondrad reports it.
    pub fn node(&self, id: &AgentId) -> Option<&FleetNode> {
        self.runtime.as_ref()?.node(id)
    }

    /// The orchestrator's session, which is not a domain agent but has its own place on top.
    pub fn orchestrator(&self) -> Option<&FleetNode> {
        self.runtime.as_ref()?.orchestrator_node()
    }

    pub fn orchestrator_selected(&self) -> bool {
        self.agent_sel
            .as_ref()
            .is_some_and(|id| self.orchestrator().is_some_and(|o| o.session_id == id.0))
    }

    /// Whether an agent answers the search: its name and id, and what the runtime says of it.
    #[cfg(test)]
    pub fn agent_matches(&self, a: &Agent, q: &str) -> bool {
        let tasks: Vec<String> = self
            .agent_tasks(&a.id)
            .into_iter()
            .map(|t| t.title.clone())
            .collect();
        let mut hay = format!("{} {} {}", a.name, a.id.0, tasks.join(" "));
        if let Some(n) = self.node(&a.id) {
            hay.push_str(&format!(
                " {} {} {} {} {}",
                n.short_id,
                n.project.clone().unwrap_or_default(),
                n.status_label(),
                said(n),
                n.cwd.clone().unwrap_or_default()
            ));
        }
        let hay = hay.to_lowercase();
        q.split_whitespace().all(|w| hay.contains(w))
    }

    #[cfg(test)]
    pub(super) fn local_agent_matches(&self, q: &str) -> Vec<AgentId> {
        self.domain
            .as_ref()
            .map(|d| {
                d.agents
                    .iter()
                    .filter(|a| self.agent_matches(a, q))
                    .map(|a| a.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Agents by name, narrowed by the search when there is one: first what matches locally,
    /// else what the daemon's search found.
    #[cfg(test)]
    pub fn agent_list(&self) -> Vec<&Agent> {
        let Some(d) = &self.domain else { return vec![] };
        let search = self.search.as_ref().filter(|s| !s.query().is_empty());
        let mut agents: Vec<&Agent> = match search {
            None => d.agents.iter().collect(),
            Some(s) => {
                let q = s.query();
                let local: Vec<&Agent> = d
                    .agents
                    .iter()
                    .filter(|a| self.agent_matches(a, &q))
                    .collect();
                match &s.remote {
                    Remote::Done {
                        query, sessions, ..
                    } if local.is_empty() && *query == q => d
                        .agents
                        .iter()
                        .filter(|a| sessions.contains(&a.id.0))
                        .collect(),
                    _ => local,
                }
            }
        };
        agents.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
        agents
    }

    /// The list grouped by project when the runtime names them; one flat list otherwise.
    #[cfg(test)]
    pub fn agent_rows(&self) -> Vec<AgentRow> {
        let agents = self.agent_list();
        if self.runtime.is_none() {
            return agents
                .into_iter()
                .map(|a| AgentRow::Agent(a.id.clone()))
                .collect();
        }
        let mut groups: BTreeMap<String, Vec<AgentId>> = BTreeMap::new();
        for a in agents {
            let project = self
                .node(&a.id)
                .and_then(|n| n.project.clone())
                .filter(|p| p != alondra_core::OTHERS_PROJECT)
                .unwrap_or_else(|| tr!("others", "otros").into());
            groups.entry(project).or_default().push(a.id.clone());
        }
        let mut sorted: Vec<(String, Vec<AgentId>)> = groups.into_iter().collect();
        sorted.sort_by_key(|(p, _)| p.to_lowercase());
        let mut rows = vec![];
        for (name, ids) in sorted {
            rows.push(AgentRow::Group {
                name,
                total: ids.len(),
            });
            rows.extend(ids.into_iter().map(AgentRow::Agent));
        }
        rows
    }

    /// Every selectable agent in keyboard order: the orchestrator first, then the list.
    #[cfg(test)]
    fn agent_ids(&self) -> Vec<AgentId> {
        let orchestrator = self.orchestrator().map(|o| AgentId(o.session_id.clone()));
        orchestrator
            .into_iter()
            .chain(self.agent_rows().into_iter().filter_map(|r| match r {
                AgentRow::Agent(id) => Some(id),
                AgentRow::Group { .. } => None,
            }))
            .collect()
    }

    /// The tasks assigned to an agent that are not archived.
    #[cfg(test)]
    pub fn agent_tasks(&self, id: &AgentId) -> Vec<&Task> {
        self.domain
            .as_ref()
            .map(|d| {
                d.tasks
                    .iter()
                    .filter(|t| t.agent_id.as_ref() == Some(id) && !t.archived)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn selected_agent(&self) -> Option<&Agent> {
        let id = self.agent_sel.as_ref()?;
        self.domain.as_ref()?.agents.iter().find(|a| &a.id == id)
    }

    /// What each thread of the agent does, from its own route; nothing from an alondrad without it.
    pub fn agent_activity(&self, id: &AgentId) -> Vec<Activity> {
        match &self.threads {
            Some((agent, threads)) if agent == id => threads.clone(),
            _ => vec![],
        }
    }

    #[cfg(test)]
    pub(super) fn reconcile_agents(&mut self) {
        let ids = self.agent_ids();
        match self
            .agent_sel
            .as_ref()
            .and_then(|s| ids.iter().position(|x| x == s))
        {
            Some(i) => self.agent_idx = i,
            None => {
                self.agent_idx = self.agent_idx.min(ids.len().saturating_sub(1));
                let next = ids.get(self.agent_idx).cloned();
                if next != self.agent_sel {
                    self.agent_detail_scroll = 0;
                }
                self.agent_sel = next;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn move_agents(&mut self, to: isize) {
        let ids = self.agent_ids();
        if ids.is_empty() {
            return;
        }
        let idx = to.clamp(0, ids.len() as isize - 1) as usize;
        if self.agent_sel.as_ref() != Some(&ids[idx]) {
            self.agent_detail_scroll = 0;
        }
        self.agent_idx = idx;
        self.agent_sel = Some(ids[idx].clone());
        self.load_threads();
    }

    /// The selected node's Views document carries its activity and runtime context.
    pub(super) fn load_threads(&mut self) {
        self.load_selected_view();
    }

    #[cfg(test)]
    pub(super) fn agents_cmd(&mut self, cmd: Cmd) {
        let i = self.agent_idx as isize;
        let detail = self.agent_zoom || (self.focus == Pane::Detail && self.agent_view.shown);
        match cmd {
            Cmd::Move(m) if detail => {
                self.agent_detail_scroll = keys::scroll(
                    self.agent_detail_scroll,
                    m,
                    self.agent_view.h,
                    self.agent_view.total,
                )
            }
            Cmd::Move(Motion::Down) => self.move_agents(i + 1),
            Cmd::Move(Motion::Up) => self.move_agents(i - 1),
            Cmd::Move(Motion::Top) => self.move_agents(0),
            Cmd::Move(Motion::Bottom) => self.move_agents(isize::MAX),
            Cmd::Move(m) => {
                self.agent_detail_scroll = keys::scroll(
                    self.agent_detail_scroll,
                    m,
                    self.agent_view.h,
                    self.agent_view.total,
                )
            }
            Cmd::Right if self.agent_view.shown && !self.agent_zoom => self.focus = Pane::Detail,
            Cmd::Right => self.agent_zoom = self.agent_sel.is_some(),
            Cmd::Expand => self.agent_zoom = !self.agent_zoom && self.agent_sel.is_some(),
            Cmd::Left | Cmd::Back if self.agent_zoom => self.agent_zoom = false,
            Cmd::Left | Cmd::Back if self.focus == Pane::Detail => self.focus = Pane::List,
            Cmd::Back if self.search.is_some() => self.search = None,
            Cmd::Left | Cmd::Back | Cmd::Pending | Cmd::Other(_) => {}
        }
    }

    #[cfg(test)]
    pub(super) fn agents_key(&mut self, k: KeyEvent) -> Flow {
        match k.code {
            KeyCode::Char('J') => {
                self.agent_detail_scroll = keys::by(
                    self.agent_detail_scroll,
                    5,
                    self.agent_view.h,
                    self.agent_view.total,
                )
            }
            KeyCode::Char('K') => {
                self.agent_detail_scroll = keys::by(
                    self.agent_detail_scroll,
                    -5,
                    self.agent_view.h,
                    self.agent_view.total,
                )
            }
            KeyCode::Char('/') => match self.search.as_mut() {
                Some(s) => s.editing = true,
                None => {
                    self.search = Some(Search {
                        input: TextInput::default(),
                        editing: true,
                        typed_at: now_millis(),
                        remote: Remote::Idle,
                    })
                }
            },
            KeyCode::Char('C') if self.orchestrator_selected() => {
                self.recover(Recovery::Acknowledge)
            }
            KeyCode::Char('S') if self.orchestrator_selected() => self.recover(Recovery::Cancel),
            KeyCode::Char('R') if self.orchestrator_selected() => self.recover(Recovery::Retry),
            KeyCode::Char('x') => self.close_agent(),
            KeyCode::Char('r') | KeyCode::Char('m') => self.message_agent(),
            KeyCode::Char('a') | KeyCode::Enter if self.orchestrator_selected() => {
                self.agent_zoom = true
            }
            KeyCode::Char('a') | KeyCode::Enter => {
                if let Some(id) = self.agent_sel.clone() {
                    return Flow::Attach(self.attach_id(&id));
                }
            }
            _ => {}
        }
        Flow::Go
    }

    #[cfg(test)]
    fn message_agent(&mut self) {
        let Some(id) = self.agent_sel.clone() else {
            return;
        };
        let (agent, name) = if self.orchestrator_selected() {
            (None, tr!("the orchestrator", "el orquestador").to_string())
        } else {
            (Some(id.clone()), self.agent_name(Some(&id)))
        };
        let message = ops::Message::to(agent, MessageKind::Message, String::new());
        self.compose(
            Target::Message {
                key: None,
                message,
                name,
                title: String::new(),
            },
            "",
        );
    }

    /// Closing a session cannot be undone, so it takes a second press within a few seconds.
    #[cfg(test)]
    fn close_agent(&mut self) {
        let Some(id) = self.agent_sel.clone() else {
            return;
        };
        if self.orchestrator_selected() {
            self.fail(tr!(
                "Alondra manages the orchestrator automatically",
                "Alondra gestiona el orquestador automáticamente"
            ));
            return;
        }
        let name = self.agent_name(Some(&id));
        if !self.confirmed(
            &format!("close:{id}"),
            trf!(
                "Press x again to close {name} (claude rm, never discards work)",
                "Pulsa x otra vez para cerrar {name} (claude rm, nunca descarta trabajo)"
            ),
        ) {
            return;
        }
        let (path, body) = ops::close(&id, true);
        #[cfg(test)]
        self.posted.borrow_mut().push((path.clone(), body.clone()));
        self.flash(trf!("Closing {name}…", "Cerrando {name}…"));
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(90))
                .post(&path, &body)
                .map_err(|e| format!("{e:#}"))
                .map(|v| {
                    if v.get("interactive").and_then(Value::as_bool) == Some(true) {
                        Closed::LeftFleet
                    } else {
                        Closed::Removed
                    }
                });
            let _ = tx.send(Msg::Closed { name, result });
        });
    }

    /// The orchestrator's recovery: each step takes a second press and names what it risks.
    #[cfg(test)]
    fn recover(&mut self, action: Recovery) {
        let Some(runtime) = self
            .runtime
            .as_ref()
            .and_then(|r| r.orchestrator_state())
            .cloned()
        else {
            return;
        };
        let allowed = match action {
            Recovery::Cancel => runtime.can_cancel,
            Recovery::Acknowledge | Recovery::Retry => runtime.can_retry,
        };
        let Some(run_id) = runtime.active_run_id.filter(|_| allowed) else {
            self.fail(tr!(
                "The orchestrator has nothing to recover now",
                "El orquestador no tiene nada que recuperar ahora"
            ));
            return;
        };
        let (ask, ok) = match action {
            Recovery::Acknowledge => (
                tr!(
                    "Inspect partial effects first. Press C again to continue without repeating this batch.",
                    "Revisa los efectos parciales. Pulsa C otra vez para continuar sin repetir este lote."
                ),
                tr!("Continuing without repeating", "Continuando sin repetir"),
            ),
            Recovery::Cancel => (
                tr!(
                    "Completed actions cannot be undone. Press S again to stop this intervention.",
                    "Las acciones completadas no se deshacen. Pulsa S otra vez para detener esta intervención."
                ),
                tr!(
                    "Orchestrator stop requested",
                    "Parada del orquestador solicitada"
                ),
            ),
            Recovery::Retry => (
                tr!(
                    "Actions may already have completed. Check tasks; press R again to retry.",
                    "Puede haber acciones completadas. Comprueba las tareas; pulsa R otra vez para reintentar."
                ),
                tr!(
                    "Orchestrator retry requested",
                    "Reintento del orquestador solicitado"
                ),
            ),
        };
        if !self.confirmed(&format!("{action:?}:{run_id}"), ask.into()) {
            return;
        }
        let (path, body) = ops::orchestrator(action, &run_id);
        self.op(None, path, body, ok.into());
    }
}

/// What a session last said: its last message, else what the daemon says it needs or does.
pub fn said(n: &FleetNode) -> String {
    n.last_message
        .clone()
        .or(n.daemon_needs.clone())
        .or(n.daemon_detail.clone())
        .unwrap_or_default()
}
