//! What the engine knows of the Claude API from its nodes' turns, and the text that sends a node
//! back to work after an API error cut its turn.
use crate::admission::{Paused, Reason};
use alondra_core::Millis;
use alondra_domain_model::{ApiHealth, OUTAGE_PROBE};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Failures of several nodes this close together, with no answer since, mean the API is down.
const OUTAGE_WINDOW: Millis = 10 * 60_000;


/// When the model last answered each node, and when an API error last cut each one's turn.
#[derive(Default)]
pub struct ApiWatch {
    /// A holder that panics leaves it whole, so it is recovered: health hints, each change an
    /// insert and a remove that the next answer or failure writes again.
    inner: Mutex<Watch>,
}

#[derive(Default)]
struct Watch {
    answered: BTreeMap<String, Millis>,
    failed: BTreeMap<String, Millis>,
    probed: Option<Millis>,
}

impl ApiWatch {
    fn lock(&self) -> std::sync::MutexGuard<'_, Watch> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn answered(&self, session: &str, at: Millis) {
        let mut watch = self.lock();
        watch.answered.insert(session.to_owned(), at);
        watch.failed.remove(session);
    }

    /// A node waiting for the API was sent back to try it.
    pub fn probed(&self, at: Millis) {
        self.lock().probed = Some(at);
    }

    /// The model answered some node after `at`.
    pub fn answered_since(&self, at: Millis) -> bool {
        self.lock().answered.values().any(|answered| *answered > at)
    }

    /// What is known of the API for `session`, failing since `first_at`.
    pub fn health(&self, session: &str, first_at: Millis, now: Millis) -> ApiHealth {
        let watch = self.lock();
        if watch.answered.iter().any(|(other, at)| other != session && *at > first_at) {
            return ApiHealth::Proven;
        }
        let recent: Vec<Millis> = watch.failed.values().copied().filter(|at| now - at <= OUTAGE_WINDOW).collect();
        let earliest = recent.iter().min().copied().unwrap_or(now);
        match recent.len() >= 2 && !watch.answered.values().any(|at| *at > earliest) {
            true => ApiHealth::Down,
            false => ApiHealth::Unknown,
        }
    }

    pub fn down(&self, now: Millis) -> bool {
        self.health("", now, now) == ApiHealth::Down
    }
}

/// While the API looks down, the one node waiting for it that tries it next: the first whose
/// probe is due. The others wait for an answer.
pub fn outage_probe(paused: &BTreeMap<String, Paused>, watch: &ApiWatch, now: Millis) -> Option<String> {
    if watch.lock().probed.is_some_and(|at| now - at < OUTAGE_PROBE) {
        return None;
    }
    paused.iter()
        .filter_map(|(session, paused)| match &paused.reason {
            Reason::ApiError(retry) if retry.outage && now >= retry.at && !watch.answered_since(paused.since) => Some((retry.at, session)),
            _ => None,
        })
        .min()
        .map(|(_, session)| session.clone())
}

pub fn resume_text(error: &str, detail: Option<&str>) -> String {
    let what = detail.map(str::trim).filter(|detail| !detail.is_empty()).unwrap_or(error);
    format!("Your previous turn was cut by a Claude API error ({what}), not by anything you did. Carry on where you left off: check the state of your branch and of the work in progress, then continue your task.")
}

