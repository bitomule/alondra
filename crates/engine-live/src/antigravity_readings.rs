//! What the live view keeps of a node on Antigravity between the messages of its host.
use alondra_core::{Millis, ToolInFlight};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Readings {
    pub(crate) session: Option<String>,
    pub(crate) turn: Option<String>,
    pub(crate) answer: String,
    pub(crate) calls: BTreeMap<String, ToolInFlight>,
    pub(crate) since: Millis,
}

impl Readings {
    /// The host ended, and agy with it: no call or turn of it goes on.
    pub fn end(&mut self) {
        self.calls.clear();
        self.turn = None;
    }
}
