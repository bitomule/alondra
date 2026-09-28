//! The inbox: the daemon's roots and their children, each key mapped to the one semantic command
//! the item offers. Reading is not replying, and authorizing a merge is not a merge.

use std::collections::BTreeMap;
use std::time::Duration;

use alondra_core::domain_v2::{
    AvailableAction, Command, DeliveryTarget, InboxItem, InboxSubject, InputKind, InputValue, Interaction, InteractionId, InteractionPayload, MergeAttemptState, MergeAuthorization, MergeMethod, PrId,
    PrState, PullRequest, Revision,
};
use alondra_core::now_millis;
use crossterm::event::{KeyCode, KeyEvent};
use serde_json::Value;

use super::keys::{self, Cmd, Motion};
use super::{App, Flow, Key, Msg, Pane, Target, md, unavailable};
use crate::client::Client;
use crate::mcp::urlencode;

pub fn action<'a>(item: &'a InboxItem, kind: &str) -> Option<&'a AvailableAction> {
    item.available_actions.iter().find(|a| a.kind == kind)
}

pub fn interaction_of(item: &InboxItem) -> Option<&InteractionId> {
    match &item.subject {
        InboxSubject::Interaction { interaction_id } => Some(interaction_id),
        _ => None,
    }
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

    /// Roots in the daemon's order, each followed by its children. A child is never regrouped here.
    pub fn inbox_rows(&self) -> Vec<(&InboxItem, bool)> {
        let Some(d) = &self.domain else { return vec![] };
        let shown = |i: &InboxItem| !self.hidden.contains_key(&Key(i.id.clone()));
        let mut rows = vec![];
        for root in d.inbox.iter().filter(|r| shown(r)) {
            rows.push((root, false));
            rows.extend(root.children.iter().filter(|c| shown(c)).map(|c| (c, true)));
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
        self.domain.as_ref()?.interactions.iter().find(|i| &i.id == id)
    }

    pub fn pull_request(&self, id: &PrId) -> Option<&PullRequest> {
        self.domain.as_ref()?.pull_requests.iter().find(|p| &p.id == id)
    }

    pub fn authorizations(&self, pr: &PrId) -> Vec<&MergeAuthorization> {
        self.domain.as_ref().map(|d| d.merge_authorizations.iter().filter(|a| &a.pr_id == pr).collect()).unwrap_or_default()
    }

    /// The PR an item is about: its own subject, or the PR its delivery presents.
    pub fn item_pr(&self, item: &InboxItem) -> Option<&PullRequest> {
        match &item.subject {
            InboxSubject::PullRequest { pr_id } => self.pull_request(pr_id),
            InboxSubject::Interaction { interaction_id } => match &self.interaction(interaction_id)?.payload {
                InteractionPayload::Delivery { targets, .. } => targets.iter().find_map(|t| match t {
                    DeliveryTarget::PullRequest { pr_id, .. } => self.pull_request(pr_id),
                    _ => None,
                }),
                _ => None,
            },
            InboxSubject::DocumentReview { .. } => None,
        }
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
        match k.code {
            KeyCode::Char('J') => self.detail_scroll = keys::by(self.detail_scroll, 5, self.detail_view.h, self.detail_view.total),
            KeyCode::Char('K') => self.detail_scroll = keys::by(self.detail_scroll, -5, self.detail_view.h, self.detail_view.total),
            KeyCode::Enter => self.open_reader(),
            KeyCode::Char('a') => {
                if let Some(agent) = self.item(&key).and_then(|i| i.agent_id.clone()) {
                    return Flow::Attach(agent.0);
                }
            }
            KeyCode::Char('r') => self.reply(&key),
            KeyCode::Char('v') => self.accept(&key),
            KeyCode::Char('x') => self.mark_read(&key),
            KeyCode::Char('w') => match self.item(&key).and_then(|i| self.item_pr(i)).map(pr_url) {
                Some(url) => open_with_mac(&url),
                None => self.fail(tr!("There is no PR here", "Aquí no hay PR")),
            },
            KeyCode::Char('d') => self.load_diff(&key),
            KeyCode::Char(c @ '1'..='9') if k.modifiers.is_empty() => self.choose(&key, c as usize - '1' as usize),
            _ => {}
        }
        Flow::Go
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

    fn reply(&mut self, key: &Key) {
        let Some(item) = self.actionable(key) else { return };
        match item.kind.as_str() {
            "question" | "action_request" => {
                let Some(i) = self.options(&item) else { return };
                match i.iter().position(|o| o.inputs.iter().any(|s| s.kind == InputKind::Text)) {
                    Some(n) => self.choose(key, n),
                    None => self.fail(tr!("Choose one of its options with its number", "Elige una de sus opciones con su número")),
                }
            }
            "delivery" => {
                let Some(a) = self.enabled(&item, "submit_feedback") else { return };
                let Some((presentation, targets, _)) = self.feedback_subject(key) else { return };
                self.compose(Target::Feedback { key: key.clone(), presentation, targets, title: item.title.clone(), revision: a.expected_revision }, "");
            }
            _ => self.fail(tr!("Information is only read: it has no reply", "La información solo se lee: no tiene respuesta")),
        }
    }

    fn options(&mut self, item: &InboxItem) -> Option<Vec<alondra_core::domain_v2::QuestionOption>> {
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
        self.answer(key.clone(), interaction, option.id.clone(), option.label.clone(), BTreeMap::new(), a.expected_revision, None);
    }

    pub(super) fn answer(&mut self, key: Key, interaction: InteractionId, option: String, label: String, inputs: BTreeMap<String, InputValue>, revision: Revision, draft: Option<(String, String)>) {
        let agent = self.item(&key).map(|i| self.agent_name(i.agent_id.as_ref())).unwrap_or_default();
        let slot = format!("answer:{interaction}:{option}:{}", serde_json::to_string(&inputs).unwrap_or_default());
        let command = Command::AnswerQuestion { attachments: vec![], interaction_id: interaction, option_id: option, inputs };
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

    pub(super) fn feedback(&mut self, key: Key, presentation: InteractionId, targets: Vec<alondra_core::domain_v2::DeliveryTarget>, text: String, revision: Revision, draft: Option<(String, String)>) {
        let agent = self.item(&key).map(|i| self.agent_name(i.agent_id.as_ref())).unwrap_or_default();
        let slot = format!("feedback:{presentation}:{text}");
        let command = Command::SubmitFeedback { attachments: vec![], presentation_id: presentation, targets, body: text };
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

    fn load_diff(&mut self, key: &Key) {
        let Some(pr) = self.item(key).and_then(|i| self.item_pr(i)).map(|p| p.id.clone()) else {
            self.fail(tr!("There is no PR here", "Aquí no hay PR"));
            return;
        };
        let key = key.clone();
        self.flash(tr!("Loading the PR diff…", "Cargando el diff del PR…"));
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(90)).get::<Value>(&format!("/v2/domain/pull-requests/{}/diff", urlencode(&pr.0))).map_err(|e| e.to_string());
            let _ = tx.send(Msg::Diff { key, result });
        });
    }

    /// The item as Markdown: what it says, what it presents and where its decisions stand.
    pub fn item_text(&self, item: &InboxItem) -> Vec<String> {
        let mut parts = vec![format!("# {}", item.title)];
        if let Some(t) = item.task_id.as_ref().and_then(|id| self.task(id)) {
            let (state, title) = (super::tasks::state_label(t.state), md::one_line(&t.title));
            parts.push(trf!("Task: **{title}** · {state}", "Tarea: **{title}** · {state}"));
        }
        if let Some(body) = item.body.as_ref().filter(|b| !b.trim().is_empty()) {
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
        if !i.attachments.is_empty() {
            parts.push(i.attachments.iter().map(|a| format!("- 📎 {}", a.name)).collect::<Vec<_>>().join("\n"));
        }
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
                    parts.extend(feedback.iter().map(|f| f.body.clone()));
                }
            }
        }
        parts
    }
}

fn open_with_mac(target: &str) {
    super::open_with_mac(target)
}
