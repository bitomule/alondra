//! The Agents screen: each Claude Code instance, whether it is connected, and the tasks assigned
//! to it. Finishing one task never finishes the agent or its other tasks.

use alondra_core::domain_v2::{Agent, AgentId, Task};
use crossterm::event::{KeyCode, KeyEvent};

use super::keys::{self, Cmd, Motion};
use super::{App, Flow, Pane, Search, input::TextInput};

impl App {
    /// Agents by name, narrowed by the search when there is one.
    pub fn agent_list(&self) -> Vec<&Agent> {
        let Some(d) = &self.domain else { return vec![] };
        let q = self.search.as_ref().map(Search::query).unwrap_or_default();
        let mut agents: Vec<&Agent> = d.agents.iter().filter(|a| q.is_empty() || a.name.to_lowercase().contains(&q) || a.id.0.to_lowercase().contains(&q)).collect();
        agents.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
        agents
    }

    /// The tasks assigned to an agent that are not archived.
    pub fn agent_tasks(&self, id: &AgentId) -> Vec<&Task> {
        self.domain.as_ref().map(|d| d.tasks.iter().filter(|t| t.agent_id.as_ref() == Some(id) && !t.archived).collect()).unwrap_or_default()
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        let id = self.agent_sel.as_ref()?;
        self.domain.as_ref()?.agents.iter().find(|a| &a.id == id)
    }

    pub(super) fn reconcile_agents(&mut self) {
        let ids: Vec<AgentId> = self.agent_list().into_iter().map(|a| a.id.clone()).collect();
        match self.agent_sel.as_ref().and_then(|s| ids.iter().position(|x| x == s)) {
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

    pub(super) fn move_agents(&mut self, to: isize) {
        let ids: Vec<AgentId> = self.agent_list().into_iter().map(|a| a.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let idx = to.clamp(0, ids.len() as isize - 1) as usize;
        if self.agent_sel.as_ref() != Some(&ids[idx]) {
            self.agent_detail_scroll = 0;
        }
        self.agent_idx = idx;
        self.agent_sel = Some(ids[idx].clone());
    }

    pub(super) fn agents_cmd(&mut self, cmd: Cmd) {
        let i = self.agent_idx as isize;
        let detail = self.agent_zoom || (self.focus == Pane::Detail && self.agent_view.shown);
        match cmd {
            Cmd::Move(m) if detail => self.agent_detail_scroll = keys::scroll(self.agent_detail_scroll, m, self.agent_view.h, self.agent_view.total),
            Cmd::Move(Motion::Down) => self.move_agents(i + 1),
            Cmd::Move(Motion::Up) => self.move_agents(i - 1),
            Cmd::Move(Motion::Top) => self.move_agents(0),
            Cmd::Move(Motion::Bottom) => self.move_agents(isize::MAX),
            Cmd::Move(m) => self.agent_detail_scroll = keys::scroll(self.agent_detail_scroll, m, self.agent_view.h, self.agent_view.total),
            Cmd::Right if self.agent_view.shown && !self.agent_zoom => self.focus = Pane::Detail,
            Cmd::Right => self.agent_zoom = self.agent_sel.is_some(),
            Cmd::Expand => self.agent_zoom = !self.agent_zoom && self.agent_sel.is_some(),
            Cmd::Left | Cmd::Back if self.agent_zoom => self.agent_zoom = false,
            Cmd::Left | Cmd::Back if self.focus == Pane::Detail => self.focus = Pane::List,
            Cmd::Back if self.search.is_some() => self.search = None,
            Cmd::Left | Cmd::Back | Cmd::Pending | Cmd::Other(_) => {}
        }
    }

    pub(super) fn agents_key(&mut self, k: KeyEvent) -> Flow {
        match k.code {
            KeyCode::Char('J') => self.agent_detail_scroll = keys::by(self.agent_detail_scroll, 5, self.agent_view.h, self.agent_view.total),
            KeyCode::Char('K') => self.agent_detail_scroll = keys::by(self.agent_detail_scroll, -5, self.agent_view.h, self.agent_view.total),
            KeyCode::Char('/') => match self.search.as_mut() {
                Some(s) => s.editing = true,
                None => self.search = Some(Search { input: TextInput::default(), editing: true }),
            },
            KeyCode::Char('a') | KeyCode::Enter => {
                if let Some(id) = self.agent_sel.clone() {
                    return Flow::Attach(id.0);
                }
            }
            _ => {}
        }
        Flow::Go
    }
}
