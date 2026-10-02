//! Durable evidence of local handoff; this never proves execution by Claude.
use crate::hub::Hub;
use alondra_core::delivery::{Attempt, Channel, State};
use alondra_core::{Event, View, now_millis};
use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn recovery_events(view: &View) -> Vec<Event> {
    let mut events = vec![];
    if !view.delivery_protocol {
        events.push(Event::DeliveryProtocolActivated);
        let sessions: std::collections::BTreeSet<_> = view
            .messages
            .values()
            .filter(|m| m.handed_at.is_none())
            .map(|m| m.session_id.clone())
            .chain(
                view.nodes
                    .values()
                    .filter(|n| n.compaction.is_some())
                    .map(|n| n.session_id.clone()),
            )
            .collect();
        for sid in sessions {
            let Some(node) = view.nodes.get(&sid) else { continue };
            let ids = view
                .pending_messages(&sid)
                .map(|m| m.id)
                .collect::<Vec<_>>();
            if !ids.is_empty() {
                let channel = if node.in_daemon {
                    Channel::Terminal
                } else {
                    Channel::HookStop
                };
                events.push(Event::DeliveryAttemptRecorded(Attempt {
                    state: State::Uncertain,
                    reason: Some("legacy_delivery_unknown".into()),
                    ..new_attempt(view.seq + events.len() as u64 + 1, &sid, channel, ids)
                }));
            }
            if node.compaction.is_some() {
                events.push(Event::DeliveryAttemptRecorded(Attempt {
                    state: State::Uncertain,
                    reason: Some("legacy_compaction_unknown".into()),
                    ..new_attempt(
                        view.seq + events.len() as u64 + 1,
                        &sid,
                        Channel::Compact,
                        vec![],
                    )
                }));
            }
        }
    }
    let gone = |a: &&Attempt| a.session_id != alondra_core::orchestrator::NODE_ID && !view.nodes.contains_key(&a.session_id);
    for attempt in view.delivery_attempts.values().filter(gone) {
        let mut next = attempt.clone();
        next.close_for_gone_node();
        if next.state != attempt.state {
            next.version += 1;
            next.updated_at = now_millis();
            events.push(Event::DeliveryAttemptRecorded(next));
        }
    }
    for attempt in view
        .delivery_attempts
        .values()
        .filter(|a| a.state == State::Dispatching && (a.channel == Channel::NodeDiscard || !gone(a)))
    {
        let mut next = attempt.clone();
        next.state = State::Uncertain;
        next.reason = Some("process_restarted".into());
        next.version += 1;
        next.updated_at = now_millis();
        events.push(Event::DeliveryAttemptRecorded(next));
    }
    events.extend(view.questions.values().filter(|q| held_by_a_hook(q)).map(|q| Event::QuestionClosed {
        id: q.id,
        status: alondra_core::QuestionStatus::Orphaned,
        note: Some("held by a hook before the daemon restarted".into()),
    }));
    events
}

/// A hook's hold lives in the daemon process. After a restart nothing can answer it, and the
/// open card would hide the menu Claude now shows in the node's terminal.
fn held_by_a_hook(question: &alondra_core::Question) -> bool {
    use alondra_core::QuestionOrigin::{AskUserQuestion, Elicitation, Permission, Plan};
    question.status == alondra_core::QuestionStatus::Open
        && matches!(question.origin, AskUserQuestion | Permission | Plan | Elicitation)
        && !question.tool_use_id.as_deref().is_some_and(|id| id.starts_with("menu:"))
}

fn new_attempt(id: u64, sid: &str, channel: Channel, message_ids: Vec<u64>) -> Attempt {
    let at = now_millis();
    Attempt {
        id,
        session_id: sid.into(),
        channel,
        message_ids,
        question_id: None,
        state: State::Dispatching,
        version: 1,
        reason: None,
        created_at: at,
        updated_at: at,
        resolved_from_version: None,
    }
}

