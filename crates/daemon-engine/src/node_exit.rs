//! What happens when a node's session ends for good: the user stopped or deleted it, or Alondra
//! replaced or closed it. Its open tasks go back to not started with no node, and the node closes.
use crate::live::Relaunch;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{AgentId, Command, CommandEnvelope, NodeEvent, TaskId, PROTOCOL_VERSION};

/// Whether the user stopped, killed or deleted the session in Claude agents: its job file says
/// `stopped`, or the job folder Alondra last saw is gone. A session whose job was never seen is
/// not known to be closed.
pub(crate) fn session_ended(domain: &DomainRuntime, session: &str) -> bool {
    session_ended_in(domain, &crate::watcher::claude_home(), session)
}

/// `session_ended` with Claude Code's home at `claude_home`.
pub(crate) fn session_ended_in(domain: &DomainRuntime, claude_home: &std::path::Path, session: &str) -> bool {
    let known = domain.live.mods.session(session).and_then(|reported| reported.job_dir);
    let Some(dir) = known.or_else(|| crate::job_state::locate(claude_home, session)) else { return false };
    !dir.exists() || crate::job_state::read_in(&dir, session).is_some_and(|job| job.phase == crate::job_state::Phase::Stopped)
}

pub(crate) fn has_open_task(domain: &DomainRuntime, session: &str) -> bool {
    domain.state().is_ok_and(|state| open_task(&state, session))
}

fn open_task(state: &alondra_core::domain::DomainState, session: &str) -> bool {
    let agent = AgentId(session.to_owned());
    state.agents.contains_key(&agent) && state.tasks.values().any(|task| task.agent_id.as_ref() == Some(&agent) && !task.over() && !task.archived && task.wrap_up.asked_at.is_none())
}

/// The user closed the session: the node closes, is not brought back, and the tasks it had
/// open go back to not started with no node, for the user to relaunch. What a relaunch needs of
/// the node is kept.
/// A session whose replacement stopped short because it could not be stopped ends for that
/// reason, and Alondra starts its tasks' next node by itself.
pub(crate) fn user_closed(domain: &DomainRuntime, session: &str) -> anyhow::Result<()> {
    let cwd = domain.live.mods.session(session).and_then(|reported| reported.cwd);
    let Some(why) = domain.live.take_aborted_replace(session) else {
        take_out(domain, session, "node closed by the user", cwd, Relaunch::AskUser)?;
        return Ok(());
    };
    for task_id in take_out(domain, session, REPLACE_ABORTED, cwd, Relaunch::ByItself)? {
        log!("{task_id} has no node: its node ended after a replacement that could not stop it ({why}); Alondra starts a new one");
    }
    Ok(())
}

pub(crate) const REPLACE_ABORTED: &str = "node ended after the orchestrator's replacement could not stop it";

/// `user_closed` for a session that ends for `reason`; a relaunch of its tasks starts in `cwd`,
/// by whoever `relaunch` names. The tasks it frees.
pub(crate) fn take_out(domain: &DomainRuntime, session: &str, reason: &str, cwd: Option<String>, relaunch: Relaunch) -> anyhow::Result<Vec<TaskId>> {
    let agent = AgentId(session.to_owned());
    let state = domain.state()?;
    let name = state.agents.get(&agent).map_or_else(|| format!("node/{}", &session[..session.len().min(8)]), |known| known.name.clone());
    let node = crate::live::ClosedNode { name, cwd, relaunch };
    let released: Vec<_> = state.tasks.values()
        .filter(|task| task.agent_id.as_ref() == Some(&agent) && !task.over() && !task.archived && task.final_presentation.is_none())
        .map(|task| task.id.clone())
        .collect();
    let mut freed = vec![];
    for task_id in released {
        let state = domain.state()?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("release-{}-{}", task_id.0, crate::auth::random_hex(8)).into(),
            expected_revision: state.revision, command: Command::ReleaseTask { task_id: task_id.clone(), reason: reason.into() },
        };
        match domain.execute_as_system(envelope) {
            Ok(_) => {
                domain.live.remember_closed_task(&task_id.0, node.clone());
                freed.push(task_id);
            }
            Err(failure) => log!("could not free {task_id} after its node was closed: {failure:?}"),
        }
    }
    crate::domain_agent_lifecycle::node_event(domain, &agent, NodeEvent::SessionClosed).map_err(|failure| anyhow::anyhow!("{failure}"))?;
    crate::domain_transport::session_ended(domain, &agent)?;
    domain.live.mark_closed(session);
    domain.live.apply(&alondra_core::Event::NodeRemoved { session_id: session.to_owned() });
    domain.forget_ended_session(session)?;
    Ok(freed)
}
