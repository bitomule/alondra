//! The inbox: the daemon's roots and their children, each key mapped to the one semantic command
//! the item offers. Reading is not replying, and authorizing a merge is not a merge.

use std::collections::BTreeMap;
use std::time::Duration;

use alondra_core::domain_v2::{
    AttachmentRef, AvailableAction, Command, DeliveryTarget, InboxItem, InboxSubject, InputKind, InputValue, Interaction, InteractionId, InteractionPayload, MergeAttemptState, MergeAuthorization,
    MergeMethod, PrId, PrState, PullRequest, Revision,
};
use alondra_core::now_millis;
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::Value;

use super::keys::{self, Cmd, Motion};
use super::{App, Flow, Key, Msg, Pane, Part, Screen, Target, md, unavailable};
use crate::client::Client;
use crate::mcp::urlencode;
use crate::ops::{self, MessageKind};

pub fn action<'a>(item: &'a InboxItem, kind: &str) -> Option<&'a AvailableAction> {
    item.available_actions.iter().find(|a| a.kind == kind)
}

pub fn interaction_of(item: &InboxItem) -> Option<&InteractionId> {
    match &item.subject {
        InboxSubject::Interaction { interaction_id } => Some(interaction_id),
        _ => None,
    }
}

/// Where a root is listed: what waits for a decision, the pull requests, and what is only read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Waiting,
    PullRequests,
    ToRead,
}

pub const SECTIONS: [Section; 3] = [Section::Waiting, Section::PullRequests, Section::ToRead];

pub fn section_of(item: &InboxItem) -> Section {
    match item.kind.as_str() {
        "pull_request" => Section::PullRequests,
        "information" => Section::ToRead,
        _ => Section::Waiting,
    }
}

/// A question or action request nobody has to answer any more: answered, or withdrawn.
fn answered(item: &InboxItem) -> bool {
    matches!(item.kind.as_str(), "question" | "action_request") && !item.pending
}

pub fn merge_method(m: MergeMethod) -> &'static str {
    match m {
        MergeMethod::Merge => tr!("merge commit", "commit de fusión"),
        MergeMethod::Squash => "squash",
        MergeMethod::Rebase => "rebase",
    }
}

pub fn pr_state(s: PrState) -> &'static str {
    match s {
        PrState::Open => tr!("open", "abierto"),
        PrState::Closed => tr!("closed", "cerrado"),
        PrState::Merged => tr!("merged", "fusionado"),
    }
}

/// Where an authorization stands; only a merge the agent confirmed reads as merged.
pub fn authorization_state(a: &MergeAuthorization) -> String {
    let method = merge_method(a.method);
    if !a.valid {
        return trf!("merge authorization ({method}) no longer valid", "autorización de fusión ({method}) ya no vale");
    }
    match a.attempt {
        MergeAttemptState::Unclaimed => trf!("merge authorized ({method}) · waiting for the agent", "fusión autorizada ({method}) · pendiente del agente"),
        MergeAttemptState::Claimed => trf!("merge authorized ({method}) · the agent is merging", "fusión autorizada ({method}) · el agente la está haciendo"),
        MergeAttemptState::Uncertain => trf!("merge authorized ({method}) · outcome not confirmed", "fusión autorizada ({method}) · resultado sin confirmar"),
        MergeAttemptState::Confirmed => trf!("merged by the agent ({method})", "fusionado por el agente ({method})"),
        MergeAttemptState::Failed => trf!("merge authorized ({method}) · the merge failed", "fusión autorizada ({method}) · la fusión falló"),
    }
}

fn short(head: &str) -> &str {
    &head[..head.len().min(7)]
}

pub fn pr_line(pr: &PullRequest) -> String {
    let state = pr_state(pr.state);
    format!("{} #{} · {} ← {} · {state}", pr.repository, pr.number, pr.base, short(&pr.head))
}

pub fn pr_url(pr: &PullRequest) -> String {
    format!("https://{}/{}/pull/{}", pr.host, pr.repository, pr.number)
}

impl App {
    /// Every item in the projection, hidden or not.
    pub(super) fn all_keys(&self) -> Vec<Key> {
        let Some(d) = &self.domain else { return vec![] };
        d.inbox.iter().flat_map(|r| std::iter::once(r).chain(&r.children)).map(|i| Key(i.id.clone())).collect()
    }