/// Selection and claim share the writer transaction; a second consumer cannot claim the terminal.
pub fn claim(hub: &Hub, sid: &str, channel: Channel) -> Result<Option<Attempt>> {
    let recorded = hub.append_all(|v| {
        if v.delivery_blocked(sid) {
            return Err(());
        }
        let Some(node) = v.nodes.get(sid) else {
            return Err(());
        };
        if node.is_orchestrator { return Err(()); }
        let ids = match channel {
            Channel::Compact
                if node.in_daemon && node.compaction.is_some_and(|c| c.typed_at.is_none()) =>
            {
                vec![]
            }
            Channel::Terminal if node.in_daemon => v
                .deliverable_messages(sid)
                .map(|m| m.id)
                .collect::<Vec<_>>(),
            Channel::HookStop if !node.in_daemon => v
                .deliverable_messages(sid)
                .map(|m| m.id)
                .collect::<Vec<_>>(),
            _ => return Err(()),
        };
        if ids.is_empty() && channel != Channel::Compact {
            return Err(());
        }
        Ok(vec![Event::DeliveryAttemptRecorded(new_attempt(
            v.seq + 1,
            sid,
            channel,
            ids,
        ))])
    })?;
    Ok(recorded.ok().and_then(|rows| {
        rows.into_iter().find_map(|(_, event)| match event {
            Event::DeliveryAttemptRecorded(a) => Some(a),
            _ => None,
        })
    }))
}

