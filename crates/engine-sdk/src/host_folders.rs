//! The folders hosts ran in, under each provider's state root. A node's folder (`sdk-…`) is named
//! after the instance of an execution its task admitted; an orchestrator batch's (`o-…` for
//! Claude, `orchestrator-…` for Codex) is made for that one batch, which alone knows its name and
//! starts one host in it. A folder goes once nothing reads it again: no host holds its lock, no
//! task that goes on claims it, and nothing was written in it for `KEPT_FOR`, the longest window
//! `alondra report` reads. The folders hosts that ended leave beside a node's are `ended_hosts`'s.
use crate::domain_runtime::DomainRuntime;
use crate::sdk_backend::SdkBackend;
use crate::sdk_owner::SdkOwners as _;
use alondra_core::accounts::Provider;
use alondra_daemon_engine_model::NativeAccountIdentity;
use alondra_domain_rules::{AgentId, DomainState, LifecycleRecord};
use alondra_engine_ports::agent_host::{AgentHost, Retained};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 3_600);
const NODE: &str = "sdk-";
const BATCHES: [&str; 2] = ["o-", "orchestrator-"];

/// The folders of `folders`, each with when it was last written, that may go at `now`: a node's
/// that no task that goes on claims, and a batch's, once nothing was written in them for
/// `KEPT_FOR`.
pub fn spent(folders: &[(String, SystemTime)], claimed: impl Fn(&str) -> bool, now: SystemTime) -> Vec<String> {
    folders.iter()
        .filter(|(name, _)| !name.contains('.'))
        .filter(|(name, _)| if name.starts_with(NODE) { !claimed(name) } else { BATCHES.iter().any(|batch| name.starts_with(batch)) })
        .filter(|(_, written)| now.duration_since(*written).is_ok_and(|quiet| quiet >= KEPT_FOR))
        .map(|(name, _)| name.clone())
        .collect()
}

/// The node folders the tasks that are not done or cancelled claim: the node each runs on and
/// every execution each admitted, whose session a reopening or a takeover may resume.
pub fn claimed(state: &DomainState) -> BTreeSet<String> {
    let mut claimed = BTreeSet::new();
    for task in state.tasks.values().filter(|task| !task.state.terminal()) {
        claimed.extend(task.agent_id.iter().map(|agent| agent.0.clone()));
        for record in state.lifecycle_journal.get(&task.id).into_iter().flatten() {
            if let LifecycleRecord::Admitted { node, execution } = record {
                claimed.insert(node.0.clone());
                claimed.extend(execution.iter().map(|execution| execution.instance.clone()));
            }
        }
    }
    claimed
}

/// Deletes the folders `names` of `root` whose lock no host holds, and returns how many went. The
/// identity of the account a node's session ran on is kept apart first, where a reopening that
/// recovers the session reads it.
pub fn remove(hosts: &dyn AgentHost, root: &Path, provider: Provider, names: &[String]) -> usize {
    let mut removed = 0;
    for name in names {
        let folder = root.join(name);
        let claim = match hosts.claim(&folder) {
            Ok(Retained::Held) => continue,
            Ok(claim) => claim,
            Err(error) => {
                log!("could not tell whether a host still runs in {}: {error}", folder.display());
                continue;
            }
        };
        if name.starts_with(NODE)
            && let Err(failed) = NativeAccountIdentity::load_observed(&folder, provider).and_then(|identity| identity.map_or(Ok(()), |identity| identity.keep(&folder)))
        {
            log!("kept the folder of {name}: the identity of its account could not be kept apart: {failed}");
            continue;
        }
        match std::fs::remove_dir_all(&folder) {
            Ok(()) => removed += 1,
            Err(error) => log!("could not delete the folder of {name}: {error}"),
        }
        drop(claim);
    }
    removed
}

/// When each folder of `root` was last written: the folder itself or its host's outbox.
fn folders(root: &Path) -> std::io::Result<Vec<(String, SystemTime)>> {
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let written = [entry.path(), entry.path().join("outbox")].iter()
            .filter_map(|path| std::fs::symlink_metadata(path).and_then(|metadata| metadata.modified()).ok())
            .max();
        if let Some(written) = written {
            folders.push((entry.file_name().to_string_lossy().into_owned(), written));
        }
    }
    Ok(folders)
}

