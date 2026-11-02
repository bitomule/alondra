//! The tasks that wait for an MCP server, from the daemon's side. A card stands for every task
//! that waits for one server: Connect opens one sign-in, on the node of the oldest that can, and
//! hands the user its page; where the browser came back to reaches that node's Claude in memory
//! (`mcp_sign_in`); Reconnect has the waiting nodes read their sign-in again. A sign-in ends when
//! its return reaches Claude or, while it comes back to Claude's own listener, when the listener
//! closes. Claude connects the server only after it has answered the browser, so a sign-in that
//! ended counts as a node reporting the server connected: every node that reports it signed out,
//! the one that signed in too, connects it again.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{
    Actor, Command, CommandEnvelope, DomainError, DomainState, InteractionId, MCP_SERVER_PREFIX, MCP_WAIT_PREFIX, McpServerKey, McpServerState,
    McpStatus, Millis, OpenSignIn, PROTOCOL_VERSION, TaskId,
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
    /// The user signs in at `url`; with a `port` the browser then comes back to
    /// `http://localhost:<port>/callback`, Claude's listener on the Mac, and on another device the
    /// client listens on that port of its own or the user pastes where it came back to.
    Browser { server: String, url: String, port: Option<u16> },
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
        Err(error) => Err(refusal(error)),
    }
}

fn refusal(error: crate::domain_store::StoreError) -> DomainError {
    match error {
        crate::domain_store::StoreError::Domain(error) => error,
        crate::domain_store::StoreError::Storage(error)
            if matches!(error.downcast_ref::<alondra_store::Error>(), Some(alondra_store::Error::UnsupportedCapability)) =>
        {
            DomainError::new("conflict", "This node's Claude cannot do it from Alondra: it runs an Agent SDK host from before it could")
        }
        crate::domain_store::StoreError::Storage(error) => DomainError::new("storage_unavailable", error.to_string()),
    }
}

fn allowed(state: &DomainState, command: Command) -> bool {
    state.guard(&Actor::System, &envelope(state, command)).is_ok()
}

/// What Connect, Reconnect and Sign out act on: the card the tasks that wait for a server wait
/// on, or a server the MCP servers screen names by its target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Card(InteractionId),
    Server(String),
}

impl Target {
    pub(crate) fn of(id: &str) -> Option<Self> {
        if id.starts_with(MCP_WAIT_PREFIX) {
            Some(Self::Card(InteractionId::from(id)))
        } else {
            id.starts_with(MCP_SERVER_PREFIX).then(|| Self::Server(id.to_owned()))
        }
    }
}