pub enum Outcome {
    Handed,
    NotStarted(&'static str),
    Uncertain(&'static str),
}

pub fn finish(hub: &Hub, id: u64, outcome: Outcome) -> Result<()> {
    hub.append_all(|v| {
        let current = v
            .delivery_attempts
            .get(&id)
            .ok_or_else(|| anyhow!("unknown delivery attempt"))?;
        anyhow::ensure!(
            current.state == State::Dispatching,
            "delivery attempt no longer dispatching"
        );
        let mut next = current.clone();
        next.version += 1;
        next.updated_at = now_millis();
        let mut events = vec![];
        match outcome {
            Outcome::Handed => {
                next.state = State::Handed;
                next.reason = None;
                if next.channel == Channel::Compact {
                    events.push(Event::Compaction {
                        session_id: next.session_id.clone(),
                        step: alondra_core::event::CompactionStep::Typed,
                    });
                } else if matches!(next.channel, Channel::MenuDecision | Channel::NodeDiscard) {
                    if let Some(id) = next.question_id {
                        events.push(Event::QuestionDelivered { id });
                        if next.channel == Channel::MenuDecision {
                            events.push(Event::MenuSeen { session_id: next.session_id.clone(), waiting_for: None });
                        }
                    }
                } else {
                    for id in &next.message_ids {
                        if let Some(message) = v.messages.get(id) {
                            events.push(Event::MessageHanded { id: *id });
                            if let Some(id) = message.answers_question {
                                events.push(Event::QuestionDelivered { id });
                            }
                        }
                    }
                }
            }
            Outcome::NotStarted(reason) => {
                next.state = State::Retryable;
                next.reason = Some(reason.into());
                if let Some(id) = next.question_id {
                    if v.questions.get(&id).is_some_and(|q| q.status == alondra_core::QuestionStatus::Answered) {
                        events.push(Event::QuestionClosed { id, status: alondra_core::QuestionStatus::Open, note: None });
                    } else {
                        // Cancellation before any key needs no unresolved retry intent.
                        next.state = State::Discarded;
                    }
                }
            }
            Outcome::Uncertain(reason) => {
                next.state = State::Uncertain;
                next.reason = Some(reason.into());
            }
        }
        events.push(Event::DeliveryAttemptRecorded(next));
        Ok(events)
    })??;
    Ok(())
}

/// Cancellation can be observed by both the waiter and the answer sender. Recheck
/// and finalize once under the writer lock, without undoing ACK or explicit resolution.
pub fn cancel_held_question(hub: &Hub, question: u64, reason: &'static str) -> Result<()> {
    hub.append_all(|v| {
        let intent = v.messages.values().any(|m| m.answers_question == Some(question));
        if let Some(current) = v.delivery_attempts.values().rev().find(|a|
            a.channel == Channel::HookDecision && a.message_ids.iter().any(|id|
                v.messages.get(id).is_some_and(|m| m.answers_question == Some(question)))) {
            if current.state != State::Dispatching { return Ok::<_, anyhow::Error>(vec![]); }
            let mut next = current.clone();
            next.state = State::Uncertain;
            next.reason = Some(reason.into());
            next.version += 1;
            next.updated_at = now_millis();
            return Ok(vec![Event::DeliveryAttemptRecorded(next)]);
        }
        // An answer committed an ordinary outbox after the receiver disappeared.
        // Its durable intent remains deliverable; cancellation must not orphan it.
        if intent { return Ok(vec![]); }
        Ok(v.questions.get(&question)
            .filter(|q| matches!(q.status, alondra_core::QuestionStatus::Open | alondra_core::QuestionStatus::Answered))
            .map(|_| Event::QuestionClosed { id: question, status: alondra_core::QuestionStatus::Orphaned, note: None })
            .into_iter().collect())
    })??;
    Ok(())
}

/// Recheck under the writer lock: PostCompact may arrive after the wake scan.
pub fn mark_compaction_overdue(hub: &Hub, sid: &str, now: i64) -> Result<()> {
    hub.append_with(|v| {
        let node = v.nodes.get(sid)?;
        if v.delivery_blocked(sid)
            || !alondra_core::upkeep::compaction_overdue(node, now)
            || !node.compaction.is_some_and(|c| c.typed_at.is_some())
        {
            return None;
        }
        let old = v
            .delivery_attempts
            .values()
            .rev()
            .find(|a| a.session_id == sid && a.channel == Channel::Compact)?;
        if old.state != State::Handed {
            return None;
        }
        let mut next = old.clone();
        next.state = State::Uncertain;
        next.version += 1;
        next.reason = Some("compaction_unconfirmed".into());
        next.updated_at = now;
        Some(Event::DeliveryAttemptRecorded(next))
    })?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    pub expected_version: u64,
    pub terminal_checked: bool,
    #[serde(default)]
    pub confirm_duplicate_risk: bool,
}

pub fn resolve(
    hub: &Hub,
    identity: Option<(&str, &str, &[u8; 32])>,
    id: u64,
    retry: bool,
    body: Resolution,
) -> Result<Value> {
    hub.local_command(
        identity,
        |v| {
            let error = |status, msg: &str| json!({"status":status,"body":{"error":msg}});
            if !body.terminal_checked || retry && !body.confirm_duplicate_risk {
                return Err(error(
                    422,
                    "inspect the terminal and explicitly confirm duplicate risk before retrying",
                ));
            }
            let Some(current) = v.delivery_attempts.get(&id) else {
                return Err(error(404, "delivery attempt not found"));
            };
            let target = if retry {
                State::Retryable
            } else {
                State::Discarded
            };
            // An identical versioned decision remains successful after a worker starts a new attempt.
            if current.state == target
                && current.resolved_from_version == Some(body.expected_version)
            {
                return Ok(vec![]);
            }
            if current.state != State::Uncertain || current.version != body.expected_version {
                return Err(error(
                    409,
                    "delivery attempt changed; refresh before deciding",
                ));
            }
            if retry && matches!(current.channel, Channel::MenuDecision | Channel::NodeDiscard) && current.question_id
                .is_none_or(|id| v.questions.get(&id).is_none_or(|q| q.status != alondra_core::QuestionStatus::Answered)) {
                return Err(error(409, "question was closed; inspect the destination instead of retrying"));
            }
            if retry && current.channel == Channel::NodeDiscard && !v.nodes.contains_key(&current.session_id) {
                return Err(error(409, "node no longer exists; inspect the worktree and discard this intent instead"));
            }
            let mut next = current.clone();
            next.state = target;
            next.resolved_from_version = Some(current.version);
            next.version += 1;
            next.updated_at = now_millis();
            next.reason = Some(
                if retry {
                    "explicit_retry"
                } else {
                    "explicit_discard"
                }
                .into(),
            );
            let mut events = vec![];
            if next.channel == Channel::Compact {
                events.push(Event::Compaction {
                    session_id: next.session_id.clone(),
                    step: if retry {
                        alondra_core::event::CompactionStep::Requested
                    } else {
                        alondra_core::event::CompactionStep::GaveUp
                    },
                });
            }
            if matches!(next.channel, Channel::MenuDecision | Channel::NodeDiscard) {
                if let Some(id) = next.question_id {
                    events.push(Event::QuestionClosed { id,
                        status: if retry { alondra_core::QuestionStatus::Open } else { alondra_core::QuestionStatus::Withdrawn },
                        note: None });
                }
            }
            events.push(Event::DeliveryAttemptRecorded(next));
            Ok(events)
        },
        |v, _| json!({"status":200,"body":{"attempt":v.delivery_attempts[&id]}}),
    )
}

/// Render additive status fields without changing legacy handed/delivered timestamps.
pub fn message_value(view: &View, message: &alondra_core::Message) -> Value {
    let mut result = json!(message);
    if let Some(attempt) = view.message_attempt(message.id) {
        result["delivery_attempt"] = json!(attempt.id);
        result["delivery_state"] = json!(attempt.state);
        result["delivery_reason"] = json!(attempt.reason);
        result["delivery_version"] = json!(attempt.version);
    } else {
        result["delivery_state"] = json!(if message.handed_at.is_some() {
            "handed"
        } else {
            "queued"
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    /// David's snapshot already held the 27-sep migration's uncertain handoffs for nodes closed
    /// on 23-sep, so replaying events alone could never clear the TUI warning.
    #[test]
    fn restart_closes_saved_handoffs_whose_node_is_gone() {
        let mut view = View::default();
        view.delivery_protocol = true;
        view.nodes.insert("live".into(), alondra_core::Node::new("live", 1));
        for (id, sid, channel) in [(1, "prueba-mac-ui", Channel::HookStop), (2, "live", Channel::HookStop), (3, "gone", Channel::NodeDiscard)] {
            view.delivery_attempts.insert(id, Attempt { state: State::Uncertain, reason: Some("legacy_delivery_unknown".into()), ..new_attempt(id, sid, channel, vec![id]) });
        }
        let closed: Vec<Attempt> = recovery_events(&view).into_iter().filter_map(|e| match e { Event::DeliveryAttemptRecorded(a) => Some(a), _ => None }).collect();
        assert_eq!(closed.len(), 1, "{closed:?}");
        assert_eq!((closed[0].id, closed[0].state, closed[0].version), (1, State::Discarded, 2));
    }
    #[test]
    fn restart_orphans_cards_that_a_hook_was_holding() {
        let mut view = View::default();
        view.delivery_protocol = true;
        let card = |id: u64, origin: &str, tool: Option<&str>, status: &str| -> alondra_core::Question {
            serde_json::from_value(serde_json::json!({"id": id, "session_id": "s", "node_name": null,
                "title": "t", "blocking": true, "origin": origin, "tool_use_id": tool,
                "status": status, "asked_at": 1})).unwrap()
        };
        for q in [card(1, "permission", Some("toolu_1"), "open"), card(2, "ask_user_question", None, "open"),
                  card(3, "plan", None, "open"), card(4, "elicitation", None, "open"),
                  card(5, "permission", Some("menu:ab12"), "open"), card(6, "ask", None, "open"),
                  card(7, "classifier", None, "open"), card(8, "permission", None, "answered")] {
            view.questions.insert(q.id, q);
        }
        let orphaned: Vec<u64> = recovery_events(&view).into_iter().filter_map(|e| match e {
            Event::QuestionClosed { id, status: alondra_core::QuestionStatus::Orphaned, .. } => Some(id),
            _ => None,
        }).collect();
        assert_eq!(orphaned, vec![1, 2, 3, 4]);
    }
    fn fixture() -> Arc<Hub> {
        let home = std::env::temp_dir().join(format!("delivery-{}", crate::auth::random_hex(8)));
        let config = alondra_core::config::Config::resolve(
            home,
            &Default::default(),
            |_| None,
            || Some("Test".into()),
        );
        let hub = Arc::new(Hub::open(config).unwrap());
        hub.append(Event::DaemonSeen(alondra_core::DaemonObservation {
            session_id: "s".into(),
            short_id: "s".into(),
            ..Default::default()
        }))
        .unwrap();
        for id in [1, 2] {
            hub.append(Event::MessageQueued(alondra_core::Message {
                id,
                session_id: "s".into(),
                from: alondra_core::MessageFrom::User,
                text: format!("message {id}"),
                answers_question: None,
                created_at: 1,
                handed_at: None,
                delivered_at: None,
                attachments: vec![],
                about: None,
            }))
            .unwrap();
        }
        hub
    }
    fn reopen(hub: &Hub) -> Arc<Hub> {
        Arc::new(Hub::open(hub.config.clone()).unwrap())
    }
    fn decision(version: u64) -> Resolution {
        Resolution {
            expected_version: version,
            terminal_checked: true,
            confirm_duplicate_risk: true,
        }
    }

    fn enqueue_next(hub: &Hub) -> u64 {
        let (_, event) = hub
            .append_with(|v| {
                Some(Event::MessageQueued(alondra_core::Message {
                    id: v.next_message_id(),
                    session_id: "s".into(),
                    from: alondra_core::MessageFrom::User,
                    text: "new work".into(),
                    answers_question: None,
                    created_at: now_millis(),
                    handed_at: None,
                    delivered_at: None,
                    attachments: vec![],
                    about: None,
                }))
            })
            .unwrap()
            .unwrap();
        let Event::MessageQueued(message) = event else {
            unreachable!()
        };
        message.id
    }

    #[test]
    fn delivery_retention_never_reuses_message_identity_after_snapshot() {
        let hub = fixture();
        let attempt = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        finish(&hub, attempt.id, Outcome::Handed).unwrap();
        hub.append(Event::Pruned {
            before: now_millis() + 1,
        })
        .unwrap();
        assert!(hub.read(|v| v.messages.is_empty()));
        hub.compact(std::time::Duration::ZERO).unwrap();
        let restored = reopen(&hub);
        let id = enqueue_next(&restored);
        assert!(
            id > 2,
            "new work must not alias any retained delivery evidence"
        );
        let next = claim(&restored, "s", Channel::Terminal).unwrap().unwrap();
        assert_eq!(next.message_ids, vec![id]);
        let home = hub.home.clone();
        drop(restored);
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_superseded_retry_does_not_retain_payload_or_terminal_metadata_forever() {
        let hub = fixture();
        let first = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        finish(&hub, first.id, Outcome::NotStarted("menu_open")).unwrap();
        let last = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        finish(&hub, last.id, Outcome::Handed).unwrap();
        hub.append(Event::Pruned {
            before: now_millis() + 1,
        })
        .unwrap();
        assert!(
            hub.read(|v| v.messages.is_empty()),
            "historical Retryable is superseded by Handed"
        );
        assert!(
            hub.read(|v| v.delivery_attempts.is_empty()),
            "resolved metadata observes retention"
        );
        let home = hub.home.clone();
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_retention_preserves_answer_target_and_question_identity() {
        let hub = fixture();
        let question = |id| {
            serde_json::from_value::<alondra_core::Question>(json!({
                "id":id,"session_id":"s","node_name":null,"title":"question","blocking":false,
                "origin":"ask","status":"answered","asked_at":1
            }))
            .unwrap()
        };
        hub.append(Event::QuestionAsked(question(1))).unwrap();
        hub.append(Event::MessageQueued(alondra_core::Message {
            id: 3,
            session_id: "s".into(),
            from: alondra_core::MessageFrom::User,
            text: "answer".into(),
            answers_question: Some(1),
            created_at: 1,
            handed_at: None,
            delivered_at: None,
            attachments: vec![],
            about: None,
        }))
        .unwrap();
        let attempt = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        finish(&hub, attempt.id, Outcome::Uncertain("write_unknown")).unwrap();
        hub.append(Event::Pruned {
            before: now_millis() + 1,
        })
        .unwrap();
        assert!(
            hub.read(|v| v.questions.contains_key(&1)),
            "unresolved answer retains its question"
        );
        let id = hub.read(|v| v.next_question_id());
        assert!(id > 1);
        hub.append(Event::QuestionAsked(question(id))).unwrap();
        resolve(&hub, None, attempt.id, true, decision(2)).unwrap();
        let next = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        finish(&hub, next.id, Outcome::Handed).unwrap();
        assert_eq!(
            hub.read(|v| v.questions[&id].status),
            alondra_core::QuestionStatus::Answered
        );
        hub.append(Event::Pruned {
            before: now_millis() + 1,
        })
        .unwrap();
        assert!(hub.read(|v| v.questions.is_empty()));
        hub.compact(std::time::Duration::ZERO).unwrap();
        let restored = reopen(&hub);
        assert!(
            restored.read(|v| v.next_question_id()) > id,
            "snapshot retains the question high-water mark"
        );
        let home = hub.home.clone();
        drop(restored);
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_resolution_receipt_survives_attempt_retention() {
        let hub = fixture();
        let attempt = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        finish(&hub, attempt.id, Outcome::Uncertain("write_unknown")).unwrap();
        let digest = [42; 32];
        let identity = Some(("client", "discard-request", &digest));
        let result = resolve(&hub, identity, attempt.id, false, decision(2)).unwrap();
        hub.append(Event::Pruned {
            before: now_millis() + 1,
        })
        .unwrap();
        assert!(hub.read(|v| v.delivery_attempts.is_empty()));
        let restored = reopen(&hub);
        assert_eq!(
            resolve(&restored, identity, attempt.id, false, decision(2)).unwrap(),
            result
        );
        assert_eq!(
            resolve(&restored, None, attempt.id, false, decision(2)).unwrap()["status"],
            404
        );
        let home = hub.home.clone();
        drop(restored);
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_uncertainty_changes_long_poll_digest_and_message_status_without_fake_delivery() {
        let hub = fixture();
        let at = now_millis();
        let before = hub.read(|v| crate::http::overview_digest(v, at));
        let attempt = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        finish(&hub, attempt.id, Outcome::Uncertain("partial_write")).unwrap();
        let after = hub.read(|v| crate::http::overview_digest(v, at));
        assert_ne!(before, after);
        let message = hub.read(|v| message_value(v, &v.messages[&1]));
        assert_eq!(message["delivery_attempt"], attempt.id);
        assert_eq!(message["delivery_state"], "uncertain");
        assert_eq!(message["delivery_reason"], "partial_write");
        assert_eq!(message["delivery_version"], 2);
        assert!(message["handed_at"].is_null());
        assert!(message["delivered_at"].is_null());
        resolve(&hub, None, attempt.id, false, decision(2)).unwrap();
        assert_ne!(hub.read(|v| crate::http::overview_digest(v, at)), after);
        let home = hub.home.clone();
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_compaction_timeout_rechecks_post_compact_before_marking_uncertain() {
        for completed in [false, true] {
            let hub = fixture();
            hub.append(Event::Compaction {
                session_id: "s".into(),
                step: alondra_core::event::CompactionStep::Requested,
            })
            .unwrap();
            let attempt = claim(&hub, "s", Channel::Compact).unwrap().unwrap();
            finish(&hub, attempt.id, Outcome::Handed).unwrap();
            let overdue = now_millis() + 86_400_000;
            assert!(hub.read(|v| alondra_core::upkeep::compaction_overdue(&v.nodes["s"], overdue)));
            if completed {
                hub.append(Event::Hook(alondra_core::HookEvent {
                    session_id: "s".into(),
                    event: "PostCompact".into(),
                    ..Default::default()
                }))
                .unwrap();
            }
            mark_compaction_overdue(&hub, "s", overdue).unwrap();
            assert_eq!(
                hub.read(|v| v.delivery_attempts[&attempt.id].state),
                if completed {
                    State::Handed
                } else {
                    State::Uncertain
                }
            );
            let home = hub.home.clone();
            drop(hub);
            std::fs::remove_dir_all(home).unwrap();
        }
    }

    #[test]
    fn delivery_compaction_uncertainty_blocks_messages_until_explicit_resolution() {
        let hub = fixture();
        hub.append(Event::Compaction {
            session_id: "s".into(),
            step: alondra_core::event::CompactionStep::Requested,
        })
        .unwrap();
        let attempt = claim(&hub, "s", Channel::Compact).unwrap().unwrap();
        finish(&hub, attempt.id, Outcome::Uncertain("partial_compact")).unwrap();
        assert!(claim(&hub, "s", Channel::Terminal).unwrap().is_none());
        assert!(claim(&hub, "s", Channel::Compact).unwrap().is_none());
        assert_eq!(
            resolve(&hub, None, attempt.id, true, decision(2)).unwrap()["status"],
            200
        );
        let next = claim(&hub, "s", Channel::Compact).unwrap().unwrap();
        finish(&hub, next.id, Outcome::Handed).unwrap();
        assert!(hub.read(|v| v.nodes["s"].compaction.unwrap().typed_at.is_some()));
        assert!(
            claim(&hub, "s", Channel::Terminal).unwrap().is_none(),
            "no message before actual compact completion"
        );
        hub.append(Event::Hook(alondra_core::HookEvent {
            session_id: "s".into(),
            event: "PostCompact".into(),
            ..Default::default()
        }))
        .unwrap();
        assert!(claim(&hub, "s", Channel::Terminal).unwrap().is_some());
        let home = hub.home.clone();
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_claim_and_completion_are_atomic_before_and_after_external_write() {
        let hub = fixture();
        let db = rusqlite::Connection::open(hub.home.join("alondra.db")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_delivery BEFORE INSERT ON event_log WHEN NEW.kind='delivery_attempt_recorded' BEGIN SELECT RAISE(ABORT,'injected delivery failure'); END;").unwrap();
        let before = hub.read(|v| v.seq);
        assert!(claim(&hub, "s", Channel::Terminal).is_err());
        assert_eq!(hub.read(|v| v.seq), before);
        assert!(!hub.read(|v| v.delivery_blocked("s")));
        db.execute_batch("DROP TRIGGER fail_delivery;").unwrap();
        let attempt = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        assert_eq!(attempt.message_ids, vec![1, 2]);
        // External write succeeds, then the final marker fails: earlier MessageHanded rows roll back too.
        db.execute_batch("CREATE TRIGGER fail_delivery BEFORE INSERT ON event_log WHEN NEW.kind='delivery_attempt_recorded' BEGIN SELECT RAISE(ABORT,'injected final receipt failure'); END;").unwrap();
        assert!(finish(&hub, attempt.id, Outcome::Handed).is_err());
        assert!(hub.read(|v| {
            v.messages
                .values()
                .all(|m| m.handed_at.is_none() && m.delivered_at.is_none())
        }));
        assert!(claim(&hub, "s", Channel::Terminal).unwrap().is_none());
        db.execute_batch("DROP TRIGGER fail_delivery;").unwrap();
        let reopened = reopen(&hub);
        assert_eq!(
            reopened.read(|v| v.delivery_attempts[&attempt.id].state),
            State::Uncertain
        );
        assert!(claim(&reopened, "s", Channel::Terminal).unwrap().is_none());
        assert!(reopened.read(|v| v.messages.values().all(|m| m.handed_at.is_none())));
        let home = hub.home.clone();
        drop(db);
        drop(hub);
        drop(reopened);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_restart_retry_and_discard_preserve_intent_without_fake_delivery() {
        let hub = fixture();
        let first = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        let hub2 = reopen(&hub);
        assert_eq!(hub2.read(|v| v.deliverable_messages("s").count()), 0);
        hub2.compact(std::time::Duration::ZERO).unwrap();
        assert_eq!(
            hub2.read(|v| v.messages.len()),
            2,
            "retention preserves unresolved payloads"
        );
        let version = hub2.read(|v| v.delivery_attempts[&first.id].version);
        let result = resolve(&hub2, None, first.id, true, decision(version)).unwrap();
        assert_eq!(result["status"], 200);
        let next = claim(&hub2, "s", Channel::Terminal).unwrap().unwrap();
        assert_ne!(first.id, next.id);
        assert_eq!(next.message_ids, first.message_ids);
        assert_eq!(
            resolve(&hub2, None, first.id, true, decision(version)).unwrap(),
            result,
            "duplicate retry does not release another batch"
        );
        assert!(claim(&hub2, "s", Channel::Terminal).unwrap().is_none());
        finish(&hub2, next.id, Outcome::Uncertain("partial_write")).unwrap();
        assert_eq!(
            resolve(&hub2, None, next.id, false, decision(1)).unwrap()["status"],
            409
        );
        let discarded = resolve(&hub2, None, next.id, false, decision(2)).unwrap();
        assert_eq!(discarded["status"], 200);
        assert_eq!(
            resolve(&hub2, None, next.id, false, decision(2)).unwrap(),
            discarded
        );
        assert!(hub2.read(|v| {
            v.messages
                .values()
                .all(|m| m.handed_at.is_none() && m.delivered_at.is_none())
        }));
        let hub3 = reopen(&hub2);
        assert!(claim(&hub3, "s", Channel::Terminal).unwrap().is_none());
        let home = hub.home.clone();
        drop(hub);
        drop(hub2);
        drop(hub3);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_concurrent_consumers_claim_one_batch_and_safe_failure_can_retry() {
        let hub = fixture();
        let gate = Arc::new(std::sync::Barrier::new(3));
        let workers = (0..2)
            .map(|_| {
                let hub = hub.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    gate.wait();
                    claim(&hub, "s", Channel::Terminal).unwrap()
                })
            })
            .collect::<Vec<_>>();
        gate.wait();
        let claims = workers
            .into_iter()
            .filter_map(|w| w.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(claims.len(), 1);
        finish(&hub, claims[0].id, Outcome::NotStarted("menu_open")).unwrap();
        let next = claim(&hub, "s", Channel::Terminal).unwrap().unwrap();
        finish(&hub, next.id, Outcome::Handed).unwrap();
        assert!(hub.read(|v| {
            v.messages
                .values()
                .all(|m| m.handed_at.is_some() && m.delivered_at.is_none())
        }));
        assert!(claim(&hub, "s", Channel::Terminal).unwrap().is_none());
        let home = hub.home.clone();
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn delivery_legacy_migration_blocks_messages_and_compaction_without_marking_handed() {
        let hub = fixture();
        hub.append(Event::Compaction {
            session_id: "s".into(),
            step: alondra_core::event::CompactionStep::Requested,
        })
        .unwrap();
        let home = hub.home.clone();
        drop(hub);
        // Reconstruct a previous-version layout with the old activation record absent.
        let db = rusqlite::Connection::open(home.join("alondra.db")).unwrap();
        db.execute_batch("DELETE FROM event_log WHERE kind='delivery_protocol_activated'; DROP VIEW events; DROP INDEX event_log_session; ALTER TABLE event_log RENAME TO events; CREATE INDEX events_session ON events(session);").unwrap();
        drop(db);
        let config = alondra_core::config::Config::resolve(
            home.clone(),
            &Default::default(),
            |_| None,
            || Some("Test".into()),
        );
        let hub = Hub::open(config).unwrap();
        assert_eq!(hub.read(|v| v.delivery_attempts.len()), 2);
        assert!(hub.read(|v| {
            v.delivery_attempts
                .values()
                .all(|a| a.state == State::Uncertain)
        }));
        assert!(claim(&hub, "s", Channel::Terminal).unwrap().is_none());
        assert!(claim(&hub, "s", Channel::Compact).unwrap().is_none());
        assert!(hub.read(|v| v.messages.values().all(|m| m.handed_at.is_none())));
        drop(hub);
        std::fs::remove_dir_all(home).unwrap();
    }
}
