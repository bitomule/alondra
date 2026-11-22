//! The folders hosts ran in, under each provider's state root. A node's folder (`sdk-…`) is named
//! after the instance of an execution its task admitted; an orchestrator batch's (`o-…` for
//! Claude, `orchestrator-…` for Codex) is made for that one batch, which deletes it when its host
//! ends. A node's folder goes on the event that leaves it unused: its host ends and no task that
//! goes on claims it, or the task that claimed it closes. It stays while its lock is held, while a
//! launch prepares it, while this daemon owns its host, and while a command of a task that claimed
//! it waits for a receipt only that host's journal can settle. When the daemon starts it deletes
//! what already meets those conditions. The folders hosts that ended leave beside a node's are
//! `ended_hosts`'s.
use crate::domain_runtime::DomainRuntime;
use crate::sdk_backend::SdkBackend;
use alondra_core::accounts::Provider;
use alondra_daemon_engine_model::NativeAccountIdentity;
use alondra_domain_rules::{AgentId, DomainState, LifecycleRecord, Task, TaskId};
use alondra_store::AuthorityScope;
use alondra_engine_ports::agent_host::{AgentHost, Preparing, Retained};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Whether this daemon owns the host of a node, which keeps its folder.
pub type Owned<'a> = &'a dyn Fn(&AgentId) -> bool;

const NODE: &str = "sdk-";
const BATCHES: [&str; 2] = ["o-", "orchestrator-"];

fn node(name: &str) -> bool {
    name.starts_with(NODE) && !name.contains('.')
}

fn batch(name: &str) -> bool {
    !name.contains('.') && BATCHES.iter().any(|batch| name.starts_with(batch))
}

/// What keeps a node's folder: a task that goes on claims it or this daemon owns its host, or a
/// task that claimed it still waits for a receipt from its host.
pub trait Claims {
    fn claimed(&self, name: &str) -> bool;
    fn owes_receipts(&self, name: &str) -> bool;
}

/// The node folders `task` claimed: the node it runs on and every execution it admitted.
pub fn folders_of(state: &DomainState, task: &Task) -> BTreeSet<String> {
    let mut folders: BTreeSet<String> = task.agent_id.iter().map(|agent| agent.0.clone()).collect();
    for record in state.lifecycle_journal.get(&task.id).into_iter().flatten() {
        if let LifecycleRecord::Admitted { node, execution } = record {
            folders.insert(node.0.clone());
            folders.extend(execution.iter().map(|execution| execution.instance.clone()));
        }
    }
    folders
}

/// The claims of the domain as `state` holds them: a task that is not done or cancelled claims
/// its folders, whose session a takeover or a reopening may resume; a task that just closed claims
/// none, whatever state its close left it in.
pub struct DomainClaims<'a> {
    open: BTreeSet<String>,
    claimants: BTreeMap<String, Vec<TaskId>>,
    owing: RefCell<BTreeMap<TaskId, bool>>,
    owned: Box<dyn Fn(&str) -> bool + 'a>,
    owes: Box<dyn Fn(&TaskId) -> bool + 'a>,
}

impl<'a> DomainClaims<'a> {
    pub fn new(state: &'a DomainState, closed: Option<&TaskId>, owned: impl Fn(&str) -> bool + 'a, owes: impl Fn(&TaskId) -> bool + 'a) -> Self {
        let mut open = BTreeSet::new();
        let mut claimants: BTreeMap<String, Vec<TaskId>> = BTreeMap::new();
        for task in state.tasks.values() {
            let folders = folders_of(state, task);
            if !task.state.terminal() && Some(&task.id) != closed {
                open.extend(folders.iter().cloned());
            }
            for folder in folders {
                claimants.entry(folder).or_default().push(task.id.clone());
            }
        }
        Self { open, claimants, owing: RefCell::default(), owned: Box::new(owned), owes: Box::new(owes) }
    }

    fn of(domain: &'a DomainRuntime, state: &'a DomainState, closed: Option<&TaskId>, owned: Owned<'a>) -> Self {
        Self::new(state, closed, move |name| owned(&AgentId(name.to_owned())), |task| owes_receipts(domain, task))
    }
}

impl Claims for DomainClaims<'_> {
    fn claimed(&self, name: &str) -> bool {
        self.open.contains(name) || (self.owned)(name)
    }
    fn owes_receipts(&self, name: &str) -> bool {
        self.claimants.get(name).into_iter().flatten().any(|task| *self.owing.borrow_mut().entry(task.clone()).or_insert_with(|| (self.owes)(task)))
    }
}