    /// Roots by section, in the daemon's order inside each, every root followed by its children.
    /// A child is never regrouped here.
    pub fn inbox_rows(&self) -> Vec<(&InboxItem, bool)> {
        let Some(d) = &self.domain else { return vec![] };
        let shown = |i: &InboxItem| !self.hidden.contains_key(&Key(i.id.clone()));
        let mut rows = vec![];
        for section in SECTIONS {
            for root in d.inbox.iter().filter(|r| !answered(r) && shown(r) && section_of(r) == section) {
                rows.push((root, false));
                rows.extend(root.children.iter().filter(|c| shown(c)).map(|c| (c, true)));
            }
        }
        rows
    }

    pub fn inbox_keys(&self) -> Vec<Key> {
        self.inbox_rows().into_iter().map(|(i, _)| Key(i.id.clone())).collect()
    }

    pub fn inbox_count(&self) -> usize {
        self.inbox_rows().into_iter().filter(|(_, nested)| !nested).count()
    }

    pub fn item(&self, key: &Key) -> Option<&InboxItem> {
        let d = self.domain.as_ref()?;
        d.inbox.iter().flat_map(|r| std::iter::once(r).chain(&r.children)).find(|i| i.id == key.0)
    }

    pub fn parent(&self, key: &Key) -> Option<&InboxItem> {
        self.domain.as_ref()?.inbox.iter().find(|r| r.children.iter().any(|c| c.id == key.0))
    }

    pub fn interaction(&self, id: &InteractionId) -> Option<&Interaction> {
        self.interactions.get(id)
    }

    pub fn pull_request(&self, id: &PrId) -> Option<&PullRequest> {
        self.domain.as_ref()?.pull_requests.iter().find(|p| &p.id == id)
    }

    pub fn authorizations(&self, pr: &PrId) -> Vec<&MergeAuthorization> {
        self.domain.as_ref().map(|d| d.merge_authorizations.iter().filter(|a| &a.pr_id == pr).collect()).unwrap_or_default()
    }

    /// The PR an item is about: its own subject, the PR its delivery presents or the PR it was
    /// linked to.
    pub fn item_pr(&self, item: &InboxItem) -> Option<&PullRequest> {
        match &item.subject {
            InboxSubject::PullRequest { pr_id } => self.pull_request(pr_id),
            InboxSubject::Interaction { interaction_id } => {
                let i = self.interaction(interaction_id)?;
                match (&i.payload, &i.subject) {
                    (InteractionPayload::Delivery { targets, .. }, _) => targets.iter().find_map(|t| match t {
                        DeliveryTarget::PullRequest { pr_id, .. } => self.pull_request(pr_id),
                        _ => None,
                    }),
                    (_, Some(alondra_core::domain_v2::InteractionSubject::PullRequest { pr_id })) => self.pull_request(pr_id),
                    _ => None,
                }
            }
            InboxSubject::DocumentReview { .. } => None,
        }
    }

    /// The PR a stacked task goes on top of, while that PR is still open.
    pub fn stacked_on(&self, pr: &PullRequest) -> Option<&PullRequest> {
        let task = self.task(&pr.task_id)?;
        let below = self.pull_request(&task.stack.as_ref()?.pr_id)?;
        (below.state == PrState::Open).then_some(below)
    }

    /// The delivery item that presents a PR, where feedback and authorization for it go.
    pub fn presenting(&self, pr: &PrId) -> Option<&InboxItem> {
        let d = self.domain.as_ref()?;
        d.inbox.iter().flat_map(|r| std::iter::once(r).chain(&r.children)).find(|i| i.kind == "delivery" && self.item_pr(i).is_some_and(|p| &p.id == pr))
    }

    pub(super) fn move_inbox(&mut self, to: isize) {
        let keys = self.inbox_keys();
        if keys.is_empty() {
            return;
        }
        let idx = to.clamp(0, keys.len() as isize - 1) as usize;
        if self.sel.as_ref() != Some(&keys[idx]) {
            self.detail_scroll = 0;
        }
        self.sel_idx = idx;
        self.sel = Some(keys[idx].clone());
        self.settle_until = 0;
    }

    pub(super) fn inbox_cmd(&mut self, cmd: Cmd) {
        let i = self.sel_idx as isize;
        let detail = self.focus == Pane::Detail && self.detail_view.shown;
        match cmd {
            Cmd::Move(m) if detail => self.detail_scroll = keys::scroll(self.detail_scroll, m, self.detail_view.h, self.detail_view.total),
            Cmd::Move(Motion::Down) => self.move_inbox(i + 1),
            Cmd::Move(Motion::Up) => self.move_inbox(i - 1),
            Cmd::Move(Motion::Top) => self.move_inbox(0),
            Cmd::Move(Motion::Bottom) => self.move_inbox(isize::MAX),
            Cmd::Move(m) => self.detail_scroll = keys::scroll(self.detail_scroll, m, self.detail_view.h, self.detail_view.total),
            Cmd::Right if self.detail_view.shown && self.sel.is_some() => self.focus = Pane::Detail,
            Cmd::Right | Cmd::Expand => self.open_reader(),
            Cmd::Left | Cmd::Back => self.focus = Pane::List,
            Cmd::Pending | Cmd::Other(_) => {}
        }
    }

