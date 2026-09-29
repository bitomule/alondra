//! What clients show that the domain does not keep: when a task last changed state, when a
//! request came in, when an inbox item first appeared, and the pull requests the user set
//! aside. A small file beside the database, written whole and renamed into place.
use alondra_core::domain_v2::{DomainState, TaskState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

const FILE: &str = "client-runtime.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct TaskStamp {
    pub state: Option<TaskState>,
    pub since: Option<i64>,
}

/// A time of `None` is a change that happened before the daemon started keeping times.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Data {
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskStamp>,
    #[serde(default)]
    pub requests: BTreeMap<String, Option<i64>>,
    #[serde(default)]
    pub inbox: BTreeMap<String, Option<i64>>,
    #[serde(default)]
    pub set_aside: BTreeSet<String>,
    #[serde(default)]
    pub baselined: bool,
}

pub(crate) struct Sidecar {
    path: PathBuf,
    data: Mutex<Data>,
}

impl Sidecar {
    pub(crate) fn open(home: &std::path::Path) -> Self {
        let path = home.join(FILE);
        let stored = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Self { path, data: Mutex::new(stored.unwrap_or_default()) }
    }

    pub(crate) fn snapshot(&self) -> Data {
        self.data.lock().expect("client sidecar poisoned").clone()
    }

    pub(crate) fn set_aside(&self, pr_id: &str, aside: bool) -> anyhow::Result<()> {
        let mut data = self.data.lock().expect("client sidecar poisoned");
        let changed = if aside { data.set_aside.insert(pr_id.to_owned()) } else { data.set_aside.remove(pr_id) };
        if changed {
            self.write(&data)?;
        }
        Ok(())
    }

    /// Records the times of what changed since the last look, and forgets what is gone.
    pub(crate) fn stamp(&self, state: &DomainState, inbox: &[String], now: i64) -> anyhow::Result<()> {
        let mut data = self.data.lock().expect("client sidecar poisoned");
        let before = data.clone();
        let seen = if data.baselined { Some(now) } else { None };
        data.baselined = true;
        for (id, task) in &state.tasks {
            let stamp = data.tasks.entry(id.0.clone()).or_insert_with(|| TaskStamp { state: Some(task.state), since: seen });
            if stamp.state != Some(task.state) {
                *stamp = TaskStamp { state: Some(task.state), since: Some(now) };
            }
        }
        data.tasks.retain(|id, _| state.tasks.keys().any(|task| &task.0 == id));
        for id in state.requests.keys() {
            data.requests.entry(id.0.clone()).or_insert(seen);
        }
        data.requests.retain(|id, _| state.requests.keys().any(|request| &request.0 == id));
        for id in inbox {
            data.inbox.entry(id.clone()).or_insert(seen);
        }
        data.inbox.retain(|id, _| inbox.contains(id));
        data.set_aside.retain(|id| state.pull_requests.keys().any(|pr| &pr.0 == id));
        if *data != before {
            self.write(&data)?;
        }
        Ok(())
    }

    fn write(&self, data: &Data) -> anyhow::Result<()> {
        let next = self.path.with_extension("json.next");
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&next)?;
        file.write_all(&serde_json::to_vec(data)?)?;
        file.sync_all()?;
        std::fs::rename(&next, &self.path)?;
        Ok(())
    }
}