/// Whether a command of `task`'s execution still waits for a receipt that only the journal of its
/// host can settle: a dispatch or control claimed for the host with no answer recorded, or a
/// handoff left uncertain. A store that cannot answer counts as one that waits.
pub fn owes_receipts(domain: &DomainRuntime, task: &TaskId) -> bool {
    let Ok(state) = domain.state() else { return true };
    let Ok(scope) = AuthorityScope::new(state.data_epoch.clone()) else { return true };
    let owes = domain.own(scope, |owner| {
        if owner.lifecycle(task)?.execution().is_none() {
            return Ok(false);
        }
        let mut uncertain = false;
        owner.with_lifecycle(&mut |journal| {
            uncertain = !journal.receipt_candidates(task)?.is_empty();
            Ok(())
        })?;
        Ok(uncertain || owner.claims_left(task)?)
    });
    owes.map(|(owes, _)| owes).unwrap_or_else(|error| {
        log!("could not read whether a command of {task} waits for its host's receipt: {error}");
        true
    })
}

/// The folders hosts ran in, under each provider's state root, and the hosts that hold their locks.
pub struct Folders<'a> {
    pub hosts: &'a dyn AgentHost,
    pub roots: Vec<(PathBuf, Provider)>,
}

impl Folders<'_> {
    /// Deletes the folder `name` of `root` unless something still needs it, and says whether it
    /// went. The identity of the account a node's session ran on is kept apart first, where a
    /// reopening that recovers the session reads it.
    fn remove(&self, root: &Path, provider: Provider, name: &str, claims: &dyn Claims) -> bool {
        let folder = root.join(name);
        if !(node(name) || batch(name)) || Preparing::held(&folder) || !folder.is_dir()
            || node(name) && (claims.claimed(name) || claims.owes_receipts(name)) {
            return false;
        }
        let lock = match self.hosts.claim(&folder) {
            Ok(Retained::Held) => return false,
            Ok(lock) => lock,
            Err(error) => {
                log!("could not tell whether a host still runs in {}: {error}", folder.display());
                return false;
            }
        };
        if node(name)
            && let Err(failed) = NativeAccountIdentity::load_observed(&folder, provider).and_then(|identity| identity.map_or(Ok(()), |identity| identity.keep(&folder)))
        {
            log!("kept the folder of {name}: the identity of its account could not be kept apart: {failed}");
            return false;
        }
        let removed = std::fs::remove_dir_all(&folder).inspect_err(|error| log!("could not delete the folder of {name}: {error}")).is_ok();
        drop(lock);
        removed
    }

    /// A node's host that ran in `folder` ended.
    pub fn host_ended(&self, folder: &Path, claims: &dyn Claims) -> bool {
        let (Some(root), Some(name)) = (folder.parent(), folder.file_name().and_then(|name| name.to_str())) else { return false };
        self.roots.iter().find(|(known, _)| known == root).is_some_and(|(root, provider)| self.remove(root, *provider, name, claims))
    }

    /// The task that claimed the folders `names` closed. Returns how many went.
    pub fn task_closed(&self, names: &BTreeSet<String>, claims: &dyn Claims) -> usize {
        self.roots.iter().map(|(root, provider)| names.iter().filter(|name| self.remove(root, *provider, name, claims)).count()).sum()
    }

    /// Every folder of the state roots that nothing needs any more. Returns how many went.
    pub fn sweep(&self, claims: &dyn Claims) -> usize {
        let mut removed = 0;
        for (root, provider) in &self.roots {
            let names: Vec<String> = match std::fs::read_dir(root) {
                Ok(entries) => entries.filter_map(Result::ok).map(|entry| entry.file_name().to_string_lossy().into_owned()).collect(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    log!("could not read the hosts' folders in {}: {error}", root.display());
                    continue;
                }
            };
            removed += names.iter().filter(|name| self.remove(root, *provider, name, claims)).count();
        }
        removed
    }
}

/// The state root of each provider, as the hosts of its nodes and batches use it.
fn roots(domain: &DomainRuntime, backend: &SdkBackend) -> Vec<(PathBuf, Provider)> {
    let mut roots: Vec<(PathBuf, Provider)> = Vec::new();
    for provider in [Provider::Claude, Provider::Codex] {
        if let Ok(selected) = backend.for_provider(provider, domain.home())
            && !roots.iter().any(|(root, _)| *root == selected.state_root)
        {
            roots.push((selected.state_root, provider));
        }
    }
    roots
}