    pub(super) fn inbox_key(&mut self, k: KeyEvent) -> Flow {
        let Some(key) = self.sel.clone() else { return Flow::Go };
        let pr_root = self.item(&key).filter(|i| i.kind == "pull_request").and_then(|i| self.item_pr(i)).map(|p| p.id.clone());
        match k.code {
            KeyCode::Char('J') => self.detail_scroll = keys::by(self.detail_scroll, 5, self.detail_view.h, self.detail_view.total),
            KeyCode::Char('K') => self.detail_scroll = keys::by(self.detail_scroll, -5, self.detail_view.h, self.detail_view.total),
            KeyCode::Enter => self.open_reader(),
            KeyCode::Char('a') => return self.terminal_of(&key),
            KeyCode::Char('r') => self.reply(&key),
            KeyCode::Char('c') => self.more_context(&key),
            KeyCode::Char('p') => self.ask_about_pr(&key),
            KeyCode::Char('v') => self.accept_or_open(&key),
            KeyCode::Char('x') if pr_root.is_none() => self.mark_read(&key),
            KeyCode::Char('w') => match self.item(&key).and_then(|i| self.item_pr(i)).map(pr_url) {
                Some(url) => super::open_with_mac(&url),
                None => self.fail(tr!("There is no PR here", "Aquí no hay PR")),
            },
            KeyCode::Char('d') => return self.review_pr(&key),
            KeyCode::Char('1') if k.modifiers.is_empty() && pr_root.as_ref().is_some_and(|pr| self.conflicts(pr)) => {
                if let Some(pr) = pr_root {
                    self.resolve_conflicts(&key, &pr);
                }
            }
            KeyCode::Char(c @ '1'..='9') if k.modifiers.is_empty() => self.choose(&key, c as usize - '1' as usize),
            _ => {}
        }
        Flow::Go
    }

    /// The runtime says the PR's branch conflicts with its base.
    pub fn conflicts(&self, pr: &PrId) -> bool {
        self.runtime.as_ref().and_then(|r| r.pull_request(&pr.0)).is_some_and(|p| p.conflicts)
    }

    /// The agent's terminal; an item of the orchestrator opens its activity instead.
    fn terminal_of(&mut self, key: &Key) -> Flow {
        match self.item(key).map(|i| i.agent_id.clone()) {
            Some(Some(agent)) => Flow::Attach(self.attach_id(&agent)),
            Some(None) => {
                self.open_orchestrator();
                Flow::Go
            }
            None => Flow::Go,
        }
    }

    /// A root only groups; its children carry the actions, so a key never acts on a PR for a child.
    fn actionable(&mut self, key: &Key) -> Option<InboxItem> {
        let item = self.item(key)?.clone();
        if !matches!(item.subject, InboxSubject::Interaction { .. }) {
            self.fail(tr!("Select the item under it: each one has its own actions", "Selecciona el elemento de debajo: cada uno tiene sus acciones"));
            return None;
        }
        Some(item)
    }

    fn enabled(&mut self, item: &InboxItem, kind: &str) -> Option<AvailableAction> {
        match action(item, kind) {
            Some(a) if a.enabled => Some(a.clone()),
            Some(a) => {
                let why = unavailable(a);
                self.fail(why);
                None
            }
            None => {
                self.fail(tr!("That is not available on this item", "Eso no está disponible en este elemento"));
                None
            }
        }
    }

    /// Marks only this information read. Nothing reaches the agent.
    pub(super) fn mark_read(&mut self, key: &Key) {
        let Some(item) = self.actionable(key) else { return };
        let Some(a) = self.enabled(&item, "mark_read") else { return };
        let Some(id) = interaction_of(&item).cloned() else { return };
        let title = md::truncate(&md::one_line(item.body.as_deref().unwrap_or(&item.title)), 60);
        self.send(format!("read:{id}"), Some(key.clone()), a.expected_revision, Command::MarkRead { interaction_id: id }, trf!("Read: {title}", "Leído: {title}"), None);
        self.reader = None;
        self.hide(key.clone());
    }

