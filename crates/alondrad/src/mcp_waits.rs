//! The tasks that wait for an MCP server, from the daemon's side. A card stands for every task
//! that waits for one server: Connect opens one sign-in, on the node of the oldest that can, and
//! hands the user its page; where the browser came back to reaches that node's Claude in memory
//! (`mcp_sign_in`); Reconnect has the waiting nodes read their sign-in again. While a sign-in comes
//! back to Claude's own listener, the listener closing is the fact that it ended: the node is
//! asked how its servers are then. Once a node reports a server connected, every other node that
//! reports it signed out connects it again.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{
    Actor, Command, CommandEnvelope, DomainError, DomainState, InteractionId, McpServerKey, McpServerState, McpStatus, McpWaiting, Millis,
    OpenSignIn, PROTOCOL_VERSION, TaskId,
};
use std::collections::{BTreeSet, HashMap};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// How long Claude may take to open a sign-in or say how its servers are.
const ANSWER: Duration = Duration::from_secs(20);
/// How often an open sign-in's listener is looked at.
const LISTENER: Duration = Duration::from_secs(2);
/// Claude keeps what it read of its stored sign-ins this long: a reconnect inside it may read the
/// old one, so another follows once it is over.
pub(crate) const SIGN_IN_CACHE: Millis = 30_000;
const RECONNECTS: u32 = 3;

/// What Connect hands the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Opened {
    /// The user signs in at `url`; with `returns_to_claude` the browser then comes back to
    /// Claude's listener on the Mac, and elsewhere the user pastes where it came back to.
    Browser { server: String, url: String, returns_to_claude: bool },
    /// Claude's stored sign-in works: nothing to do in a browser.
    Connected { server: String },
}

fn envelope(state: &DomainState, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("mcp-{}", crate::auth::random_hex(16)).into(),
        expected_revision: state.revision,
        command,
    }
}

/// Runs `command` as Alondra when its guard lets it now.
fn system(domain: &DomainRuntime, command: Command) -> Result<u64, DomainError> {
    let state = domain.state().map_err(|error| DomainError::new("storage_unavailable", error.to_string()))?;
    let envelope = envelope(&state, command);
    state.guard(&Actor::System, &envelope)?;
    match domain.execute_as_system(envelope) {
        Ok(_) => Ok(domain.read().state().revision),
        Err(crate::domain_store::StoreError::Domain(error)) => Err(error),
        Err(crate::domain_store::StoreError::Storage(error)) if error.to_string().contains("UnsupportedCapability") => {
            Err(DomainError::new("conflict", "This node's Claude cannot do it from Alondra: it runs an Agent SDK host from before it could"))
        }
        Err(crate::domain_store::StoreError::Storage(error)) => Err(DomainError::new("storage_unavailable", error.to_string())),
    }
}

fn allowed(state: &DomainState, command: Command) -> bool {
    state.guard(&Actor::System, &envelope(state, command)).is_ok()
}

/// The server a card waits for, while some task still waits for it.
fn waited(state: &DomainState, card: &InteractionId) -> Result<(McpServerKey, Vec<McpWaiting>), DomainError> {
    let key = state.interactions.get(card).and_then(McpServerKey::of_card)
        .ok_or_else(|| DomainError::new("not_found", "This card does not wait for an MCP server"))?;
    let waiting = state.mcp_waiting_for(&key);
    if waiting.is_empty() {
        return Err(DomainError::new("conflict", format!("No task waits for {} any more", key.name)));
    }
    Ok((key, waiting))
}

fn open_sign_in<'a>(state: &'a DomainState, task: &TaskId, server: &str) -> Option<&'a OpenSignIn> {
    let agent = state.tasks.get(task)?.agent_id.as_ref()?;
    state.mcp.get(agent)?.sign_ins.get(server)
}

/// Whether something listens on `port` of this Mac, as a browser would reach it. An empty
/// connection is what a browser opens ahead of a request; nothing is sent on it.
fn listening(port: u16) -> bool {
    let addresses: Vec<SocketAddr> = ("localhost", port).to_socket_addrs().map(Iterator::collect).unwrap_or_default();
    addresses.iter().any(|address| TcpStream::connect_timeout(address, Duration::from_millis(300)).is_ok())
}

fn still_open(open: &OpenSignIn) -> bool {
    open.callback_port.is_none_or(listening)
}

fn browser(server: &str, open: &OpenSignIn) -> Opened {
    Opened::Browser { server: server.into(), url: open.url.clone(), returns_to_claude: open.callback_port.is_some() }
}

/// Waits until `outcome` says something of the domain, for at most `ANSWER`.
async fn answered<T>(domain: &DomainRuntime, mut outcome: impl FnMut(&DomainState) -> Option<T>) -> Option<T> {
    let mut changes = domain.subscribe();
    let deadline = tokio::time::Instant::now() + ANSWER;
    loop {
        if let Some(answer) = outcome(domain.read().state()) {
            return Some(answer);
        }
        match tokio::time::timeout_at(deadline, changes.changed()).await {
            Ok(Ok(())) => {}
            _ => return outcome(domain.read().state()),
        }
    }
}

