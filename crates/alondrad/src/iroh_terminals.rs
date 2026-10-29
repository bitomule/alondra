//! The iroh endpoints this Mac's devices said hello from, with the hello's signature. A terminal
//! paired directly has no account to bind its endpoint at the relay with, so the iroh relay would
//! not let it in; this Mac binds it there instead, carrying the device's own hello as its proof.
//! Also what devices need to dial this Mac's endpoint: its direct addresses and its iroh relay.
use crate::locks::Poisonable as _;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const FILE: &str = "iroh-terminals.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Hello {
    pub(crate) node: String,
    pub(crate) signature: String,
}

pub(crate) struct IrohTerminals {
    path: Option<PathBuf>,
    /// A holder that panics leaves it whole, so it is recovered: a retain and an insert, then a
    /// save whose failure memory already outlives.
    hellos: Mutex<BTreeMap<String, Hello>>,
    /// A holder that panics leaves it whole, so it is recovered: one assignment.
    dial: Mutex<Dial>,
}

/// How a device reaches this Mac's endpoint besides its ID.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Dial {
    pub(crate) addresses: Vec<String>,
    pub(crate) relay: Option<String>,
}

impl IrohTerminals {
    /// A missing or unreadable file means no endpoint is known until its device says hello again.
    pub(crate) fn open(home: &Path) -> Self {
        let path = home.join(FILE);
        let hellos = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self { path: Some(path), hellos: Mutex::new(hellos), dial: Mutex::default() }
    }

    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self { path: None, hellos: Mutex::default(), dial: Mutex::default() }
    }

    /// Notes the endpoint `device_id` proved itself from. True when its endpoint changed.
    pub(crate) fn record(&self, device_id: &str, hello: Hello) -> Result<bool> {
        let mut hellos = self.hellos.lock_or_recover("the iroh terminals");
        if hellos.get(device_id).is_some_and(|known| known.node == hello.node) {
            return Ok(false);
        }
        hellos.retain(|_, known| known.node != hello.node);
        hellos.insert(device_id.to_owned(), hello);
        self.save(&hellos)?;
        Ok(true)
    }

    /// The last hello of every device `keep` accepts; the others are forgotten.
    pub(crate) fn hellos(&self, keep: impl Fn(&str) -> bool) -> BTreeMap<String, Hello> {
        let mut hellos = self.hellos.lock_or_recover("the iroh terminals");
        let before = hellos.len();
        hellos.retain(|device_id, _| keep(device_id));
        if hellos.len() != before
            && let Err(error) = self.save(&hellos)
        {
            log!("iroh terminals not saved: {error:#}");
        }
        hellos.clone()
    }

    fn save(&self, hellos: &BTreeMap<String, Hello>) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec(hellos)?).context("write iroh terminals")?;
        std::fs::rename(&temporary, path).context("replace iroh terminals")?;
        Ok(())
    }

    pub(crate) fn dial(&self) -> Dial {
        self.dial.lock_or_recover("the iroh dial").clone()
    }

    pub(crate) fn set_dial(&self, change: impl FnOnce(&mut Dial)) {
        change(&mut self.dial.lock_or_recover("the iroh dial"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(node: &str) -> Hello {
        Hello { node: node.into(), signature: format!("signed-{node}") }
    }

    #[test]
    fn a_device_keeps_one_endpoint_and_an_endpoint_one_device_across_restarts() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let terminals = IrohTerminals::open(&home);
        assert!(terminals.record("client-a", hello("node-1")).unwrap());
        assert!(!terminals.record("client-a", hello("node-1")).unwrap());
        assert!(terminals.record("client-a", hello("node-2")).unwrap());
        assert!(terminals.record("client-b", hello("node-2")).unwrap());
        let reopened = IrohTerminals::open(&home);
        assert_eq!(reopened.hellos(|_| true), BTreeMap::from([("client-b".into(), hello("node-2"))]));
    }

    #[test]
    fn a_device_no_longer_kept_is_forgotten_on_disk_too() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let terminals = IrohTerminals::open(&home);
        terminals.record("client-a", hello("node-1")).unwrap();
        terminals.record("client-b", hello("node-2")).unwrap();
        assert_eq!(terminals.hellos(|id| id == "client-b").len(), 1);
        assert_eq!(IrohTerminals::open(&home).hellos(|_| true), BTreeMap::from([("client-b".into(), hello("node-2"))]));
    }
}