/// The server a target names and the open tasks whose nodes act for it, oldest first: for a
/// card, the tasks that still wait for it; for a server, every open task whose node reports it.
fn acting(state: &DomainState, target: &Target) -> Result<(McpServerKey, Vec<TaskId>), DomainError> {
    match target {
        Target::Card(card) => {
            let key = state.interactions.get(card).and_then(McpServerKey::of_card)
                .ok_or_else(|| DomainError::new("not_found", "This card does not wait for an MCP server"))?;
            let waiting: Vec<TaskId> = state.mcp_waiting_for(&key).into_iter().map(|waiting| waiting.task).collect();
            if waiting.is_empty() {
                return Err(DomainError::new("conflict", format!("No task waits for {} any more", key.name)));
            }
            Ok((key, waiting))
        }
        Target::Server(target) => {
            let nodes = state.mcp_nodes();
            let key = nodes.iter().flat_map(|node| &node.servers).map(McpServerKey::of).find(|key| key.target() == *target)
                .ok_or_else(|| DomainError::new("not_found", "No running task uses this MCP server any more"))?;
            let tasks = nodes.into_iter().filter(|node| node.servers.iter().any(|server| key.names(server))).map(|node| node.task).collect();
            Ok((key, tasks))
        }
    }
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
    Opened::Browser { server: server.into(), url: open.url.clone(), port: open.callback_port }
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
fn opened(state: &DomainState, target: &Target, task: &TaskId, server: &McpServerKey, since: u64) -> Option<Result<Opened, DomainError>> {
    let name = server.name.as_str();
    if matches!(target, Target::Card(_)) && !state.mcp_waiting_for(server).iter().any(|waiting| waiting.task == *task) {
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
pub(crate) async fn connect(domain: &DomainRuntime, target: &Target) -> Result<Opened, DomainError> {
    let mut name = String::new();
    for asked in [false, true] {
        let snapshot = domain.read();
        let state = snapshot.state();
        let (key, waiting) = acting(state, target)?;
        name.clone_from(&key.name);
        if matches!(target, Target::Server(_)) && waiting.iter().all(|task| state.mcp_server_of(task, &key).is_some_and(|server| server.status == McpStatus::Connected)) {
            return Ok(Opened::Connected { server: name });
        }
        for task in &waiting {
            if let Some(open) = open_sign_in(state, task, &name).filter(|open| still_open(open)) {
                return Ok(browser(&name, open));
            }
        }
        let start = |task: &TaskId| Command::StartMcpSignIn { task_id: task.clone(), server: name.clone() };
        if let Some(task) = waiting.iter().find(|task| allowed(state, start(task))).cloned() {
            drop(snapshot);
            let since = system(domain, start(&task))?;
            return answered(domain, |state| opened(state, target, &task, &key, since)).await.unwrap_or_else(|| {
                Err(DomainError::new("unavailable", format!("Claude did not open the sign-in to {name}: press Connect again")))
            });
        }
        let read = |task: &TaskId| Command::ReadMcpStatus { task_id: task.clone() };
        let unread: Vec<TaskId> = waiting.iter().cloned()
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
    Err(DomainError::new("conflict", match target {
        Target::Card(_) => format!("No node that waits for {name} can open its sign-in now: go on without it, or try again once one runs"),
        Target::Server(_) => format!("No running task can open the sign-in to {name} now: try again once one runs"),
    }))
}

/// Where the browser came back to after the user signed in on another device: it reaches the
/// node's Claude in memory, never written down, and the node then says how its servers are.
pub(crate) async fn give_back(domain: &Arc<DomainRuntime>, target: &Target, address: String) -> Result<(), DomainError> {
    let state = domain.read();
    let (key, waiting) = acting(state.state(), target)?;
    let name = key.name.clone();
    let task = waiting.iter()
        .find(|task| open_sign_in(state.state(), task, &name).is_some_and(|open| open.callback_port.is_some()))
        .cloned()
        .ok_or_else(|| DomainError::new("not_found", format!("No sign-in to {name} is open: press Connect again")))?;
    drop(state);
    let runtime = domain.clone();
    let (server, at) = (name.clone(), task);
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
    Ok(())
}

/// Reconnect: every acting node that can reads its sign-in to the server again.
pub(crate) fn reconnect(domain: &DomainRuntime, target: &Target) -> Result<usize, DomainError> {
    let (key, tasks) = acting(domain.read().state(), target)?;
    let reconnected = tasks.iter()
        .filter(|task| system(domain, Command::ReconnectMcp { task_id: (*task).clone(), server: key.name.clone() }).is_ok())
        .count();
    if reconnected == 0 {
        return Err(DomainError::new("conflict", format!("No node that uses {} runs now: try again once one does", key.name)));
    }
    Ok(reconnected)
}

/// Sign out: the first node that can has its Claude forget and revoke its sign-in to the server,
/// and every other node that uses it connects it again, so none keeps using a revoked sign-in.
pub(crate) fn sign_out(domain: &DomainRuntime, target: &Target) -> Result<String, DomainError> {
    let (key, tasks) = acting(domain.read().state(), target)?;
    let clear = |task: &TaskId| Command::ClearMcpSignIn { task_id: task.clone(), server: key.name.clone() };
    let Some(cleared) = tasks.iter().find(|task| allowed(domain.read().state(), clear(task))).cloned() else {
        return Err(DomainError::new("conflict", format!("No running task can sign out of {} now: try again once one runs", key.name)));
    };
    system(domain, clear(&cleared))?;
    for task in tasks.iter().filter(|task| **task != cleared) {
        if let Err(error) = system(domain, Command::ReconnectMcp { task_id: task.clone(), server: key.name.clone() }) {
            log!("could not connect {} again for task {task} after signing out: {}", key.name, error.message);
        }
    }
    Ok(key.name)
}

/// The reconnects owed to nodes that report signed out a server another node reports connected,
/// or whose sign-in ended: one at once, and again after Claude's cache of stored sign-ins is over,
/// at most `RECONNECTS`.
#[derive(Default)]
pub(crate) struct Reconnects {
    tried: HashMap<(TaskId, String), (u32, Millis)>,
    signed_in: BTreeSet<McpServerKey>,
}

impl Reconnects {
    /// A sign-in to `server` ended: it is owed as if a node reported it connected, until no node
    /// that reports it signed out has a reconnect left.
    pub(crate) fn signed_in(&mut self, server: McpServerKey) {
        self.signed_in.insert(server);
    }

    /// What is due now, from the servers each open task's node reported.
    pub(crate) fn due(&mut self, nodes: &[(TaskId, &[McpServerState])], now: Millis) -> (Vec<(TaskId, String)>, Option<Millis>) {
        let connected: BTreeSet<McpServerKey> = nodes.iter()
            .flat_map(|(_, servers)| servers.iter().filter(|server| server.status == McpStatus::Connected).map(McpServerKey::of))
            .chain(self.signed_in.iter().cloned())
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
        let tried = &self.tried;
        self.signed_in.retain(|key| nodes.iter().any(|(task, servers)| {
            servers.iter().any(|server| {
                matches!(server.status, McpStatus::NeedsAuth | McpStatus::Failed)
                    && key.names(server)
                    && tried.get(&(task.clone(), server.name.clone())).is_none_or(|(tries, _)| *tries < RECONNECTS)
            })
        }));
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

/// An open sign-in that comes back to Claude's listener: the task whose node opened it, the
/// server, and the flow Claude names it by.
type Returning = (TaskId, String, String);

/// The open sign-ins that come back to Claude's listener, with its port: those of a card's waiting
/// tasks and those the MCP servers screen opened alike.
fn returning(state: &DomainState) -> Vec<(Returning, u16)> {
    state.mcp_nodes().into_iter().flat_map(|node| {
        node.signing_in.into_iter().filter_map(|server| {
            let open = open_sign_in(state, &node.task, &server)?;
            Some(((node.task.clone(), server, open.state.clone()?), open.callback_port?))
        }).collect::<Vec<_>>()
    }).collect()
}

/// The server a node reported by `name`.
fn reported(state: &DomainState, task: &TaskId, name: &str) -> Option<McpServerKey> {
    let agent = state.tasks.get(task)?.agent_id.as_ref()?;
    state.mcp.get(agent)?.servers.iter().find(|server| server.name == name).map(McpServerKey::of)
}

pub(crate) async fn run(domain: Arc<DomainRuntime>) {
    let mut changes = domain.subscribe();
    let mut reconnects = Reconnects::default();
    let mut seen: BTreeSet<Returning> = BTreeSet::new();
    let mut ended: BTreeSet<Returning> = BTreeSet::new();
    loop {
        let now = alondra_core::now_millis();
        let snapshot = domain.read();
        let state = snapshot.state();
        let open = returning(state);
        let still: BTreeSet<Returning> = open.iter().map(|(sign_in, _)| sign_in.clone()).collect();
        let mut over: Vec<Returning> = seen.difference(&still).filter(|sign_in| !ended.contains(*sign_in)).cloned().collect();
        for (sign_in, port) in &open {
            if !ended.contains(sign_in) && !listening(*port) {
                ended.insert(sign_in.clone());
                over.push(sign_in.clone());
            }
        }
        ended.retain(|sign_in| still.contains(sign_in));
        seen = still;
        for (task, server, _) in over {
            if let Some(key) = reported(state, &task, &server) {
                reconnects.signed_in(key);
            }
        }
        let (due, next) = reconnects.due(&nodes(state), now);
        drop(snapshot);
        for (task, server) in due {
            match system(&domain, Command::ReconnectMcp { task_id: task.clone(), server: server.clone() }) {
                Ok(_) => log!("connecting {server} again for task {task}: someone signed in to it"),
                Err(error) if error.code == "no_task" || error.code == "task_not_active" => {}
                Err(error) => log!("could not connect {server} again for task {task}: {}", error.message),
            }
        }
        let watching = open.len() > ended.len();
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
