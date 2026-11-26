//! The `claude --bg` sessions an earlier release planned to move to the Agent SDK, as the rest of
//! the engine meets them: what is said to such a node follows it to its SDK node once its move is
//! done, and how Alondra asks Claude Code about a session and stops it. The moves themselves are
//! carried out above the engine (`bg_handover`).
use alondra_engine_kernel::sdk_backend::SdkOf as _;
use crate::live::LiveOf as _;
use alondra_core::clock;
use alondra_engine_ports::Runtime;
use crate::domain_store::{Handover, HandoverStep};
use crate::node_push::Unreachable;
use alondra_domain_model::*;
pub use alondra_engine_kernel::bg_sessions::{
    BgSessions, ClaudeSessions, EVERY, Handovers, HandoversOf, handover_of, identity, member, still_running, successor,
};

/// Whether `agent`'s session moved, or is moving, to the SDK: nothing restarts or removes it.
pub(crate) fn moved(domain: &dyn Runtime, agent: &AgentId) -> bool {
    handover_of(domain, agent).is_some()
}

/// Whether the outbox messages for `agent` wait for its move instead of reaching it through its
/// node mod.
pub(crate) fn holds_messages(domain: &dyn Runtime, agent: &AgentId) -> bool {
    domain.sdk().is_some() && moved(domain, agent)
}

/// How `tell` reached the node.
pub enum Told {
    /// Down its node mod's connection: resolved once the session takes it.
    Pushed(tokio::sync::oneshot::Receiver<()>),
    /// Through the SDK: it opens the node's session there, or follows it to the SDK node.
    Moving,
}

/// Tells the `claude --bg` node `session` `text` from `from`: down its connection, or to the SDK
/// node its move leads to.
pub fn tell(domain: &dyn Runtime, session: &str, from: &str, text: &str, as_user: bool) -> Result<Told, Unreachable> {
    let agent = AgentId(session.to_owned());
    if domain.sdk().is_some() {
        let stamped = crate::node_push::stamped(from, alondra_core::now_millis(), text);
        if let Some(handover) = handover_of(domain, &agent) {
            follow(domain, handover, stamped);
            return Ok(Told::Moving);
        }
    }
    domain.live().pushes.message(session, from, text, as_user).map(Told::Pushed)
}

/// What reaches a node that already moves goes to the SDK node once the move is done.
fn follow(domain: &dyn Runtime, handover: Handover, text: String) {
    let Some((_, runtime)) = domain.sdk() else { return };
    tokio::spawn(async move {
        let node = handover.node.clone();
        let mut changed = runtime.subscribe();
        loop {
            match handover_of(&*runtime, &node) {
                Some(Handover { step: HandoverStep::Done, task, .. }) => {
                    let command = format!("tell-moved-{}", crate::auth::random_hex(16));
                    if let Err(error) = crate::domain_messages::tell_sdk_node(&*runtime, &task, crate::node_push::ALONDRA, &text, &command, false) {
                        log!("what was said to {node} while it moved did not reach its SDK node: {}", error.message);
                    }
                    return;
                }
                Some(_) => {}
                None => return,
            }
            tokio::select! {
                _ = changed.changed() => {}
                () = clock::sleep(EVERY) => {}
            }
        }
    });
}