/// How the sign-in Connect asked `task`'s node for goes on, once it is known.
fn opened(state: &DomainState, task: &TaskId, server: &McpServerKey, since: u64) -> Option<Result<Opened, DomainError>> {
    let name = server.name.as_str();
    if !state.mcp_waiting_for(server).iter().any(|waiting| waiting.task == *task) {
        return Some(Ok(Opened::Connected { server: name.into() }));
    }
    if let Some(open) = open_sign_in(state, task, name) {
        return Some(Ok(browser(name, open)));
    }
    let agent = state.tasks.get(task)?.agent_id.as_ref()?;
    let node = state.mcp.get(agent)?;
    if let Some((error, _)) = node.refused.get(name).filter(|(_, at)| *at > since) {
        return Some(Err(DomainError::new("conflict", format!("Claude could not open the sign-in to {name}: {error}"))));
    }
    state.mcp_server_of(task, server).filter(|reported| reported.status == McpStatus::Connected)
        .map(|_| Ok(Opened::Connected { server: name.into() }))
}

/// Connect: the open sign-in of a waiting node whose Claude still waits for it, or a new one on
/// the node of the oldest waiting task that can open it. A node that has not said how its
/// servers are since the daemon started is asked first.
pub(crate) async fn connect(domain: &DomainRuntime, card: &InteractionId) -> Result<Opened, DomainError> {
    for asked in [false, true] {
        let snapshot = domain.read();
        let state = snapshot.state();
        let (key, waiting) = waited(state, card)?;
        let name = key.name.clone();
        for task in waiting.iter().map(|waiting| &waiting.task) {
            if let Some(open) = open_sign_in(state, task, &name).filter(|open| still_open(open)) {
                return Ok(browser(&name, open));
            }
        }
        let start = |task: &TaskId| Command::StartMcpSignIn { task_id: task.clone(), server: name.clone() };
        if let Some(task) = waiting.iter().map(|waiting| &waiting.task).find(|task| allowed(state, start(task))).cloned() {
            drop(snapshot);
            let since = system(domain, start(&task))?;
            return answered(domain, |state| opened(state, &task, &key, since)).await.unwrap_or_else(|| {
                Err(DomainError::new("unavailable", format!("Claude did not open the sign-in to {name}: press Connect again")))
            });
        }
        let read = |task: &TaskId| Command::ReadMcpStatus { task_id: task.clone() };
        let unread: Vec<TaskId> = waiting.iter().map(|waiting| waiting.task.clone())
            .filter(|task| state.mcp_server_of(task, &key).is_none() && allowed(state, read(task))).collect();
        drop(snapshot);
        if asked || unread.is_empty() {
            break;
        }
        for task in &unread {
            let _ = system(domain, read(task));
        }
        answered(domain, |state| unread.iter().any(|task| state.mcp_server_of(task, &key).is_some()).then_some(())).await;
    }
    let name = domain.read().state().interactions.get(card).and_then(McpServerKey::of_card).map(|key| key.name).unwrap_or_default();
    Err(DomainError::new("conflict", format!("No node that waits for {name} can open its sign-in now: go on without it, or try again once one runs")))
}

/// Where the browser came back to after the user signed in on another device: it reaches the
/// node's Claude in memory, never written down, and the node then says how its servers are.
pub(crate) async fn give_back(domain: &Arc<DomainRuntime>, card: &InteractionId, address: String) -> Result<(), DomainError> {
    let state = domain.read();
    let (key, waiting) = waited(state.state(), card)?;
    let name = key.name.clone();
    let task = waiting.iter().map(|waiting| &waiting.task)
        .find(|task| open_sign_in(state.state(), task, &name).is_some_and(|open| open.callback_port.is_some()))
        .cloned()
        .ok_or_else(|| DomainError::new("not_found", format!("No sign-in to {name} is open: press Connect again")))?;
    drop(state);
    let runtime = domain.clone();
    let (server, at) = (name.clone(), task.clone());
    let delivered = tokio::task::spawn_blocking(move || crate::mcp_sign_in::complete(&runtime, &at, &server, &address)).await
        .map_err(|_| DomainError::new("unavailable", "The sign-in could not be handed to Claude"))?;
    match delivered {
        Ok(()) => {}
        Err(crate::mcp_sign_in::SignInError::NotAReturn) => {
            return Err(DomainError::new("invalid_input", "Paste the whole address the browser came back to: it starts with http://localhost: and has /callback"));
        }
        Err(crate::mcp_sign_in::SignInError::Refused(reason)) => return Err(DomainError::new("conflict", reason)),
        Err(crate::mcp_sign_in::SignInError::Undelivered(_)) => {
            return Err(DomainError::new("conflict", format!("The sign-in to {name} was cut: press Connect again")));
        }
    }
    let _ = system(domain, Command::ReadMcpStatus { task_id: task });
    Ok(())
}

