use alondra_core::{View, domain_v2 as d, model as old, pr as old_pr, tasks as old_task};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub(crate) struct ImportReport {
    pub source_seq: u64,
    pub requests: usize,
    pub request_closures: usize,
    pub request_attachments: usize,
    pub tasks: usize,
    pub agents: usize,
    pub split_pr_tasks: usize,
    pub formal_prs: usize,
    pub reference_only_prs: Vec<u64>,
    pub questions: usize,
    pub open_questions_requiring_reconciliation: usize,
    pub open_questions_reopened: usize,
    pub final_presentations: usize,
    /// Open PRs whose base is another open PR's head, as (upper, lower) legacy ids.
    /// Stacks are reported, not converted: D02 requires explicit readiness.
    pub stacked_prs: Vec<(u64, u64)>,
    /// Tasks legacy let run that D02 holds as WaitingDependency (a dependency
    /// in review or closing, or no stack readiness): they need a manual decision.
    pub d02_waiting_dependency: Vec<String>,
    pub reports: usize,
    pub messages: usize,
    pub documents: usize,
    pub attachments: usize,
    pub attachment_size_discrepancies: usize,
    /// Legacy deliveries still unresolved at cutover, conserved as Interactions
    /// and never resent: retryable ones wait unread for their imported Agent,
    /// uncertain or dispatching ones are shown to the user, the rest are history.
    pub conserved_undelivered: usize,
    pub conserved_uncertain: usize,
    pub conserved_historical: usize,
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

/// Materialize the current legacy state, including historical cards, into a
/// new epoch. Imported records have System authorship and a visible provenance
/// prefix; neither old replies nor old auto-merge flags become v2 authority.
/// `target_home` is a new, private rehearsal HOME; all referenced content is
/// copied and verified there before the caller may commit the resulting state.
pub(crate) fn materialize_legacy(
    view: &View,
    source_home: &Path,
    target_home: &Path,
    epoch: d::DataEpoch,
) -> Result<(d::DomainState, ImportReport)> {
    let mut state = d::DomainState::new(epoch);
    state.revision = view.seq.max(1);
    let mut report = ImportReport {
        source_seq: view.seq,
        requests: 0,
        request_closures: 0,
        request_attachments: 0,
        tasks: 0,
        agents: 0,
        split_pr_tasks: 0,
        formal_prs: 0,
        reference_only_prs: vec![],
        questions: 0,
        open_questions_requiring_reconciliation: 0,
        open_questions_reopened: 0,
        final_presentations: 0,
        stacked_prs: vec![],
        d02_waiting_dependency: vec![],
        reports: 0,
        messages: 0,
        documents: 0,
        attachments: 0,
        attachment_size_discrepancies: 0,
        conserved_undelivered: 0,
        conserved_uncertain: 0,
        conserved_historical: 0,
        mapping_rows: 0,
        links: vec![],
    };
    for request in view.board.requests.values() {
        let body = request
            .text
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .or(request.title.as_deref())
            .unwrap_or("Legacy request without text");
        let id = d::RequestId(request.id.clone());
        state.requests.insert(
            id.clone(),
            d::Request {
                id,
                body: body.to_owned(),
                closed: request.closed.is_some(),
            },
        );
        report.links.push(link(
            request,
            "request",
            &request.id,
            "entity",
            "request",
            &request.id,
        )?);
        report.requests += 1;
    }
    // A node may have no Task. Preserve its identity and conversation as
    // historical data without importing transient process or connection state.
    let orchestrator_session = view
        .orchestrator
        .as_ref()
        .and_then(|runtime| runtime.session_id.as_deref());
    for (session, node) in &view.nodes {
        if node.is_orchestrator || Some(session.as_str()) == orchestrator_session {
            continue;
        }
        // R01: the Agent is the Claude session, so a live session keeps acting on
        // its imported Tasks under the same identity.
        let agent_id = d::AgentId(session.clone());
        state.agents.insert(
            agent_id.clone(),
            d::Agent {
                id: agent_id.clone(),
                name: node
                    .name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(session)
                    .to_owned(),
                connected: false,
            },
        );
        report.links.push(link(node, "node", session, "entity", "agent", &agent_id.0)?);
    }
    // A few very old cards precede durable Request records. Give them a
    // clearly synthetic request rather than silently dropping their Task.
    for task in view.board.tasks.values() {
        let request_id = d::RequestId(task.request.clone());
        if !state.requests.contains_key(&request_id) {
            report.links.push(link(
                task,
                "task",
                &task.id,
                "synthetic_request",
                "request",
                &task.request,
            )?);
        }
        state
            .requests
            .entry(request_id.clone())
            .or_insert_with(|| d::Request {
                id: request_id.clone(),
                body: format!(
                    "Legacy request for Task {} (original request missing)",
                    task.id
                ),
                closed: false,
            });
        let agent_id = task
            .assignee
            .as_ref()
            .map(|a| d::AgentId(a.session_id.clone()));
        if let Some(agent_id) = &agent_id {
            state
                .agents
                .entry(agent_id.clone())
                .or_insert_with(|| d::Agent {
                    id: agent_id.clone(),
                    name: format!(
                        "Legacy session {}",
                        task.assignee.as_ref().unwrap().session_id
                    ),
                    connected: false,
                });
        }
        let id = d::TaskId(task.id.clone());
        state.tasks.insert(
            id.clone(),
            d::Task {
                id,
                request_id,
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
                    .filter(|v| view.board.tasks.contains_key(*v))
                    .map(|v| d::TaskId(v.clone()))
                    .collect(),
                stack: None,
                documents: BTreeMap::new(),
                archived: task.archived_at.is_some()
                    && matches!(
                        task.state,
                        old_task::TaskState::Done | old_task::TaskState::Cancelled
                    ),
            },
        );
        report
            .links
            .push(link(task, "task", &task.id, "entity", "task", &task.id)?);
        report.tasks += 1;
    }
    // Preserve the Claude conversation identifiers as import provenance.
    let sessions: BTreeSet<_> = view
        .board
        .tasks
        .values()
        .filter_map(|task| task.assignee.as_ref().map(|a| a.session_id.as_str()))
        .chain(
            view.nodes
                .iter()
                .filter(|(session, node)| {
                    !node.is_orchestrator && Some(session.as_str()) != orchestrator_session
                })
                .map(|(session, _)| session.as_str()),
        )
        .collect();
    for session in sessions {
        report.links.push(link(&session, "session", session, "agent_conversation",
            "agent", session)?);
    }
    if let Some(session) = view.orchestrator.as_ref().and_then(|runtime| runtime.session_id.as_deref()) {
        report.links.push(link(&session, "session", session, "orchestrator_conversation",
            "orchestrator", "generation-0")?);
    }
    report.agents = state.agents.len();
    // A legacy Task could own many PRs. Keep one on the original Task and
    // create explicit derived Tasks for the others. Ineligible PRs still get
    // a historical Information card carrying URL/title/state and old ID.
    let mut prs: Vec<_> = view.prs.values().collect();
    prs.sort_by_key(|p| (p.state != old_pr::PrState::Open, p.id));
    for pr in prs {
        let owner = pr
            .task
            .as_ref()
            .filter(|id| state.tasks.contains_key(&d::TaskId((*id).clone())));
        let valid = pr.head_sha.as_ref().is_some_and(|v| !v.trim().is_empty())
            && pr.base.as_ref().is_some_and(|v| !v.trim().is_empty())
            && !pr.merge_methods.is_empty();
        if !valid {
            report.reference_only_prs.push(pr.id);
            continue;
        }
        let (task_id, split) = if let Some(owner) = owner {
            let id = d::TaskId(owner.clone());
            if state.tasks[&id].pr_id.is_none() {
                (id, false)
            } else {
                (d::TaskId(format!("legacy-pr-task-{}", pr.id)), true)
            }
        } else {
            (d::TaskId(format!("legacy-pr-task-{}", pr.id)), true)
        };
        if split {
            ensure!(
                !state.tasks.contains_key(&task_id),
                "legacy PR split Task ID collides with an existing Task"
            );
            let request_id = owner
                .and_then(|id| state.tasks.get(&d::TaskId(id.clone())))
                .map(|task| task.request_id.clone())
                .unwrap_or_else(|| d::RequestId(format!("legacy-pr-request-{}", pr.id)));
            state
                .requests
                .entry(request_id.clone())
                .or_insert_with(|| d::Request {
                    id: request_id.clone(),
                    body: format!("Legacy PR #{} without a recorded Request", pr.id),
                    closed: false,
                });
            let original = owner.and_then(|id| state.tasks.get(&d::TaskId(id.clone())));
            state.tasks.insert(
                task_id.clone(),
                d::Task {
                    id: task_id.clone(),
                    request_id,
                    agent_id: original.and_then(|t| t.agent_id.clone()),
                    title: format!(
                        "{} · PR #{} (legacy Task {})",
                        nonempty(&pr.title, "Legacy PR"),
                        pr.number,
                        owner.map_or("unassigned", String::as_str)
                    ),
                    state: match pr.state {
                        old_pr::PrState::Open => d::TaskState::InReview,
                        old_pr::PrState::Merged => d::TaskState::Done,
                        _ => d::TaskState::Cancelled,
                    },
                    revision: state.revision,
                    started: true,
                    review_prs: true,
                    final_presentation: None,
                    pr_id: None,
                    dependencies: BTreeSet::new(),
                    stack: None,
                    documents: BTreeMap::new(),
                    archived: false,
                },
            );
            report
                .links
                .push(link(pr, "pr", pr.id, "derived_task", "task", &task_id.0)?);
            report.split_pr_tasks += 1;
        }
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
        let repository = pr.repo.to_ascii_lowercase();
        let state_kind = match pr.state {
            old_pr::PrState::Open => d::PrState::Open,
            old_pr::PrState::Merged => d::PrState::Merged,
            _ => d::PrState::Closed,
        };
        let head = pr.head_sha.clone().unwrap();
        state.pull_requests.insert(
            id.clone(),
            d::PullRequest {
                id: id.clone(),
                task_id: task_id.clone(),
                host: "github.com".into(),
                repository,
                number: pr.number,
                head: head.clone(),
                base: pr.base.clone().unwrap(),
                state: state_kind,
                merged_head: (state_kind == d::PrState::Merged).then_some(head),
                allowed_methods: methods,
                review: Some(d::PullRequestReview { decisions: vec![] }),
            },
        );
        let task = state
            .tasks
            .get_mut(&task_id)
            .context("imported PR Task missing")?;
        task.pr_id = Some(id);
        task.review_prs = true;
        report.links.push(link(
            pr,
            "pr",
            pr.id,
            "entity",
            "pr",
            format!("legacy-pr-{}", pr.id),
        )?);
        report.formal_prs += 1;
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
    // Legacy metadata is retained on an Information card even when a PR was
    // imported formally. Old approvals are deliberately not transferred.
    let mut ordered: Vec<(i64, d::Interaction)> = vec![];
    for request in view.board.requests.values() {
        let attachments = attachments(&request.attachments, source_home, target_home, &mut report)?;
        report.request_attachments += attachments.len();
        map_attachment_links(
            &mut report,
            "request",
            &request.id,
            &request.attachments,
            &attachments,
        )?;
        let interaction_id = format!("legacy-request-{}", request.id);
        let mut request_body = format!(
            "[Legacy Request {}; created_at={}; synthetic={}; archived_at={:?}; title={:?}] {}",
            request.id,
            request.created_at,
            request.synthetic,
            request.archived_at,
            request.title,
            request.text.as_deref().unwrap_or_default()
        );
        if let Some(options) = &request.options {
            request_body.push_str("\nLegacy launch options: ");
            request_body.push_str(&serde_json::to_string(options)?);
        }
        ordered.push((
            request.created_at,
            info(
                interaction_id.clone(),
                d::Scope::Orchestrator {
                    request_id: Some(d::RequestId(request.id.clone())),
                },
                d::Recipient::User,
                request_body,
                attachments,
                state.revision,
            ),
        ));
        report.links.push(link(
            request,
            "request",
            &request.id,
            "record",
            "interaction",
            &interaction_id,
        )?);
        if let Some(closure) = &request.closed {
            let closure_id = format!("legacy-request-closure-{}", request.id);
            ordered.push((
                closure.at,
                info(
                    closure_id.clone(),
                    d::Scope::Orchestrator {
                        request_id: Some(d::RequestId(request.id.clone())),
                    },
                    d::Recipient::User,
                    format!(
                        "[Legacy Request closure {}; withdrawn={}; at={}] {}",
                        request.id,
                        closure.withdrawn,
                        closure.at,
                        closure.answer.as_deref().unwrap_or("No answer recorded")
                    ),
                    vec![],
                    state.revision,
                ),
            ));
            report.links.push(link(
                closure,
                "request",
                &request.id,
                "closure",
                "interaction",
                &closure_id,
            )?);
            report.request_closures += 1;
        }
    }
    for pr in view.prs.values() {
        report.links.push(link(
            pr,
            "pr",
            pr.id,
            "record",
            "interaction",
            format!("legacy-pr-record-{}", pr.id),
        )?);
        let scope = pr
            .task
            .as_ref()
            .filter(|v| state.tasks.contains_key(&d::TaskId((*v).clone())))
            .map(|v| d::Scope::Task {
                task_id: d::TaskId(v.clone()),
            })
            .unwrap_or(d::Scope::Orchestrator { request_id: None });
        ordered.push((
            pr.opened_at,
            info(
                format!("legacy-pr-record-{}", pr.id),
                scope,
                d::Recipient::User,
                format!(
                    "[Legacy PR #{}; state={:?}; session={}] {}\n{}\n{}",
                    pr.id,
                    pr.state,
                    pr.session_id,
                    pr.title,
                    pr.url,
                    pr.body.as_deref().unwrap_or("")
                ),
                vec![],
                state.revision,
            ),
        ));
    }
    for q in view.questions.values() {
        let scope = legacy_scope(q.task.as_deref(), &state);
        let recipient = if q.to_orchestrator {
            d::Recipient::Orchestrator
        } else {
            d::Recipient::User
        };
        let attachments = attachments(&q.attachments, source_home, target_home, &mut report)?;
        map_attachment_links(&mut report, "question", q.id, &q.attachments, &attachments)?;
        let mut body = format!(
            "[Legacy question #{}; status={:?}; session={}] {}",
            q.id, q.status, q.session_id, q.title
        );
        if let Some(detail) = &q.detail {
            body.push_str("\n");
            body.push_str(detail);
        }
        body.push_str("\nLegacy context: ");
        body.push_str(&serde_json::to_string(&serde_json::json!({
            "origin":q.origin,"to_orchestrator":q.to_orchestrator,
            "action":q.action,"steps":q.steps,"links":q.links,
            "mission":q.mission,"replaces":q.replaces,"about_pr":q.about_pr,
            "tool_use_id":q.tool_use_id,"node_context":q.node_context,
        }))?);
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
        if let Some(answer) = &q.answer {
            body.push_str("\nLegacy answer (including selected option and provenance): ");
            body.push_str(&serde_json::to_string(answer)?);
        }
        let author_session = d::AgentId(q.session_id.clone());
        let live = q.status == old::QuestionStatus::Open
            && matches!(&scope, d::Scope::Task { task_id }
                if state.tasks.get(task_id).is_some_and(|task| !task.state.terminal()))
            && view.nodes.get(&q.session_id).is_some_and(|node| node.alive)
            && state.agents.contains_key(&author_session);
        // A question whose session is still alive on an active Task stays
        // answerable: the answer reaches the Task's current Agent. Any other
        // open question has nobody to receive a v2 answer (v2 refuses answers
        // on a closed Task) and is preserved as a historical record.
        let (author, blocking, payload) = if live {
            report.open_questions_reopened += 1;
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
            (
                d::Author::Agent { agent_id: author_session },
                q.blocking,
                d::InteractionPayload::Question { options, answer: None },
            )
        } else {
            if q.status == old::QuestionStatus::Open {
                body.push_str("\nRequires reconciliation: no active Task Agent can receive a v2 answer to this historical question.");
                report.open_questions_requiring_reconciliation += 1;
            }
            (
                d::Author::System,
                false,
                d::InteractionPayload::Information {
                    read_by: if q.status == old::QuestionStatus::Open {
                        BTreeSet::new()
                    } else {
                        BTreeSet::from(["legacy-import".into()])
                    },
                },
            )
        };
        ordered.push((
            q.asked_at,
            d::Interaction {
                id: d::InteractionId(format!("legacy-question-{}", q.id)),
                scope,
                author,
                recipient,
                body,
                blocking,
                subject: None,
                payload,
                attachments,
                revision: state.revision,
                order: 0,
            },
        ));
        report.links.push(link(
            q,
            "question",
            q.id,
            "entity",
            "interaction",
            format!("legacy-question-{}", q.id),
        )?);
        report.questions += 1;
    }
    for r in view.reports.values() {
        let scope = legacy_scope(r.task.as_deref(), &state);
        let attachments = attachments(&r.attachments, source_home, target_home, &mut report)?;
        map_attachment_links(&mut report, "report", r.id, &r.attachments, &attachments)?;
        let document_id = if r.kind == old::ReportKind::Document {
            let bytes = if let Some(first) = r.attachments.first() {
                read_stored_bytes(source_home, first)?.1
            } else {
                r.body.as_deref().unwrap_or(&r.title).as_bytes().to_vec()
            };
            let (digest, reference) = store_blob(target_home, &bytes)?;
            let document_id = d::DocumentId(format!("legacy-document-{}", r.id));
            let revision_id = d::DocumentRevisionId(format!("legacy-document-revision-{}", r.id));
            state.documents.insert(
                document_id.clone(),
                d::Document {
                    id: document_id.clone(),
                    title: nonempty(&r.title, "Legacy document"),
                    current_revision: revision_id.clone(),
                },
            );
            state.document_revisions.insert(
                revision_id.clone(),
                d::DocumentRevision {
                    id: revision_id,
                    document_id: document_id.clone(),
                    number: 1,
                    content_digest: digest,
                    content_reference: reference,
                },
            );
            report.links.push(link(
                r,
                "report",
                r.id,
                "document",
                "document",
                format!("legacy-document-{}", r.id),
            )?);
            report.links.push(link(
                r,
                "report",
                r.id,
                "document_revision",
                "document_revision",
                format!("legacy-document-revision-{}", r.id),
            )?);
            if let d::Scope::Task { task_id } = &scope {
                state
                    .tasks
                    .get_mut(task_id)
                    .context("document Task missing")?
                    .documents
                    .insert(document_id.clone(), "document".into());
                state.document_reviews.push(d::DocumentReview {
                    task_id: task_id.clone(),
                    document_id: document_id.clone(),
                    decisions: vec![],
                });
            }
            report.documents += 1;
            Some(document_id)
        } else {
            None
        };
        let marker = document_id.map_or(String::new(), |id| format!("; document_id={id}"));
        let body = format!(
            "[Legacy report #{}; kind={:?}; session={}{}] {}\n{}",
            r.id,
            r.kind,
            r.session_id,
            marker,
            r.title,
            r.body.as_deref().unwrap_or("")
        );
        ordered.push((
            r.created_at,
            info(
                format!("legacy-report-{}", r.id),
                scope,
                if r.to_orchestrator {
                    d::Recipient::Orchestrator
                } else {
                    d::Recipient::User
                },
                body,
                attachments,
                state.revision,
            ),
        ));
        report.links.push(link(
            r,
            "report",
            r.id,
            "entity",
            "interaction",
            format!("legacy-report-{}", r.id),
        )?);
        report.reports += 1;
    }
    for m in view.messages.values() {
        let scope = match &m.about {
            Some(old::MessageAbout::Task { task }) => legacy_scope(Some(task), &state),
            _ => d::Scope::Orchestrator { request_id: None },
        };
        let attachments = attachments(&m.attachments, source_home, target_home, &mut report)?;
        map_attachment_links(&mut report, "message", m.id, &m.attachments, &attachments)?;
        ordered.push((
            m.created_at,
            info(
                format!("legacy-message-{}", m.id),
                scope,
                d::Recipient::Orchestrator,
                format!(
                    "[Legacy message #{}; from={:?}; session={}] {}",
                    m.id, m.from, m.session_id, m.text
                ),
                attachments,
                state.revision,
            ),
        ));
        report.links.push(link(
            m,
            "message",
            m.id,
            "entity",
            "interaction",
            format!("legacy-message-{}", m.id),
        )?);
        report.messages += 1;
    }
    for attempt in view.delivery_attempts.values().filter(|attempt| super::unresolved_delivery(attempt)) {
        let agent_id = d::AgentId(attempt.session_id.clone());
        let task = state
            .tasks
            .values()
            .find(|task| task.agent_id.as_ref() == Some(&agent_id) && !task.state.terminal())
            .map(|task| task.id.clone());
        let retryable = attempt.state == alondra_core::delivery::State::Retryable;
        let (recipient, unread, outcome) = if retryable && state.agents.contains_key(&agent_id) {
            report.conserved_undelivered += 1;
            (d::Recipient::Agent { agent_id }, true,
                "never reached the session and waits for its Agent; it is not resent automatically")
        } else if retryable {
            report.conserved_historical += 1;
            (d::Recipient::User, false, "never reached the session, which was not imported")
        } else {
            report.conserved_uncertain += 1;
            (d::Recipient::User, true,
                "may already have reached the session; it is never resent automatically")
        };
        let mut body = format!(
            "[Legacy delivery #{}; state={:?}; channel={:?}; reason={}; session={}] This delivery {outcome}.",
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
        ordered.push((
            attempt.updated_at,
            d::Interaction {
                id: d::InteractionId(id.clone()),
                scope: task
                    .map(|task_id| d::Scope::Task { task_id })
                    .unwrap_or(d::Scope::Orchestrator { request_id: None }),
                author: d::Author::System,
                recipient,
                body,
                blocking: false,
                subject: None,
                payload: d::InteractionPayload::Information {
                    read_by: if unread { BTreeSet::new() } else { BTreeSet::from(["legacy-import".into()]) },
                },
                attachments: vec![],
                revision: state.revision,
                order: 0,
            },
        ));
        report.links.push(link(attempt, "delivery_attempt", attempt.id, "conserved", "interaction", id)?);
    }
    // Work in flight keeps its review context: an open lifetime PR and/or the
    // legacy final report become the Task's current final presentation, so the
    // user can decide on it and the live Agent can complete the Task in v2.
    let active: Vec<_> = state
        .tasks
        .values()
        .filter(|task| !task.state.terminal())
        .map(|task| task.id.clone())
        .collect();
    for task_id in active {
        let mut targets = vec![];
        let mut at = 0;
        let task = &state.tasks[&task_id];
        if let Some(pr) = task.pr_id.as_ref().map(|id| &state.pull_requests[id]) {
            if pr.state == d::PrState::Open {
                targets.push(d::DeliveryTarget::PullRequest {
                    pr_id: pr.id.clone(),
                    head: pr.head.clone(),
                });
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
            if r.kind == old::ReportKind::Document && task.documents.contains_key(&document_id) {
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
        ordered.push((
            at,
            d::Interaction {
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
                payload: d::InteractionPayload::Delivery {
                    targets,
                    is_final: true,
                    acceptance: None,
                    feedback: vec![],
                },
                attachments: vec![],
                revision: state.revision,
                order: 0,
            },
        ));
        state.tasks.get_mut(&task_id).unwrap().final_presentation =
            Some(d::InteractionId(interaction_id.clone()));
        report.links.push(link(&task_id.0, "task", &task_id.0, "final_presentation", "interaction", &interaction_id)?);
        report.final_presentations += 1;
    }
    ordered.sort_by(|a, b| (a.0, &a.1.id).cmp(&(b.0, &b.1.id)));
    for (index, (_, mut interaction)) in ordered.into_iter().enumerate() {
        interaction.order = index as u64 + 1;
        state
            .interactions
            .insert(interaction.id.clone(), interaction);
    }
    state.revision = state.revision.max(state.interactions.len() as u64);
    let legacy_states: BTreeMap<_, _> = state
        .tasks
        .iter()
        .map(|(id, task)| (id.clone(), task.state))
        .collect();
    state.derive_states();
    report.d02_waiting_dependency = state
        .tasks
        .values()
        .filter(|task| {
            task.state == d::TaskState::WaitingDependency
                && legacy_states[&task.id] != d::TaskState::WaitingDependency
        })
        .map(|task| task.id.0.clone())
        .collect();
    state.validate().map_err(anyhow::Error::new)?;
    ensure!(
        report.questions == view.questions.len()
            && report.reports == view.reports.len()
            && report.messages == view.messages.len()
            && report.formal_prs + report.reference_only_prs.len() == view.prs.len(),
        "legacy import lost a card"
    );
    validate_links(&state, &report.links)?;
    let coverage = |kind: &str, role: &str| {
        report
            .links
            .iter()
            .filter(|v| v.source_kind == kind && v.role == role)
            .count()
    };
    ensure!(
        coverage("request", "entity") == view.board.requests.len()
            && coverage("request", "record") == view.board.requests.len()
            && coverage("request", "closure") == report.request_closures
            && coverage("task", "entity") == view.board.tasks.len()
            && coverage("pr", "record") == view.prs.len()
            && coverage("question", "entity") == view.questions.len()
            && coverage("report", "entity") == view.reports.len()
            && coverage("message", "entity") == view.messages.len()
            && coverage("attachment", "content") == report.attachments
            && coverage("delivery_attempt", "conserved")
                == view.delivery_attempts.values().filter(|a| super::unresolved_delivery(a)).count(),
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
fn legacy_scope(task: Option<&str>, state: &d::DomainState) -> d::Scope {
    task.filter(|id| state.tasks.contains_key(&d::TaskId((*id).into())))
        .map(|id| d::Scope::Task {
            task_id: d::TaskId(id.into()),
        })
        .unwrap_or(d::Scope::Orchestrator { request_id: None })
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
    }
}
pub(super) fn stored_file(home: &Path, attachment: &old::Attachment) -> Result<PathBuf> {
    let stored = attachment
        .stored_path
        .as_ref()
        .context("legacy attachment has no stored copy")?;
    let path = Path::new(stored);
    let canonical_home = home.canonicalize()?;
    let path = if path.is_absolute() {
        if path.starts_with(home) || path.starts_with(&canonical_home) {
            path.to_owned()
        } else {
            // A coherent backup records its original HOME. Reject a path from
            // any other tree even if it happens to share a `files/` suffix.
            let manifest: super::BackupManifest =
                serde_json::from_slice(&fs::read(home.join("manifest.json"))?)?;
            ensure!(manifest.format == 1, "unsupported backup manifest");
            let original_files = Path::new(&manifest.source_home).join("files");
            let canonical_original_files = Path::new(&manifest.canonical_source_home).join("files");
            let suffix = path
                .strip_prefix(&original_files)
                .or_else(|_| path.strip_prefix(&canonical_original_files))
                .context("legacy attachment was outside backed-up HOME files")?;
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
    home: &Path,
    attachment: &old::Attachment,
) -> Result<(PathBuf, Vec<u8>)> {
    let file = stored_file(home, attachment)?;
    let bytes = fs::read(&file)?;
    let marker = home.join("manifest.json");
    if marker.exists() {
        let manifest: super::BackupManifest = serde_json::from_slice(&fs::read(marker)?)?;
        ensure!(manifest.format == 1, "unsupported backup manifest");
        let canonical_home = home.canonicalize()?;
        let relative = file
            .strip_prefix(home)
            .or_else(|_| file.strip_prefix(&canonical_home))
            .context("stored file is outside backup HOME")?
            .to_string_lossy()
            .into_owned();
        let entry = manifest
            .files
            .iter()
            .find(|item| item.path == relative)
            .context("stored file is absent from backup manifest")?;
        ensure!(
            entry.bytes == bytes.len() as u64
                && entry.sha256 == super::hex(&Sha256::digest(&bytes)),
            "stored file differs from backup manifest at consumption"
        );
    }
    Ok((file, bytes))
}
fn attachments(
    input: &[old::Attachment],
    source: &Path,
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