    /// Confirms the delivery as presented. The agent still declares the task finished itself.
    pub(super) fn accept(&mut self, key: &Key) {
        let Some(item) = self.actionable(key) else { return };
        let Some(a) = self.enabled(&item, "accept_delivery") else { return };
        let Some((presentation, targets, _)) = self.feedback_subject(key) else { return };
        if self.reviews.get(key).is_some_and(|r| !r.is_empty()) {
            self.fail(tr!("Your comments are not sent with an acceptance: s sends them as feedback", "Tus comentarios no van con la aceptación: s los envía como feedback"));
            return;
        }
        let command = Command::AcceptDelivery { presentation_id: presentation.clone(), targets };
        self.send(format!("accept:{presentation}"), Some(key.clone()), a.expected_revision, command, tr!("Delivery accepted · waiting for the agent to finish", "Entrega aceptada · pendiente de que el agente termine").into(), None);
        self.reader = None;
    }

    /// v accepts a delivery; on anything else it opens the item's images and files with the
    /// Mac's app, since the terminal only shows text.
    pub(super) fn accept_or_open(&mut self, key: &Key) {
        if self.item(key).is_some_and(|i| action(i, "accept_delivery").is_some()) {
            return self.accept(key);
        }
        let Some(item) = self.item(key).cloned() else { return };
        let files: Vec<(String, String)> = self.item_files(&item).into_iter().filter(|(_, route)| !matches!(self.files.get(route), Some(Ok(super::files::FileBody::Text(_))))).collect();
        if files.is_empty() {
            self.fail(tr!("No image or file to open here", "Aquí no hay imagen ni fichero que abrir"));
            return;
        }
        self.spawn(move |tx| {
            for (name, route) in files {
                if let Err(e) = super::open_stored(&name, &route) {
                    let _ = tx.send(Msg::Posted { key: None, result: Err(format!("{name}: {e:#}")) });
                }
            }
        });
        self.flash(tr!("Opened with its app", "Abierto con su aplicación"));
    }

    /// The message a reply to this item is: to its agent, about it.
    pub(super) fn reply_to(&mut self, item: &InboxItem, kind: MessageKind) -> Option<ops::Message> {
        let Some(agent) = item.agent_id.clone() else {
            let mut m = ops::Message::to(None, MessageKind::Message, String::new());
            m.interaction = interaction_of(item).cloned();
            return Some(m);
        };
        let mut m = ops::Message::to(Some(agent), kind, String::new());
        m.interaction = interaction_of(item).cloned();
        m.mark_read = item.kind == "information" && item.pending;
        Some(m)
    }

    fn compose_message(&mut self, key: &Key, item: &InboxItem, message: ops::Message) {
        let name = if message.agent.is_some() { self.agent_name(item.agent_id.as_ref()) } else { tr!("the orchestrator", "el orquestador").into() };
        let title = md::one_line(item.body.as_deref().filter(|b| !b.trim().is_empty()).unwrap_or(&item.title));
        self.compose(Target::Message { key: Some(key.clone()), message, name, title }, "");
    }

    fn reply(&mut self, key: &Key) {
        if let Some(pr) = self.item(key).filter(|i| i.kind == "pull_request").and_then(|i| self.item_pr(i)).map(|p| p.id.clone()) {
            return self.request_changes(key, &pr);
        }
        let Some(item) = self.actionable(key) else { return };
        match item.kind.as_str() {
            "question" | "action_request" => {
                let Some(i) = self.options(&item) else { return };
                match i.iter().position(|o| o.inputs.iter().any(|s| s.kind == InputKind::Text)) {
                    Some(n) => self.choose(key, n),
                    None => {
                        let Some(m) = self.reply_to(&item, MessageKind::Reply) else { return };
                        self.compose_message(key, &item, m);
                    }
                }
            }
            "delivery" => {
                let Some(a) = self.enabled(&item, "submit_feedback") else { return };
                let Some((presentation, targets, _)) = self.feedback_subject(key) else { return };
                self.compose(Target::Feedback { key: key.clone(), presentation, targets, title: item.title.clone(), revision: a.expected_revision }, "");
            }
            _ => {
                let Some(m) = self.reply_to(&item, MessageKind::Reply) else { return };
                self.compose_message(key, &item, m);
            }
        }
    }

    /// Sends a question back to its agent asking for more context; it stays open until the agent asks again.
    fn more_context(&mut self, key: &Key) {
        let Some(item) = self.actionable(key) else { return };
        if !matches!(item.kind.as_str(), "question" | "action_request") {
            self.fail(tr!("Only a question can be sent back for more context", "Solo una pregunta se puede devolver pidiendo más contexto"));
            return;
        }
        let Some(m) = self.reply_to(&item, MessageKind::MoreContext) else { return };
        self.compose_message(key, &item, m);
    }