/// Reconnect: every waiting node that can reads its sign-in to the server again.
pub(crate) fn reconnect(domain: &DomainRuntime, card: &InteractionId) -> Result<usize, DomainError> {
    let (key, waiting) = waited(domain.read().state(), card)?;
    let reconnected = waiting.iter()
        .filter(|waiting| system(domain, Command::ReconnectMcp { task_id: waiting.task.clone(), server: key.name.clone() }).is_ok())
        .count();
    if reconnected == 0 {
        return Err(DomainError::new("conflict", format!("No node that waits for {} runs now: try again once one does", key.name)));
    }
    Ok(reconnected)
}

/// The reconnects owed to nodes that report signed out a server another node reports connected:
/// one at once, and again after Claude's cache of stored sign-ins is over, at most `RECONNECTS`.
#[derive(Default)]
pub(crate) struct Reconnects {
    tried: HashMap<(TaskId, String), (u32, Millis)>,
}

impl Reconnects {
    /// What is due now, from the servers each open task's node reported.
    pub(crate) fn due(&mut self, nodes: &[(TaskId, &[McpServerState])], now: Millis) -> (Vec<(TaskId, String)>, Option<Millis>) {
        let connected: BTreeSet<McpServerKey> = nodes.iter()
            .flat_map(|(_, servers)| servers.iter().filter(|server| server.status == McpStatus::Connected).map(McpServerKey::of))
            .collect();
        let mut owed = BTreeSet::new();
        for (task, servers) in nodes {
            for server in servers.iter().filter(|server| matches!(server.status, McpStatus::NeedsAuth | McpStatus::Failed)) {
                if connected.iter().any(|key| key.names(server)) {
                    owed.insert((task.clone(), server.name.clone()));
                }
            }
        }
        self.tried.retain(|owed_to, _| owed.contains(owed_to));
        let (mut due, mut next) = (vec![], None::<Millis>);
        for owed_to in owed {
            let at = match self.tried.get(&owed_to) {
                None => now,
                Some((tries, _)) if *tries >= RECONNECTS => continue,
                Some((_, last)) => last + SIGN_IN_CACHE,
            };
            if at <= now {
                let tries = self.tried.get(&owed_to).map_or(0, |(tries, _)| *tries);
                self.tried.insert(owed_to.clone(), (tries + 1, now));
                if tries + 1 < RECONNECTS {
                    next = Some(next.map_or(now + SIGN_IN_CACHE, |next| next.min(now + SIGN_IN_CACHE)));
                }
                due.push(owed_to);
            } else {
                next = Some(next.map_or(at, |next| next.min(at)));
            }
        }
        (due, next)
    }
}

/// The servers each open task's node reported.
fn nodes(state: &DomainState) -> Vec<(TaskId, &[McpServerState])> {
    state.tasks.values().filter(|task| !task.over()).filter_map(|task| {
        let node = state.mcp.get(task.agent_id.as_ref()?)?;
        Some((task.id.clone(), node.servers.as_slice()))
    }).collect()
}

/// The open sign-ins that come back to Claude's listener, by the task whose node opened them.
fn returning(state: &DomainState) -> Vec<(TaskId, u16, String)> {
    state.mcp_waiting().into_iter().filter_map(|waiting| {
        let open = open_sign_in(state, &waiting.task, &waiting.server.name)?;
        Some((waiting.task, open.callback_port?, open.state.clone()?))
    }).collect()
}

pub(crate) async fn run(domain: Arc<DomainRuntime>) {
    let mut changes = domain.subscribe();
    let mut reconnects = Reconnects::default();
    let mut read: BTreeSet<(TaskId, String)> = BTreeSet::new();
    loop {
        let now = alondra_core::now_millis();
        let snapshot = domain.read();
        let state = snapshot.state();
        let open = returning(state);
        read.retain(|(task, flow)| open.iter().any(|(at, _, state)| at == task && state == flow));
        let mut ended = vec![];
        for (task, port, flow) in &open {
            if !read.contains(&(task.clone(), flow.clone())) && !listening(*port) {
                read.insert((task.clone(), flow.clone()));
                ended.push(task.clone());
            }
        }
        let (due, next) = reconnects.due(&nodes(state), now);
        drop(snapshot);
        for task in ended {
            if let Err(error) = system(&domain, Command::ReadMcpStatus { task_id: task.clone() }) {
                log!("could not ask task {task}'s Claude how its MCP servers are after a sign-in: {}", error.message);
            }
        }
        for (task, server) in due {
            match system(&domain, Command::ReconnectMcp { task_id: task.clone(), server: server.clone() }) {
                Ok(_) => log!("connecting {server} again for task {task}: another node signed in to it"),
                Err(error) if error.code == "no_task" || error.code == "task_not_active" => {}
                Err(error) => log!("could not connect {server} again for task {task}: {}", error.message),
            }
        }
        let watching = open.len() > read.len();
        let wake = [next.map(|at| Duration::from_millis(u64::try_from(at - now).unwrap_or(0))), watching.then_some(LISTENER)]
            .into_iter().flatten().min();
        match wake {
            Some(after) => tokio::select! {
                _ = changes.changed() => {}
                _ = tokio::time::sleep(after) => {}
            },
            None => {
                if changes.changed().await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "mcp_waits_tests.rs"]
mod tests;
