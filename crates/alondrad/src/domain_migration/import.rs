use alondra_core::{View, domain_v2 as d, model as old, pr as old_pr, tasks as old_task};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize)]
pub(crate) struct Skipped {
    pub requests: usize,
    pub tasks: usize,
    pub prs: usize,
    pub questions: usize,
    pub reports: usize,
    pub messages: usize,
    pub deliveries: usize,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct ImportReport {
    pub source_seq: u64,
    pub requests: usize,
    pub tasks: usize,
    /// Tasks made for open PRs beyond an active Task's registered one, or open
    /// PRs no active Task owned (those share one synthetic Request).
    pub derived_pr_tasks: usize,
    pub agents: usize,
    pub prs: usize,
    /// Open PRs whose base is another open PR's head, as (upper, lower) legacy ids.
    /// Stacks are reported, not converted: D02 requires explicit readiness.
    pub stacked_prs: Vec<(u64, u64)>,
    /// Tasks legacy let run that D02 holds as WaitingDependency (a dependency
    /// in review or closing, or no stack readiness): they need a manual decision.
    pub d02_waiting_dependency: Vec<String>,
    /// Open questions whose author is the assignee of their active Task.
    pub questions_reopened: usize,
    /// Other open questions of an active Task: unread, nobody can answer them.
    pub questions_unread: usize,
    pub reports_unread: usize,
    pub documents: usize,
    pub final_presentations: usize,
    /// Legacy deliveries of unknown outcome to an active Task's assignee: shown
    /// to the user unread, never resent or retried.
    pub deliveries_uncertain: usize,
    pub attachments: usize,
    pub attachment_size_discrepancies: usize,
    pub schedule_links_dropped: usize,
    /// Sent runs whose Task finished: v2 keeps a sent run only with its Task.
    pub schedule_runs_dropped: usize,
    pub skipped: Skipped,
    pub mapping_rows: usize,
    #[serde(skip_serializing)]
    pub links: Vec<ImportLink>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ImportLink {
    pub source_kind: String,
    pub source_id: String,
    pub role: String,
    pub target_kind: String,
    pub target_id: String,
    pub source_digest: String,
}
fn link(
    record: &impl Serialize,
    source_kind: &str,
    source_id: impl ToString,
    role: &str,
    target_kind: &str,
    target_id: impl ToString,
) -> Result<ImportLink> {
    Ok(ImportLink {
        source_kind: source_kind.into(),
        source_id: source_id.to_string(),
        role: role.into(),
        target_kind: target_kind.into(),
        target_id: target_id.to_string(),
        source_digest: super::hex(&Sha256::digest(serde_json::to_vec(record)?)),
    })
}

/// Legacy work still in flight: neither done, cancelled nor archived.
pub(crate) fn in_flight(task: &old_task::Task) -> bool {
    !matches!(task.state, old_task::TaskState::Done | old_task::TaskState::Cancelled)
        && task.archived_at.is_none()
}

/// A Request neither closed nor archived.
pub(crate) fn open_request(request: &old_task::Request) -> bool {
    request.closed.is_none() && request.archived_at.is_none()
}

/// Every legacy PR that no active Task owned shares this one synthetic Request.
pub(crate) const UNOWNED_PR_REQUEST: &str = "legacy-pr-request-unowned";

/// The only liveness v2 can trust at import: the Agent holds this active Task.
pub(crate) fn assigned_and_active(task: &d::Task, agent: &d::AgentId) -> bool {
    task.agent_id.as_ref() == Some(agent) && !task.state.terminal()
}

/// Where legacy content is read from. `recorded_home` is the HOME the events
/// were written in: absolute attachment paths point there, and a copy maps
/// them onto its own `files/`.
pub(crate) struct Source<'a> {
    pub home: &'a Path,
    pub recorded_home: &'a Path,
}

/// Materialize only the work in flight into a new epoch: open Requests and
/// active Tasks with their Agents, PRs, documents, unanswered questions, unread
/// reports and unsettled deliveries. Closed work and old conversation stay in
/// the archived legacy database. Imported records have System authorship and a
/// visible provenance prefix; neither old replies nor old auto-merge flags
/// become v2 authority. `target_home` is a new, private stage.
pub(crate) fn materialize_legacy(
    view: &View,
    source: &Source<'_>,
    target_home: &Path,
    epoch: d::DataEpoch,
) -> Result<(d::DomainState, ImportReport)> {
    let mut state = d::DomainState::new(epoch);
    state.revision = view.seq.max(1);
    let mut report = ImportReport { source_seq: view.seq, ..ImportReport::default() };
    let active: BTreeMap<&String, &old_task::Task> =
        view.board.tasks.iter().filter(|(_, task)| in_flight(task)).collect();
    report.skipped.tasks = view.board.tasks.len() - active.len();
    let mut request_ids: BTreeSet<&String> = view
        .board
        .requests
        .iter()
        .filter(|(_, request)| open_request(request))
        .map(|(id, _)| id)
        .collect();
    request_ids.extend(active.values().map(|task| &task.request));
    let mut ordered: Vec<(i64, d::Interaction)> = vec![];
    for id in &request_ids {
        let request_id = d::RequestId((*id).clone());
        let Some(request) = view.board.requests.get(*id) else {
            let task = active.values().find(|task| &task.request == *id).context("request owner missing")?;
            state.requests.insert(request_id.clone(), d::Request::new(request_id,
                format!("Legacy request for Task {} (original request missing)", task.id)));
            report.links.push(link(task, "task", &task.id, "synthetic_request", "request", id)?);
            report.requests += 1;
            continue;
        };
        let body = request
            .text
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .or(request.title.as_deref())
            .unwrap_or("Legacy request without text");
        state.requests.insert(request_id.clone(), d::Request {
            closed: request.closed.is_some(),
            title: request.title.clone().filter(|title| !title.trim().is_empty()),
            answer: request.closed.as_ref().and_then(|closure| closure.answer.clone()),
            withdrawn: request.closed.as_ref().is_some_and(|closure| closure.withdrawn),
            ..d::Request::new(request_id.clone(), body.to_owned())
        });
        report.links.push(link(request, "request", &request.id, "entity", "request", &request.id)?);
        report.requests += 1;
        if !request.attachments.is_empty() {
            let attachments = attachments(&request.attachments, source, target_home, &mut report)?;
            map_attachment_links(&mut report, "request", &request.id, &request.attachments, &attachments)?;
            ordered.push((request.created_at, info(
                format!("legacy-request-{}", request.id),
                d::Scope::Orchestrator { request_id: Some(request_id) },
                d::Recipient::User,
                format!("[Legacy Request {} attachments] {}", request.id, request.text.as_deref().unwrap_or_default()),
                attachments,
                state.revision,
            )));
        }
    }
    report.skipped.requests = view.board.requests.keys().filter(|id| !request_ids.contains(id)).count();
    for task in active.values() {
        let agent_id = task.assignee.as_ref().map(|a| d::AgentId(a.session_id.clone()));
        if let Some(agent_id) = &agent_id {
            if !state.agents.contains_key(agent_id) {
                let node = view.nodes.get(&agent_id.0);
                state.agents.insert(agent_id.clone(), d::Agent {
                    id: agent_id.clone(),
                    name: node
                        .and_then(|node| node.name.as_deref())
                        .filter(|name| !name.trim().is_empty())
                        .map_or_else(|| format!("Legacy session {}", agent_id.0), str::to_owned),
                    connected: false,
                });
                report.links.push(link(&agent_id.0, "session", &agent_id.0, "agent_conversation", "agent", &agent_id.0)?);
            }
        }
        let id = d::TaskId(task.id.clone());
        state.tasks.insert(id.clone(), d::Task {
            id,
            request_id: d::RequestId(task.request.clone()),
            agent_id,
            title: nonempty(&task.title, "Legacy Task"),
            state: task_state(task.state),
            revision: state.revision,
            started: task.state != old_task::TaskState::NotStarted,
            review_prs: false,
            final_presentation: None,
            pr_id: None,
            dependencies: task
                .depends_on
                .iter()
                .filter(|v| active.contains_key(v))
                .map(|v| d::TaskId(v.clone()))
                .collect(),
            stack: None,
            documents: BTreeMap::new(),
            archived: false,
        });
        report.links.push(link(task, "task", &task.id, "entity", "task", &task.id)?);
        report.tasks += 1;
    }
    if let Some(session) = view.orchestrator.as_ref().and_then(|runtime| runtime.session_id.as_deref()) {
        report.links.push(link(&session, "session", session, "orchestrator_conversation",
            "orchestrator", "generation-0")?);
    }
    report.agents = state.agents.len();
    // An active Task keeps its registered PR: the open one, or else its latest.
    // Other open PRs become derived Tasks; closed PRs of finished work stay behind.
    let mut prs: Vec<_> = view.prs.values().collect();
    prs.sort_by_key(|p| (p.state != old_pr::PrState::Open, if p.state == old_pr::PrState::Open { p.id } else { u64::MAX - p.id }));
    for pr in prs {
        let owner = pr
            .task
            .as_ref()
            .filter(|id| state.tasks.contains_key(&d::TaskId((*id).clone())));
        let open = pr.state == old_pr::PrState::Open;
        let valid = pr.head_sha.as_ref().is_some_and(|v| !v.trim().is_empty())
            && pr.base.as_ref().is_some_and(|v| !v.trim().is_empty())
            && !pr.merge_methods.is_empty();
        let registers = owner.is_some_and(|id| state.tasks[&d::TaskId(id.clone())].pr_id.is_none());
        if !valid || !(open || registers) {
            report.skipped.prs += 1;
            continue;
        }
        let task_id = match owner {
            Some(owner) if registers => d::TaskId(owner.clone()),
            _ => {
                let task_id = d::TaskId(format!("legacy-pr-task-{}", pr.id));
                ensure!(!state.tasks.contains_key(&task_id), "legacy PR Task ID collides with an existing Task");
                let original = owner.and_then(|id| state.tasks.get(&d::TaskId(id.clone())));
                let request_id = original
                    .map(|task| task.request_id.clone())
                    .unwrap_or_else(|| d::RequestId(UNOWNED_PR_REQUEST.into()));
                if !state.requests.contains_key(&request_id) {
                    state.requests.insert(request_id.clone(),
                        d::Request::new(request_id.clone(), "Imported pull requests without a task".into()));
                    report.requests += 1;
                }
                state.tasks.insert(task_id.clone(), d::Task {
                    id: task_id.clone(),
                    request_id,
                    agent_id: original.and_then(|t| t.agent_id.clone()),
                    title: format!("{} · PR #{}", nonempty(&pr.title, "Legacy PR"), pr.number),
                    state: d::TaskState::InReview,
                    revision: state.revision,
                    started: true,
                    review_prs: true,
                    final_presentation: None,
                    pr_id: None,
                    dependencies: BTreeSet::new(),
                    stack: None,
                    documents: BTreeMap::new(),
                    archived: false,
                });
                report.links.push(link(pr, "pr", pr.id, "derived_task", "task", &task_id.0)?);
                report.derived_pr_tasks += 1;
                task_id
            }
        };
        let id = d::PrId(format!("legacy-pr-{}", pr.id));
        let methods: Vec<_> = pr
            .merge_methods
            .iter()
            .map(|method| match method {
                old_pr::MergeMethod::Merge => d::MergeMethod::Merge,
                old_pr::MergeMethod::Squash => d::MergeMethod::Squash,
                old_pr::MergeMethod::Rebase => d::MergeMethod::Rebase,
            })
            .collect();
        let state_kind = match pr.state {
            old_pr::PrState::Open => d::PrState::Open,
            old_pr::PrState::Merged => d::PrState::Merged,
            _ => d::PrState::Closed,
        };
        let head = pr.head_sha.clone().unwrap();
        state.pull_requests.insert(id.clone(), d::PullRequest {
            id: id.clone(),
            task_id: task_id.clone(),
            host: "github.com".into(),
            repository: pr.repo.to_ascii_lowercase(),
            number: pr.number,
            head: head.clone(),
            base: pr.base.clone().unwrap(),
            state: state_kind,
            merged_head: (state_kind == d::PrState::Merged).then_some(head),
            allowed_methods: methods,
            review: Some(d::PullRequestReview { decisions: vec![] }),
        });
        let task = state.tasks.get_mut(&task_id).context("imported PR Task missing")?;
        task.pr_id = Some(id.clone());
        task.review_prs = true;
        report.links.push(link(pr, "pr", pr.id, "entity", "pr", &id.0)?);
        report.prs += 1;
    }
    for pr in view.prs.values() {
        let Some(below) = view.stack_below(pr) else { continue };
        let formal = |id: u64| state.pull_requests.contains_key(&d::PrId(format!("legacy-pr-{id}")));
        if !formal(pr.id) {
            continue;
        }
        report.stacked_prs.push((pr.id, below.id));
        if formal(below.id) {
            report.links.push(link(pr, "pr", pr.id, "stacked_on", "pr", format!("legacy-pr-{}", below.id))?);
        }
    }
    for q in view.questions.values() {
        let task_id = q.task.as_ref().map(|id| d::TaskId(id.clone()));
        let Some(task) = task_id.as_ref().and_then(|id| state.tasks.get(id)).filter(|_| q.status == old::QuestionStatus::Open) else {
            report.skipped.questions += 1;
            continue;
        };
        let task_id = task.id.clone();
        let author_session = d::AgentId(q.session_id.clone());
        // Legacy liveness is unreliable once the daemon stops, so a question
        // stays answerable only while its author is the current assignee of
        // its active Task: the answer reaches that Agent. Any other open
        // question has nobody to receive a v2 answer and arrives unread.
        let live = assigned_and_active(task, &author_session);
        let attachments = attachments(&q.attachments, source, target_home, &mut report)?;
        map_attachment_links(&mut report, "question", q.id, &q.attachments, &attachments)?;
        let mut body = format!("[Legacy question #{}; session={}] {}", q.id, q.session_id, q.title);
        if let Some(detail) = &q.detail {
            body.push('\n');
            body.push_str(detail);
        }
        if !q.options.is_empty() {
            body.push_str("\nLegacy options:");
            for option in &q.options {
                body.push_str("\n- ");
                body.push_str(&option.label);
                if let Some(description) = &option.description {
                    body.push_str(": ");
                    body.push_str(description);
                }
            }
        }
        let (author, blocking, payload) = if live {
            report.questions_reopened += 1;
            let options = q
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| d::QuestionOption {
                    id: format!("legacy-option-{}", index + 1),
                    label: nonempty(&option.label, "Option"),
                    inputs: vec![],
                })
                .collect();
            (d::Author::Agent { agent_id: author_session }, q.blocking,
                d::InteractionPayload::Question { options, answer: None })
        } else {
            body.push_str("\nRequires reconciliation: its author is not the assignee of this Task, so a v2 answer cannot reach it.");
            report.questions_unread += 1;
            (d::Author::System, false, d::InteractionPayload::Information { read_by: BTreeSet::new() })
        };
        let id = format!("legacy-question-{}", q.id);
        ordered.push((q.asked_at, d::Interaction {
            id: d::InteractionId(id.clone()),
            scope: d::Scope::Task { task_id },
            author,
            recipient: if q.to_orchestrator { d::Recipient::Orchestrator } else { d::Recipient::User },
            body,
            blocking,
            subject: None,
            payload,
            attachments,
            revision: state.revision,
            order: 0,
            withdrawn: false,
        }));
        report.links.push(link(q, "question", q.id, "entity", "interaction", id)?);
    }
    for r in view.reports.values() {
        let task_id = r.task.as_ref().map(|id| d::TaskId(id.clone())).filter(|id| state.tasks.contains_key(id));
        let final_report = task_id.as_ref().is_some_and(|id| view.board.tasks.get(&id.0).is_some_and(|task| task.final_report == Some(r.id)));
        let document = r.kind == old::ReportKind::Document && task_id.is_some();
        let unread = !r.read && task_id.is_some();
        if !(document || unread || final_report) {
            report.skipped.reports += 1;
            continue;
        }
        if let (true, Some(task_id)) = (document, &task_id) {
            let bytes = if let Some(first) = r.attachments.first() {
                read_stored_bytes(source, first)?.1
            } else {
                r.body.as_deref().unwrap_or(&r.title).as_bytes().to_vec()
            };
            let (digest, reference) = store_blob(target_home, &bytes)?;
            let document_id = d::DocumentId(format!("legacy-document-{}", r.id));
            let revision_id = d::DocumentRevisionId(format!("legacy-document-revision-{}", r.id));
            state.documents.insert(document_id.clone(), d::Document {
                id: document_id.clone(),
                title: nonempty(&r.title, "Legacy document"),
                current_revision: revision_id.clone(),
            });
            state.document_revisions.insert(revision_id.clone(), d::DocumentRevision {
                id: revision_id.clone(),
                document_id: document_id.clone(),
                number: 1,
                content_digest: digest,
                content_reference: reference,
            });
            report.links.push(link(r, "report", r.id, "document", "document", &document_id.0)?);
            report.links.push(link(r, "report", r.id, "document_revision", "document_revision", &revision_id.0)?);
            state.tasks.get_mut(task_id).context("document Task missing")?
                .documents.insert(document_id.clone(), "document".into());
            state.document_reviews.push(d::DocumentReview {
                task_id: task_id.clone(),
                document_id,
                decisions: vec![],
            });
            report.documents += 1;
        }
        if unread {
            let attachments = attachments(&r.attachments, source, target_home, &mut report)?;
            map_attachment_links(&mut report, "report", r.id, &r.attachments, &attachments)?;
            let id = format!("legacy-report-{}", r.id);
            let mut card = info(
                id.clone(),
                d::Scope::Task { task_id: task_id.clone().context("unread report Task missing")? },
                if r.to_orchestrator { d::Recipient::Orchestrator } else { d::Recipient::User },
                format!("[Legacy report #{}; session={}] {}\n{}", r.id, r.session_id, r.title, r.body.as_deref().unwrap_or("")),
                attachments,
                state.revision,
            );
            card.payload = d::InteractionPayload::Information { read_by: BTreeSet::new() };
            ordered.push((r.created_at, card));
            report.links.push(link(r, "report", r.id, "entity", "interaction", id)?);
            report.reports_unread += 1;
        }
    }
    report.skipped.messages = view.messages.len();
    for attempt in view.delivery_attempts.values().filter(|attempt| super::unresolved_delivery(attempt)) {
        let agent_id = d::AgentId(attempt.session_id.clone());
        let task = state.tasks.values().find(|task| assigned_and_active(task, &agent_id)).map(|task| task.id.clone());
        let (Some(task), false) = (task, attempt.state == alondra_core::delivery::State::Retryable) else {
            report.skipped.deliveries += 1;
            continue;
        };
        report.deliveries_uncertain += 1;
        let mut body = format!(
            "[Legacy delivery #{}; state={:?}; channel={:?}; reason={}; session={}] This delivery may already have reached the session; it is never resent or retried.",
            attempt.id,
            attempt.state,
            attempt.channel,
            attempt.reason.as_deref().unwrap_or("none"),
            attempt.session_id
        );
        for message in attempt.message_ids.iter().filter_map(|id| view.messages.get(id)) {
            body.push_str(&format!("\nLegacy message #{}: {}", message.id, message.text));
        }
        if let Some(question) = attempt.question_id {
            body.push_str(&format!("\nTerminal decision for legacy question #{question}."));
        }
        let id = format!("legacy-delivery-{}", attempt.id);
        let mut card = info(id.clone(), d::Scope::Task { task_id: task }, d::Recipient::User, body, vec![], state.revision);
        card.payload = d::InteractionPayload::Information { read_by: BTreeSet::new() };
        ordered.push((attempt.updated_at, card));
        report.links.push(link(attempt, "delivery_attempt", attempt.id, "conserved", "interaction", id)?);
    }
    // Work in flight keeps its review context: an open PR and/or the legacy
    // final report become the Task's current final presentation, so the user
    // can decide on it and the live Agent can complete the Task in v2.
    let presented: Vec<_> = state.tasks.keys().cloned().collect();
    for task_id in presented {
        let mut targets = vec![];
        let mut at = 0;
        let task = &state.tasks[&task_id];
        if let Some(pr) = task.pr_id.as_ref().map(|id| &state.pull_requests[id]) {
            if pr.state == d::PrState::Open {
                targets.push(d::DeliveryTarget::PullRequest { pr_id: pr.id.clone(), head: pr.head.clone() });
            }
        }
        let final_report = view
            .board
            .tasks
            .get(&task_id.0)
            .and_then(|task| task.final_report)
            .and_then(|id| view.reports.get(&id))
            .filter(|r| r.task.as_deref() == Some(task_id.0.as_str()));
        if let Some(r) = final_report {
            at = r.created_at;
            let document_id = d::DocumentId(format!("legacy-document-{}", r.id));
            if task.documents.contains_key(&document_id) {
                targets.push(d::DeliveryTarget::DocumentRevision {
                    document_id,
                    revision_id: d::DocumentRevisionId(format!("legacy-document-revision-{}", r.id)),
                });
            } else {
                targets.push(d::DeliveryTarget::Text {
                    text: nonempty(r.body.as_deref().unwrap_or_default(), &nonempty(&r.title, "Legacy final report")),
                });
            }
        }
        if targets.is_empty() {
            continue;
        }
        let interaction_id = format!("legacy-final-{}", task_id.0);
        ordered.push((at, d::Interaction {
            id: d::InteractionId(interaction_id.clone()),
            scope: d::Scope::Task { task_id: task_id.clone() },
            author: match &task.agent_id {
                Some(agent_id) => d::Author::Agent { agent_id: agent_id.clone() },
                None => d::Author::System,
            },
            recipient: d::Recipient::User,
            body: format!("[Legacy final presentation for Task {}] {}", task_id.0, task.title),
            blocking: false,
            subject: None,
            payload: d::InteractionPayload::Delivery { targets, is_final: true, acceptance: None, feedback: vec![] },
            attachments: vec![],
            revision: state.revision,
            order: 0,
            withdrawn: false,
        }));
        state.tasks.get_mut(&task_id).unwrap().final_presentation = Some(d::InteractionId(interaction_id.clone()));
        report.links.push(link(&task_id.0, "task", &task_id.0, "final_presentation", "interaction", &interaction_id)?);
        report.final_presentations += 1;
    }
    ordered.sort_by(|a, b| (a.0, &a.1.id).cmp(&(b.0, &b.1.id)));
    for (index, (_, mut interaction)) in ordered.into_iter().enumerate() {
        interaction.order = index as u64 + 1;
        state.interactions.insert(interaction.id.clone(), interaction);
    }
    state.revision = state.revision.max(state.interactions.len() as u64);
    let legacy_states: BTreeMap<_, _> = state.tasks.iter().map(|(id, task)| (id.clone(), task.state)).collect();
    state.derive_states();
    report.d02_waiting_dependency = state
        .tasks
        .values()
        .filter(|task| task.state == d::TaskState::WaitingDependency
            && legacy_states[&task.id] != d::TaskState::WaitingDependency)
        .map(|task| task.id.0.clone())
        .collect();
    state.validate().map_err(anyhow::Error::new)?;
    validate_links(&state, &report.links)?;
    let coverage = |kind: &str, role: &str| report.links.iter().filter(|v| v.source_kind == kind && v.role == role).count();
    ensure!(
        coverage("task", "entity") == report.tasks
            && coverage("pr", "entity") == report.prs
            && coverage("question", "entity") == report.questions_reopened + report.questions_unread
            && coverage("report", "entity") == report.reports_unread
            && coverage("attachment", "content") == report.attachments
            && coverage("delivery_attempt", "conserved") == report.deliveries_uncertain
            && report.tasks + report.skipped.tasks == view.board.tasks.len()
            && report.prs + report.skipped.prs == view.prs.len()
            && report.questions_reopened + report.questions_unread + report.skipped.questions == view.questions.len(),
        "legacy provenance mapping lacks source coverage"
    );
    report.mapping_rows = report.links.len();
    Ok((state, report))
}