    /// The PR's agent, with the PR the message is about.
    fn pr_message(&self, pr: &PrId, kind: MessageKind) -> Option<ops::Message> {
        let agent = self.pull_request(pr).and_then(|p| self.task(&p.task_id)).and_then(|t| t.agent_id.clone())?;
        let mut m = ops::Message::to(Some(agent), kind, String::new());
        m.pr = Some(pr.clone());
        Some(m)
    }

    fn ask_about_pr(&mut self, key: &Key) {
        let Some(item) = self.item(key).cloned() else { return };
        let Some(pr) = self.item_pr(&item).map(|p| p.id.clone()) else {
            self.fail(tr!("There is no PR here", "Aquí no hay PR"));
            return;
        };
        match self.pr_message(&pr, MessageKind::PrQuestion) {
            Some(m) => self.compose_message(key, &item, m),
            None => self.fail(tr!("This PR's task has no agent to ask", "La tarea de este PR no tiene agente al que preguntar")),
        }
    }

    /// r on a PR: feedback on the delivery that presents it, or a change request to its agent
    /// when nothing presents it.
    fn request_changes(&mut self, key: &Key, pr: &PrId) {
        if let Some(delivery) = self.presenting(pr).filter(|d| action(d, "submit_feedback").is_some_and(|a| a.enabled)).map(|d| Key(d.id.clone())) {
            self.sel = Some(delivery.clone());
            return self.reply(&delivery);
        }
        let Some(item) = self.item(key).cloned() else { return };
        match self.pr_message(pr, MessageKind::PrChanges) {
            Some(m) => self.compose_message(key, &item, m),
            None => self.fail(tr!("This PR's task has no agent to ask", "La tarea de este PR no tiene agente al que preguntar")),
        }
    }

    /// Comments on a PR (the reader's or tuicr's) go as feedback on the delivery presenting it,
    /// else as a change request to its agent.
    pub(super) fn pr_feedback(&mut self, key: &Key, pr: &PrId, text: String, attachments: Vec<AttachmentRef>) {
        let presenting = self.presenting(pr).filter(|d| action(d, "submit_feedback").is_some_and(|a| a.enabled)).map(|d| Key(d.id.clone()));
        if let Some(delivery) = presenting
            && let Some((presentation, targets, revision)) = self.feedback_subject(&delivery)
        {
            return self.feedback(delivery, presentation, targets, text, attachments, revision, None);
        }
        let Some(mut m) = self.pr_message(pr, MessageKind::PrChanges) else {
            self.fail(tr!("This PR's task has no agent to ask", "La tarea de este PR no tiene agente al que preguntar"));
            return;
        };
        m.text = text;
        m.attachments = attachments;
        let name = self.pull_request(pr).and_then(|p| self.task(&p.task_id)).map(|t| self.agent_name(t.agent_id.as_ref())).unwrap_or_default();
        self.message(Some(key.clone()), m, trf!("Changes requested from {name}", "Cambios pedidos a {name}"));
    }

    fn resolve_conflicts(&mut self, key: &Key, pr: &PrId) {
        let number = self.pull_request(pr).map_or(0, |p| p.number);
        let (path, body) = ops::resolve_conflicts(pr);
        self.op(Some(key.clone()), path, body, trf!("Asked its agent to resolve the conflicts of PR #{number}", "Pedido a su agente que resuelva los conflictos del PR #{number}"));
        self.hide(key.clone());
    }

    /// d reviews the PR in tuicr on its local clone; remotely, or with no clone, its diff opens here.
    fn review_pr(&mut self, key: &Key) -> Flow {
        let Some(pr) = self.item(key).and_then(|i| self.item_pr(i)).cloned() else {
            self.fail(tr!("There is no PR here", "Aquí no hay PR"));
            return Flow::Go;
        };
        if crate::service_client::target().is_none() {
            let cwd = self.task(&pr.task_id).and_then(|t| t.agent_id.as_ref()).and_then(|a| self.runtime.as_ref()?.node(a)).and_then(|n| n.cwd.clone());
            if let Some(dir) = super::clone_of(cwd.as_deref(), &pr.repository) {
                let number = pr.number;
                self.flash(trf!("Opening PR #{number} in tuicr…", "Abriendo el PR #{number} en tuicr…"));
                return Flow::Review { key: key.clone(), pr: pr.id, number, dir };
            }
        }
        self.load_diff(key);
        Flow::Go
    }

    pub(super) fn options(&mut self, item: &InboxItem) -> Option<Vec<alondra_core::domain_v2::QuestionOption>> {
        let id = interaction_of(item)?;
        match &self.interaction(id)?.payload {
            InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => Some(options.clone()),
            _ => None,
        }
    }