fn with_folders<T: Default>(domain: &DomainRuntime, closed: Option<&TaskId>, owned: Owned<'_>, run: impl FnOnce(&Folders, &DomainClaims, &DomainState) -> T) -> T {
    let (Some((backend, _)), Ok(state), Ok(hosts)) = (domain.sdk(), domain.state(), alondra_engine_ports::agent_host::of(domain.ports())) else {
        return T::default();
    };
    let folders = Folders { hosts: &*hosts, roots: roots(domain, backend) };
    run(&folders, &DomainClaims::of(domain, &state, closed, owned), &state)
}

/// When the daemon starts: deletes every folder that already meets the conditions.
pub fn sweep(domain: &DomainRuntime, owned: Owned<'_>) -> usize {
    with_folders(domain, None, owned, |folders, claims, _| folders.sweep(claims))
}

/// The host of a node that ran in `folder` ended.
pub fn host_ended(domain: &DomainRuntime, folder: &Path, owned: Owned<'_>) {
    if with_folders(domain, None, owned, |folders, claims, _| folders.host_ended(folder, claims)) {
        log!("deleted the folder of the host that ran in {}: nothing needs it any more", folder.display());
    }
}

/// `task` closed or was cancelled: the folders it claimed go.
pub fn task_closed(domain: &DomainRuntime, task: &TaskId, owned: Owned<'_>) {
    let removed = with_folders(domain, Some(task), owned, |folders, claims, state| {
        let names = state.tasks.get(task).map(|task| folders_of(state, task)).unwrap_or_default();
        folders.task_closed(&names, claims)
    });
    if removed > 0 {
        log!("deleted the folders of {removed} hosts of {task}, which closed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Directory;
    use alondra_engine_doubles::agent_host::InProcessHosts;
    use alondra_engine_ports::agent_host::{HostLaunch, LaunchedHost, VerifiedArtifact};
    use std::collections::BTreeMap;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    /// A state root under a test folder, with doubled hosts and no process behind them.
    struct Root {
        _home: Directory,
        path: PathBuf,
        artifact: PathBuf,
        hosts: InProcessHosts,
        live: Vec<Box<dyn LaunchedHost>>,
    }

    impl Root {
        fn new() -> Self {
            let home = Directory::new();
            std::fs::create_dir_all(&home).unwrap();
            let canonical = home.canonicalize().unwrap();
            let artifact = canonical.join("host");
            std::fs::write(&artifact, b"").unwrap();
            let path = canonical.join("sdk");
            std::fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self { _home: home, path, artifact, hosts: InProcessHosts::default(), live: vec![] }
        }

        fn folder(&self, name: &str) -> PathBuf {
            let folder = self.path.join(name);
            std::fs::DirBuilder::new().mode(0o700).create(&folder).unwrap();
            folder
        }

        fn launched(&mut self, name: &str) -> PathBuf {
            let folder = self.folder(name);
            let artifact = VerifiedArtifact::verify(&self.artifact, &self.artifact, EMPTY_SHA256, EMPTY_SHA256).unwrap();
            let launch = HostLaunch { artifact, state_dir: &folder, environment: &BTreeMap::new(), confinement: None };
            self.live.push(self.hosts.launch(launch, &mut |_| Ok(b"bootstrap".to_vec())).unwrap());
            folder
        }

        fn ended(&mut self, name: &str) -> PathBuf {
            let folder = self.launched(name);
            self.hosts.served(&folder).unwrap().end(std::os::unix::process::ExitStatusExt::from_raw(0));
            folder
        }

        fn folders(&self) -> Folders<'_> {
            Folders { hosts: &self.hosts, roots: vec![(self.path.clone(), Provider::Claude)] }
        }

        fn left(&self) -> BTreeSet<String> {
            std::fs::read_dir(&self.path).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect()
        }
    }

    #[derive(Default)]
    struct Facts {
        claimed: BTreeSet<String>,
        owing: BTreeSet<String>,
    }

    impl Claims for Facts {
        fn claimed(&self, name: &str) -> bool {
            self.claimed.contains(name)
        }
        fn owes_receipts(&self, name: &str) -> bool {
            self.owing.contains(name)
        }
    }

    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn keep_identity(folder: &Path) {
        let mut identity = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(folder.join("native-account.json")).unwrap();
        std::io::Write::write_all(&mut identity, br#"{"email":"ana@example.com","pay_per_use":false}"#).unwrap();
    }

    // 21 November: 18,152 node folders and 1,899 batch folders, 1.6 GB, since only the folders
    // hosts that ended leave beside a node's were ever deleted.
    #[test]
    fn a_nodes_folder_goes_when_its_host_ends_and_no_task_that_goes_on_claims_it() {
        let mut root = Root::new();
        let unclaimed = root.ended("sdk-unclaimed");
        keep_identity(&unclaimed);
        let resting = root.ended("sdk-resting");
        let facts = Facts { claimed: names(&["sdk-resting"]), ..Facts::default() };

        assert!(root.folders().host_ended(&unclaimed, &facts));
        assert!(!root.folders().host_ended(&resting, &facts), "an open task may resume the session it holds");

        assert_eq!(root.left(), names(&["native-identity", "sdk-resting"]));
        let kept = NativeAccountIdentity::load_observed(&unclaimed, Provider::Claude).unwrap();
        assert!(kept.is_some(), "a reopening still finds the account its session ran on");
    }

    #[test]
    fn the_folders_of_a_task_go_when_it_closes_unless_a_host_holds_one_or_a_task_that_goes_on_claims_it() {
        let mut state: DomainState = serde_json::from_value(serde_json::to_value(DomainState::new("epoch")).unwrap()).unwrap();
        for (id, agent, task_state) in [("t-closed", "sdk-last", "closing"), ("t-open", "sdk-shared", "running")] {
            let task: Task = serde_json::from_value(serde_json::json!({
                "id": id, "request_id": "r", "agent_id": agent, "title": "T", "state": task_state, "revision": 1, "started": true,
                "review_prs": false, "final_presentation": null, "pr_id": null, "dependencies": [], "documents": {}, "archived": false,
            })).unwrap();
            state.tasks.insert(id.into(), task);
        }
        let admitted = |node: &str, instance: &str| LifecycleRecord::Admitted {
            node: AgentId(node.into()),
            execution: Some(alondra_domain_rules::ExecutionIdentity { conversation: "c".into(), instance: instance.into(), host: "alondra-sdk".into() }),
        };
        state.lifecycle_journal.insert("t-closed".into(), vec![admitted("sdk-first", "sdk-first"), admitted("sdk-shared", "sdk-shared"), admitted("sdk-last", "sdk-last")]);
        let mut root = Root::new();
        for name in ["sdk-first", "sdk-shared", "sdk-other"] {
            root.ended(name);
        }
        root.launched("sdk-last");
        let closed = TaskId::from("t-closed");
        let claims = DomainClaims::new(&state, Some(&closed), |_| false, |_| false);

        let folders = folders_of(&state, &state.tasks[&closed]);
        assert_eq!(folders, names(&["sdk-first", "sdk-last", "sdk-shared"]));
        assert_eq!(root.folders().task_closed(&folders, &claims), 1);

        assert_eq!(root.left(), names(&["sdk-last", "sdk-other", "sdk-shared"]), "a host still runs in one and an open task claims another");
    }

    #[test]
    fn a_folder_whose_host_owes_a_receipt_stays_until_the_receipt_is_in_the_domain() {
        let mut root = Root::new();
        let folder = root.ended("sdk-owing");
        let owing = Facts { owing: names(&["sdk-owing"]), ..Facts::default() };

        assert!(!root.folders().host_ended(&folder, &owing), "only its outbox can settle a command no receipt answered");
        assert!(folder.exists());
        assert!(root.folders().host_ended(&folder, &Facts::default()));
        assert!(!folder.exists());
    }

    #[test]
    fn the_start_deletes_what_nothing_needs_and_never_a_folder_a_launch_prepares() {
        let mut root = Root::new();
        for name in ["sdk-open", "sdk-done", "o-batch", "orchestrator-batch"] {
            root.ended(name);
        }
        root.launched("sdk-live");
        root.launched("o-running");
        let preparing = Preparing::hold(&root.folder("sdk-launching"));
        root.folder("sdk-done.ended-1");
        root.folder("native-threads");
        let facts = Facts { claimed: names(&["sdk-open"]), ..Facts::default() };

        assert_eq!(root.folders().sweep(&facts), 3);

        assert_eq!(root.left(), names(&["native-threads", "o-running", "sdk-done.ended-1", "sdk-launching", "sdk-live", "sdk-open"]));
        drop(preparing);
    }
}