fn validate_links(state: &d::DomainState, links: &[ImportLink]) -> Result<()> {
    let mut sources = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for item in links {
        ensure!(
            sources.insert((&item.source_kind, &item.source_id, &item.role)),
            "duplicate legacy source mapping"
        );
        ensure!(
            targets.insert((&item.target_kind, &item.target_id, &item.role)),
            "duplicate legacy target mapping"
        );
        let exists = match item.target_kind.as_str() {
            "request" => state
                .requests
                .contains_key(&d::RequestId(item.target_id.clone())),
            "task" => state.tasks.contains_key(&d::TaskId(item.target_id.clone())),
            "pr" => state
                .pull_requests
                .contains_key(&d::PrId(item.target_id.clone())),
            "agent" => state
                .agents
                .contains_key(&d::AgentId(item.target_id.clone())),
            "orchestrator" => item.target_id == "generation-0"
                && state.orchestrator_generation == 0,
            "interaction" => state
                .interactions
                .contains_key(&d::InteractionId(item.target_id.clone())),
            "document" => state
                .documents
                .contains_key(&d::DocumentId(item.target_id.clone())),
            "document_revision" => state
                .document_revisions
                .contains_key(&d::DocumentRevisionId(item.target_id.clone())),
            "attachment" => {
                let Some((interaction, id)) = item.target_id.split_once('/') else {
                    anyhow::bail!("invalid attachment mapping")
                };
                state
                    .interactions
                    .get(&d::InteractionId(interaction.into()))
                    .is_some_and(|v| v.attachments.iter().any(|a| a.id == id))
            }
            _ => false,
        };
        ensure!(
            exists,
            "legacy mapping points to missing target {}",
            item.target_kind
        );
        ensure!(
            item.source_digest.len() == 64,
            "legacy mapping lacks source digest"
        );
    }
    Ok(())
}