    /// A number answers the question it is pressed on, or authorizes a merge method on a delivery.
    pub(super) fn choose(&mut self, key: &Key, i: usize) {
        if now_millis() < self.settle_until {
            self.fail(tr!("The selection just changed: check which one it is and press again", "La selección acaba de cambiar: mira cuál es y vuelve a pulsar"));
            return;
        }
        let Some(item) = self.actionable(key) else { return };
        match item.kind.as_str() {
            "question" | "action_request" => self.choose_option(key, &item, i),
            "delivery" => self.authorize(key, &item, i),
            _ => self.fail(tr!("This item has no numbered options", "Este elemento no tiene opciones numeradas")),
        }
    }

    fn choose_option(&mut self, key: &Key, item: &InboxItem, i: usize) {
        let Some(a) = self.enabled(item, "answer_question") else { return };
        let Some(options) = self.options(item) else { return };
        let Some(option) = options.get(i) else {
            let n = i + 1;
            self.fail(trf!("This question has no option {n}", "Esta pregunta no tiene opción {n}"));
            return;
        };
        let Some(interaction) = interaction_of(item).cloned() else { return };
        let text = option.inputs.iter().find(|s| s.kind == InputKind::Text);
        if option.inputs.iter().any(|s| s.required && s.kind != InputKind::Text) {
            self.fail(tr!("This option asks for values the terminal cannot fill yet", "Esta opción pide valores que el terminal aún no sabe rellenar"));
            return;
        }
        if let Some(input) = text {
            let target = Target::Answer {
                key: key.clone(),
                interaction,
                option: option.id.clone(),
                label: option.label.clone(),
                input: Some(input.id.clone()),
                required: input.required,
                title: item.title.clone(),
                revision: a.expected_revision,
            };
            self.compose(target, "");
            return;
        }
        self.answer(key.clone(), interaction, option.id.clone(), option.label.clone(), BTreeMap::new(), vec![], a.expected_revision, None);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn answer(&mut self, key: Key, interaction: InteractionId, option: String, label: String, inputs: BTreeMap<String, InputValue>, attachments: Vec<AttachmentRef>, revision: Revision, draft: Option<(String, String)>) {
        let agent = self.item(&key).map(|i| self.agent_name(i.agent_id.as_ref())).unwrap_or_default();
        let slot = format!("answer:{interaction}:{option}:{}:{}", serde_json::to_string(&inputs).unwrap_or_default(), attachments.len());
        let command = Command::AnswerQuestion { attachments, interaction_id: interaction, option_id: option, inputs };
        self.send(slot, Some(key.clone()), revision, command, format!("«{label}» → {agent}"), draft);
        self.reader = None;
        self.hide(key);
    }

    /// The delivery a feedback or an acceptance is about, exactly as it was presented.
    pub(super) fn feedback_subject(&mut self, key: &Key) -> Option<(InteractionId, Vec<DeliveryTarget>, Revision)> {
        let item = self.item(key)?.clone();
        let id = interaction_of(&item)?.clone();
        let revision = action(&item, "submit_feedback").map(|a| a.expected_revision)?;
        match &self.interaction(&id)?.payload {
            InteractionPayload::Delivery { targets, .. } => Some((id, targets.clone(), revision)),
            _ => {
                self.fail(tr!("Only a delivery takes feedback", "Solo una entrega admite feedback"));
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn feedback(&mut self, key: Key, presentation: InteractionId, targets: Vec<DeliveryTarget>, text: String, attachments: Vec<AttachmentRef>, revision: Revision, draft: Option<(String, String)>) {
        let agent = self.item(&key).map(|i| self.agent_name(i.agent_id.as_ref())).unwrap_or_default();
        let slot = format!("feedback:{presentation}:{text}:{}", attachments.len());
        let command = Command::SubmitFeedback { attachments, presentation_id: presentation, targets, body: text };
        self.send(slot, Some(key), revision, command, trf!("Feedback sent to {agent}", "Feedback enviado a {agent}"), draft);
    }

    /// Records the user's authorization for the agent to merge. It never merges and never hides
    /// the PR: its state changes only when the merge is confirmed.
    fn authorize(&mut self, key: &Key, item: &InboxItem, i: usize) {
        let Some(a) = self.enabled(item, "authorize_merge") else { return };
        let Some(pr) = self.item_pr(item).cloned() else { return };
        let Some(&method) = pr.allowed_methods.get(i) else {
            let n = i + 1;
            self.fail(trf!("This PR has no option {n}", "Este PR no tiene opción {n}"));
            return;
        };
        let (Some(presentation), Some(head)) = (a.presentation_id.clone(), a.expected_head.clone()) else { return };
        let label = merge_method(method);
        let number = pr.number;
        let slot = format!("authorize:{presentation}:{}:{head}:{label}", pr.id);
        let n = i + 1;
        if !self.sending.contains_key(&slot) && !self.confirmed(&slot, trf!("Press {n} again to authorize the merge of PR #{number} ({label})", "Pulsa {n} otra vez para autorizar la fusión del PR #{number} ({label})")) {
            return;
        }
        let command = Command::AuthorizeMerge { authorization_id: crate::client::new_id("authorization").into(), presentation_id: presentation, pr_id: pr.id.clone(), expected_head: head, method };
        let ok = trf!("Merge of PR #{number} authorized ({label}) · the agent merges it", "Fusión del PR #{number} autorizada ({label}) · la hace el agente");
        self.send(slot, Some(key.clone()), a.expected_revision, command, ok, None);
    }

    pub(super) fn load_diff(&mut self, key: &Key) {
        let Some(pr) = self.item(key).and_then(|i| self.item_pr(i)).map(|p| p.id.clone()) else {
            self.fail(tr!("There is no PR here", "Aquí no hay PR"));
            return;
        };
        let key = key.clone();
        self.flash(tr!("Loading the PR diff…", "Cargando el diff del PR…"));
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(90)).get::<Value>(&ops::pull_request_diff(&pr)).map_err(|e| e.to_string());
            let _ = tx.send(Msg::Diff { key, result });
        });
    }

    /// The stored files an item shows: its attachments and the document versions it is about.
    pub fn item_files(&self, item: &InboxItem) -> Vec<(String, String)> {
        let d = self.domain.as_ref();
        let document = |document_id: &alondra_core::domain_v2::DocumentId, revision_id: &alondra_core::domain_v2::DocumentRevisionId| {
            let title = d.and_then(|d| d.documents.iter().find(|x| &x.id == document_id)).map_or(document_id.0.clone(), |x| x.title.clone());
            let number = d.and_then(|d| d.document_revisions.iter().find(|r| &r.id == revision_id)).map_or(0, |r| r.number);
            (trf!("{title} · version {number}", "{title} · versión {number}"), crate::client::document_content(&revision_id.0))
        };
        let mut files = vec![];
        match &item.subject {
            InboxSubject::Interaction { interaction_id } => {
                if let Some(i) = self.interaction(interaction_id) {
                    files.extend(i.attachments.iter().map(|a| (a.name.clone(), crate::client::attachment_content(a))));
                    if let InteractionPayload::Delivery { targets, .. } = &i.payload {
                        files.extend(targets.iter().filter_map(|t| match t {
                            DeliveryTarget::DocumentRevision { document_id, revision_id } => Some(document(document_id, revision_id)),
                            _ => None,
                        }));
                    }
                }
            }
            InboxSubject::DocumentReview { document_id, revision_id, .. } => files.push(document(document_id, revision_id)),
            InboxSubject::PullRequest { .. } => {}
        }
        files
    }

    /// What the reader shows for an item: its text, then each of its files.
    pub(super) fn item_parts(&self, item: &InboxItem) -> Vec<Part> {
        let mut parts: Vec<Part> = self.item_text(item).into_iter().map(Part::Text).collect();
        parts.extend(self.item_files(item).into_iter().map(|(name, route)| Part::File { name, route }));
        parts
    }

    /// The item as Markdown: what it says, what it presents and where its decisions stand.
    pub fn item_text(&self, item: &InboxItem) -> Vec<String> {
        let mut parts = vec![format!("# {}", item.title)];
        if let Some(t) = item.task_id.as_ref().and_then(|id| self.task(id)) {
            let (state, title) = (super::tasks::state_label(t.state), md::one_line(&t.title));
            parts.push(trf!("Task: **{title}** · {state}", "Tarea: **{title}** · {state}"));
        }
        let full = interaction_of(item).and_then(|id| self.interaction(id)).map(|i| &i.body);
        if let Some(body) = full.or(item.body.as_ref()).filter(|b| !b.trim().is_empty()) {
            parts.push(body.clone());
        }
        match &item.subject {
            InboxSubject::Interaction { interaction_id } => {
                if let Some(i) = self.interaction(interaction_id) {
                    parts.extend(self.interaction_text(i));
                }
            }
            InboxSubject::PullRequest { pr_id } => {
                if let Some(pr) = self.pull_request(pr_id) {
                    parts.push(pr_line(pr));
                    if let Some(below) = self.stacked_on(pr) {
                        let n = below.number;
                        parts.push(trf!("Goes on top of PR #{n}, which is still open", "Va encima del PR #{n}, que sigue abierto"));
                    }
                    parts.push(pr_url(pr));
                    for a in self.authorizations(pr_id) {
                        parts.push(authorization_state(a));
                    }
                }
            }
            InboxSubject::DocumentReview { document_id, revision_id, .. } => {
                let d = self.domain.as_ref();
                let title = d.and_then(|d| d.documents.iter().find(|x| &x.id == document_id)).map_or(document_id.0.clone(), |x| x.title.clone());
                let number = d.and_then(|d| d.document_revisions.iter().find(|r| &r.id == revision_id)).map_or(0, |r| r.number);
                parts.push(trf!("Document **{title}** · version {number}", "Documento **{title}** · versión {number}"));
            }
        }
        parts
    }

    fn interaction_text(&self, i: &Interaction) -> Vec<String> {
        let mut parts = vec![];
        match &i.payload {
            InteractionPayload::Information { read_by } => {
                if !read_by.is_empty() {
                    parts.push(tr!("Read.", "Leído.").into());
                }
            }
            InteractionPayload::Question { options, answer } | InteractionPayload::ActionRequest { options, answer } => {
                let list: Vec<String> = options
                    .iter()
                    .enumerate()
                    .map(|(n, o)| {
                        let asks = if o.inputs.iter().any(|s| s.kind == InputKind::Text) { tr!(" (asks you to write)", " (pide que escribas)") } else { "" };
                        format!("{}. {}{asks}", n + 1, o.label)
                    })
                    .collect();
                if !list.is_empty() {
                    parts.push(list.join("\n"));
                }
                if let Some(a) = answer {
                    let label = options.iter().find(|o| o.id == a.option_id).map_or(a.option_id.as_str(), |o| o.label.as_str());
                    parts.push(trf!("Answered: {label}", "Contestada: {label}"));
                }
            }
            InteractionPayload::Delivery { targets, acceptance, feedback, is_final } => {
                parts.push(if *is_final { tr!("## Final delivery", "## Entrega final") } else { tr!("## Delivery", "## Entrega") }.into());
                for t in targets {
                    parts.push(match t {
                        DeliveryTarget::Text { text } => text.clone(),
                        DeliveryTarget::DocumentRevision { document_id, revision_id } => {
                            let d = self.domain.as_ref();
                            let title = d.and_then(|d| d.documents.iter().find(|x| &x.id == document_id)).map_or(document_id.0.clone(), |x| x.title.clone());
                            let number = d.and_then(|d| d.document_revisions.iter().find(|r| &r.id == revision_id)).map_or(0, |r| r.number);
                            trf!("Document **{title}** · version {number}", "Documento **{title}** · versión {number}")
                        }
                        DeliveryTarget::PullRequest { pr_id, head } => match self.pull_request(pr_id) {
                            Some(pr) => {
                                let mut line = format!("PR {} #{} @ {} · {}", pr.repository, pr.number, short(head), pr_state(pr.state));
                                if &pr.head != head {
                                    line.push_str(tr!(" · the PR moved since", " · el PR ha cambiado desde entonces"));
                                }
                                line
                            }
                            None => format!("PR {pr_id}"),
                        },
                    });
                }
                if acceptance.as_ref().is_some_and(|a| a.valid) {
                    parts.push(tr!("Accepted · waiting for the agent to finish", "Aceptada · pendiente de que el agente termine").into());
                }
                if let Some(pr) = targets.iter().find_map(|t| match t {
                    DeliveryTarget::PullRequest { pr_id, .. } => Some(pr_id),
                    _ => None,
                }) {
                    parts.extend(self.authorizations(pr).into_iter().map(authorization_state));
                }
                if !feedback.is_empty() {
                    let n = feedback.len();
                    parts.push(trf!("## Feedback sent ({n})", "## Feedback enviado ({n})"));
                    parts.extend(feedback.iter().map(|f| {
                        let clips: Vec<String> = f.attachments.iter().map(|a| format!("📎 {}", a.name)).collect();
                        if clips.is_empty() { f.body.clone() } else { format!("{}\n\n{}", f.body, clips.join(" · ")) }
                    }));
                }
            }
        }
        parts
    }

    /// The Agents screen with the orchestrator selected and its activity open.
    pub(super) fn open_orchestrator(&mut self) {
        let Some(id) = self.runtime.as_ref().and_then(|r| r.orchestrator_node()).map(|n| alondra_core::domain_v2::AgentId(n.session_id.clone())) else {
            self.fail(tr!("Alondra manages the orchestrator automatically; its activity shows here once alondrad reports it", "Alondra gestiona el orquestador automáticamente; su actividad aparece aquí cuando alondrad la informa"));
            return;
        };
        self.reader = None;
        self.show(Screen::Fleet);
        self.agent_sel = Some(id);
        self.agent_zoom = true;
        self.threads_asked = None;
        self.load_threads();
    }
}