/// The state root of each provider, as the hosts of its nodes and batches use it.
pub(crate) fn roots(domain: &DomainRuntime, backend: &SdkBackend) -> Vec<(PathBuf, Provider)> {
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

/// Deletes every spent folder of the state roots at `now`, and returns how many went.
pub fn sweep(domain: &DomainRuntime, backend: &SdkBackend, now: SystemTime) -> usize {
    let Ok(state) = domain.state() else { return 0 };
    let Ok(hosts) = alondra_engine_ports::agent_host::of(domain.ports()) else { return 0 };
    let claimed = claimed(&state);
    let mut removed = 0;
    for (root, provider) in roots(domain, backend) {
        let found = match folders(&root) {
            Ok(found) => found,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                log!("could not read the hosts' folders in {}: {error}", root.display());
                continue;
            }
        };
        let spent = spent(&found, |name| claimed.contains(name) || domain.owns_sdk_host(&AgentId(name.to_owned())), now);
        removed += remove(&*hosts, &root, provider, &spent);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Directory;
    use alondra_engine_doubles::agent_host::InProcessHosts;
    use alondra_engine_ports::agent_host::{HostLaunch, VerifiedArtifact};
    use std::collections::BTreeMap;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    use std::time::UNIX_EPOCH;

    const DAY: Duration = Duration::from_secs(24 * 3_600);
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn day(number: u32) -> SystemTime {
        UNIX_EPOCH + DAY * number
    }

    fn private(path: &Path) -> PathBuf {
        std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        path.to_path_buf()
    }

    fn launched(hosts: &InProcessHosts, artifact: &Path, folder: &Path) -> Box<dyn alondra_engine_ports::agent_host::LaunchedHost> {
        let artifact = VerifiedArtifact::verify(artifact, artifact, EMPTY_SHA256, EMPTY_SHA256).unwrap();
        let launch = HostLaunch { artifact, state_dir: folder, environment: &BTreeMap::new(), confinement: None };
        hosts.launch(launch, &mut |_| Ok(b"bootstrap".to_vec())).unwrap()
    }

    // 21 November: 18,152 node folders and 1,899 batch folders, 1.6 GB, since only the folders
    // hosts that ended leave beside a node's were ever deleted.
    #[test]
    fn a_folder_goes_once_no_task_that_goes_on_claims_it_and_nothing_was_written_in_it_for_a_week() {
        let folders: Vec<(String, SystemTime)> = [
            ("sdk-open", day(1)), ("sdk-done", day(1)), ("sdk-done-lately", day(25)), ("sdk-unclaimed", day(1)),
            ("o-batch", day(1)), ("o-batch-lately", day(29)), ("orchestrator-batch", day(1)),
            ("sdk-done.ended-1", day(1)), ("native-identity", day(1)), ("native-threads", day(1)),
        ].into_iter().map(|(name, written)| (name.to_owned(), written)).collect();
        let mut gone = spent(&folders, |name| name == "sdk-open", day(30));
        gone.sort();
        assert_eq!(gone, ["o-batch", "orchestrator-batch", "sdk-done", "sdk-unclaimed"]);
    }

    #[test]
    fn a_task_that_goes_on_claims_its_node_and_every_execution_it_admitted_and_a_finished_one_none() {
        let mut state: DomainState = serde_json::from_value(serde_json::to_value(DomainState::new("epoch")).unwrap()).unwrap();
        for (id, agent, task_state) in [("t-open", "sdk-current", "running"), ("t-done", "sdk-finished", "done"), ("t-cancelled", "sdk-dropped", "cancelled")] {
            let task: alondra_domain_rules::Task = serde_json::from_value(serde_json::json!({
                "id": id, "request_id": "r", "agent_id": agent, "title": "T", "state": task_state, "revision": 1, "started": true,
                "review_prs": false, "final_presentation": null, "pr_id": null, "dependencies": [], "documents": {}, "archived": false,
            })).unwrap();
            state.tasks.insert(id.into(), task);
        }
        let admitted = |node: &str, instance: &str| LifecycleRecord::Admitted {
            node: AgentId(node.into()),
            execution: Some(alondra_domain_rules::ExecutionIdentity { conversation: "c".into(), instance: instance.into(), host: "alondra-sdk".into() }),
        };
        state.lifecycle_journal.insert("t-open".into(), vec![admitted("sdk-first", "sdk-first"), admitted("sdk-current", "sdk-moved")]);
        state.lifecycle_journal.insert("t-done".into(), vec![admitted("sdk-finished", "sdk-finished")]);
        assert_eq!(claimed(&state), BTreeSet::from(["sdk-current".into(), "sdk-first".into(), "sdk-moved".into()]));
    }

    #[test]
    fn a_folder_whose_host_still_runs_stays_and_a_nodes_account_identity_outlives_its_folder() {
        let home = Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let canonical = home.canonicalize().unwrap();
        let root = private(&canonical.join("sdk"));
        std::fs::write(canonical.join("host"), b"").unwrap();
        let hosts = InProcessHosts::default();
        let running = private(&root.join("sdk-running"));
        let _running = launched(&hosts, &canonical.join("host"), &running);
        let ended = private(&root.join("sdk-ended"));
        drop(launched(&hosts, &canonical.join("host"), &ended));
        hosts.served(&ended).unwrap().end(std::os::unix::process::ExitStatusExt::from_raw(0));
        let mut identity = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(ended.join("native-account.json")).unwrap();
        std::io::Write::write_all(&mut identity, br#"{"email":"ana@example.com","pay_per_use":false}"#).unwrap();
        private(&root.join("o-batch"));

        let names = ["sdk-running", "sdk-ended", "o-batch"].map(str::to_owned);
        assert_eq!(remove(&hosts, &root, Provider::Claude, &names), 2);

        assert!(running.exists(), "a host still runs in it");
        assert!(!ended.exists() && !root.join("o-batch").exists());
        let kept = NativeAccountIdentity::load_observed(&ended, Provider::Claude).unwrap();
        assert!(kept.is_some(), "a reopening still finds the account its session ran on");
    }
}