fn map_attachment_links(
    report: &mut ImportReport,
    parent_kind: &str,
    parent_id: impl ToString,
    originals: &[old::Attachment],
    imported: &[d::AttachmentRef],
) -> Result<()> {
    let parent_id = parent_id.to_string();
    ensure!(
        originals.len() == imported.len(),
        "attachment mapping count changed"
    );
    for (ordinal, (source, target)) in originals.iter().zip(imported).enumerate() {
        let interaction = format!("legacy-{}-{}", parent_kind, parent_id);
        report.links.push(link(
            source,
            "attachment",
            format!("{parent_kind}/{parent_id}/{ordinal}"),
            "content",
            "attachment",
            format!("{interaction}/{}", target.id),
        )?);
    }
    Ok(())
}

fn task_state(value: old_task::TaskState) -> d::TaskState {
    match value {
        old_task::TaskState::NotStarted => d::TaskState::NotStarted,
        old_task::TaskState::Running => d::TaskState::Running,
        old_task::TaskState::NeedsYou => d::TaskState::NeedsYou,
        old_task::TaskState::InReview => d::TaskState::InReview,
        old_task::TaskState::WaitingDependency => d::TaskState::WaitingDependency,
        old_task::TaskState::Closing => d::TaskState::Closing,
        old_task::TaskState::Done => d::TaskState::Done,
        old_task::TaskState::Cancelled => d::TaskState::Cancelled,
    }
}
fn nonempty(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.into()
    } else {
        value.into()
    }
}
fn info(
    id: String,
    scope: d::Scope,
    recipient: d::Recipient,
    body: String,
    attachments: Vec<d::AttachmentRef>,
    revision: u64,
) -> d::Interaction {
    d::Interaction {
        id: d::InteractionId(id),
        scope,
        author: d::Author::System,
        recipient,
        body,
        blocking: false,
        subject: None,
        attachments,
        payload: d::InteractionPayload::Information {
            read_by: BTreeSet::from(["legacy-import".into()]),
        },
        revision,
        order: 0,
        withdrawn: false,
    }
}
pub(super) fn stored_file(source: &Source<'_>, attachment: &old::Attachment) -> Result<PathBuf> {
    let stored = attachment
        .stored_path
        .as_ref()
        .context("legacy attachment has no stored copy")?;
    let path = Path::new(stored);
    let home = source.home;
    let canonical_home = home.canonicalize()?;
    let path = if path.is_absolute() {
        if path.starts_with(home) || path.starts_with(&canonical_home) {
            path.to_owned()
        } else {
            // Reject a path from any other tree even if it shares a `files/` suffix.
            let suffix = path
                .strip_prefix(source.recorded_home.join("files"))
                .context("legacy attachment was outside the recorded HOME files")?;
            home.join("files").join(suffix)
        }
    } else {
        home.join(path)
    };
    let canonical = path.canonicalize()?;
    ensure!(
        canonical.starts_with(canonical_home.join("files")),
        "legacy attachment escapes HOME files"
    );
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "legacy attachment is not a regular stored file"
    );
    Ok(path)
}
pub(super) fn read_stored_bytes(
    source: &Source<'_>,
    attachment: &old::Attachment,
) -> Result<(PathBuf, Vec<u8>)> {
    let file = stored_file(source, attachment)?;
    let bytes = fs::read(&file)?;
    Ok((file, bytes))
}
fn attachments(
    input: &[old::Attachment],
    source: &Source<'_>,
    target: &Path,
    report: &mut ImportReport,
) -> Result<Vec<d::AttachmentRef>> {
    let mut output = Vec::new();
    for (ordinal, attachment) in input.iter().enumerate() {
        let (file, bytes) = read_stored_bytes(source, attachment)?;
        ensure!(
            bytes.len() <= 64 * 1024 * 1024,
            "legacy attachment exceeds v2 limit"
        );
        if attachment
            .bytes
            .is_some_and(|size| size != bytes.len() as u64)
        {
            report.attachment_size_discrepancies += 1;
        }
        let (digest, reference) = store_blob(target, &bytes)?;
        output.push(d::AttachmentRef {
            id: format!("legacy-attachment-{}-{}", hex_path(&file), ordinal),
            name: nonempty(&attachment.name, "Legacy attachment"),
            content_digest: digest,
            content_reference: reference,
            bytes: bytes.len() as u64,
        });
        report.attachments += 1;
    }
    Ok(output)
}
fn hex_path(path: &Path) -> String {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    super::hex(&digest)[..16].to_string()
}
fn store_blob(home: &Path, bytes: &[u8]) -> Result<(String, String)> {
    let digest = super::hex(&Sha256::digest(bytes));
    let files = home.join("files");
    if !files.exists() {
        fs::DirBuilder::new().mode(0o700).create(&files)?;
    }
    let folder = files.join("domain-v2");
    if !folder.exists() {
        fs::DirBuilder::new().mode(0o700).create(&folder)?;
    }
    for path in [&files, &folder] {
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0,
            "unsafe import content directory"
        );
    }
    let target = folder.join(&digest);
    match fs::symlink_metadata(&target) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file()
                    && metadata.nlink() == 1
                    && metadata.uid() == unsafe { libc::geteuid() },
                "unsafe existing import blob"
            );
            let mut existing = Vec::new();
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&target)?
                .read_to_end(&mut existing)?;
            ensure!(existing == bytes, "import blob collision");
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&target)?;
            output.write_all(bytes)?;
            output.sync_all()?;
            File::open(&folder)?.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok((digest.clone(), format!("files/domain-v2/{digest}")))
}
