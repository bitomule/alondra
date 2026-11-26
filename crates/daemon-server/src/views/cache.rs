use crate::locks::Poisonable as _;
use alondra_domain_model::{NodeChangeCursor, ReadSnapshot};
use alondra_view_model::{
    BuildError, Client, ClientCapabilities, Lang, LiveSnapshot, RenderContext, Status, Transport, View,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

const MAX_ENTRIES: usize = 256;
/// A version weighs a few hundred bytes where its view weighs up to the response budget, so far
/// more of them stay: an unchanged view is answered from here long after its body left.
const MAX_VERSIONS: usize = 4096;

/// What a client that already holds a view needs to hear it did not change, and what the changes
/// feed compares: everything of a rendered view but its body.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Version {
    pub(crate) version: String,
    pub(crate) cursor: String,
    pub(crate) data_epoch: String,
    pub(crate) status: Status,
}

impl Version {
    fn of(view: &View) -> Self {
        Self {
            version: view.version.clone(),
            cursor: view.cursor.clone(),
            data_epoch: view.data_epoch.clone(),
            status: view.status.clone(),
        }
    }
}

/// The inputs a view is rendered from: the domain revision and the live data it was given.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Input {
    domain: NodeChangeCursor,
    cursor: String,
    live_revision: String,
    capabilities: ClientCapabilities,
}

impl Input {
    pub(crate) fn of(read: &ReadSnapshot, live: &LiveSnapshot) -> Self {
        let observation = super::observation::Observation::default();
        let live_hash = super::components::Components::new(live.clone(), &observation).digest(
            &observation, super::observation::Serialization::Digest,
        );
        Self::with_digest(read, &live_hash)
    }

    pub(crate) fn prepared(read: &ReadSnapshot, live: &super::live::Snapshot) -> Self {
        Self::with_digest(read, &live.digest())
    }

    fn with_digest(read: &ReadSnapshot, live_hash: &[u8]) -> Self {
        let domain = read.state();
        let clock = serde_json::to_vec(&(
            domain.data_epoch.clone(),
            domain.revision,
            live_hash,
        ))
        .expect("view clock serialization");
        Self {
            domain: read.node_cursor(),
            cursor: format!(
                "c1.{}",
                base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, clock,)
            ),
            live_revision: live_hash[..8].iter().map(|byte| format!("{byte:02x}")).collect(),
            capabilities: ClientCapabilities::default(),
        }
    }

    pub(crate) fn with_capabilities(mut self, capabilities: ClientCapabilities) -> Self {
        self.capabilities = capabilities;
        if !capabilities.is_empty() {
            let digest = Sha256::digest(format!("{}|{}", self.live_revision, capabilities.query()).as_bytes());
            self.live_revision = digest[..8].iter().map(|byte| format!("{byte:02x}")).collect();
        }
        self
    }

    /// The live data's digest, as the changes feed's cursor names it.
    pub(crate) fn live_revision(&self) -> &str {
        &self.live_revision
    }

    fn key(&self, key: &str, client: Client, lang: Lang, transport: Transport) -> String {
        format!("{key}|{client:?}|{lang:?}|{transport:?}|{}|{}", self.capabilities.query(), self.cursor)
    }
}

#[derive(Default)]
pub(crate) struct Cache {
    /// A holder that panics leaves it whole, so it is recovered: a cache of rendered views.
    entries: Mutex<Entries<View>>,
    /// A holder that panics leaves it whole, so it is recovered: a cache of versions.
    versions: Mutex<Entries<Version>>,
    #[cfg(test)]
    builds: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) build_threads: Mutex<Vec<std::thread::ThreadId>>,
}

struct Entries<V> {
    values: HashMap<String, (NodeChangeCursor, V)>,
    newest: VecDeque<String>,
}

impl<V> Default for Entries<V> {
    fn default() -> Self {
        Self { values: HashMap::new(), newest: VecDeque::new() }
    }
}

impl<V: Clone> Entries<V> {
    fn get(&mut self, key: &str, domain: &NodeChangeCursor) -> Option<V> {
        let (stored, value) = self.values.get(key)?;
        if stored != domain { return None; }
        let value = value.clone();
        self.newest.retain(|entry| entry != key);
        self.newest.push_back(key.to_owned());
        Some(value)
    }

    fn insert(&mut self, key: String, domain: NodeChangeCursor, value: V, max: usize) {
        if self.values.contains_key(&key) {
            self.newest.retain(|entry| entry != &key);
        }
        self.newest.push_back(key.clone());
        self.values.insert(key, (domain, value));
        while self.values.len() > max {
            if let Some(oldest) = self.newest.pop_front() {
                self.values.remove(&oldest);
            }
        }
    }
}

impl Cache {
    #[cfg(test)]
    pub(crate) fn build_count(&self) -> usize {
        self.builds.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Drops every view body and keeps their versions, as a long run of other views does.
    #[cfg(test)]
    pub(crate) fn forget_bodies(&self) {
        *self.entries.lock().unwrap() = Entries::default();
    }

    #[cfg(test)]
    pub(crate) fn serialized_retention(&self) -> (usize, usize) {
        let entries = self.entries.lock_or_recover("the view render cache");
        let bytes = entries.values.values().map(|(_, view)| serde_json::to_vec(view).expect("view serialization").len()).sum();
        (entries.values.len(), bytes)
    }

    /// The version of a view already rendered from these inputs, without rendering it.
    pub(crate) fn version(&self, key: &str, input: &Input, client: Client, lang: Lang, transport: Transport) -> Option<Version> {
        self.versions.lock_or_recover("the view version cache").get(&input.key(key, client, lang, transport), &input.domain)
    }

    /// The versions of these views, rendering only those never rendered from these inputs.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn versions(
        &self,
        keys: &[String],
        read: &ReadSnapshot,
        live: &LiveSnapshot,
        input: &Input,
        client: Client,
        lang: Lang,
        transport: Transport,
    ) -> HashMap<String, Result<Version, BuildError>> {
        keys.iter().map(|key| {
            let version = self.version(key, input, client, lang, transport).map_or_else(
                || self.render_from(key, read, live, input, client, lang, transport).map(|view| Version::of(&view)),
                Ok,
            );
            (key.clone(), version)
        }).collect()
    }

    pub(crate) fn render(
        &self,
        key: &str,
        read: &ReadSnapshot,
        live: &LiveSnapshot,
        client: Client,
        lang: Lang,
        transport: Transport,
    ) -> Result<View, BuildError> {
        self.render_from(key, read, live, &Input::of(read, live), client, lang, transport)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_from(
        &self,
        key: &str,
        read: &ReadSnapshot,
        live: &LiveSnapshot,
        input: &Input,
        client: Client,
        lang: Lang,
        transport: Transport,
    ) -> Result<View, BuildError> {
        let domain = read.state();
        let cursor = &input.cursor;
        let cache_key = input.key(key, client, lang, transport);
        if let Some(view) = self.entries.lock_or_recover("the view render cache").get(&cache_key, &read.node_cursor()) {
            return Ok(view);
        }
        #[cfg(test)]
        self.builds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(test)]
        self.build_threads.lock().unwrap().push(std::thread::current().id());
        let mut view = crate::screens::get().build(
            key,
            RenderContext {
                domain,
                index: read.index(),
                live,
                client,
                lang,
                transport,
                cursor,
                rendered_at: "",
            },
        )?;
        input.capabilities.adapt_view(&mut view, lang);
        // The view cursor is a valid feed cursor. Its status version is known only
        // after rendering, while the three-field clock above remains the cache key.
        let feed_clock = serde_json::to_vec(&(
            domain.data_epoch.0.clone(), domain.revision, &input.live_revision, view.status.version.clone(),
        )).expect("feed cursor serialization");
        view.cursor = format!("c1.{}", base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD, feed_clock,
        ));
        self.versions.lock_or_recover("the view version cache").insert(cache_key.clone(), read.node_cursor(), Version::of(&view), MAX_VERSIONS);
        self.entries.lock_or_recover("the view render cache").insert(cache_key, read.node_cursor(), view.clone(), MAX_ENTRIES);
        Ok(view)
    }
}

#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
mod tests {
    use super::*;
    use alondra_domain_model::DomainState;
    use base64::Engine as _;

    fn read(domain: &DomainState) -> ReadSnapshot {
        ReadSnapshot::build(std::sync::Arc::new(domain.clone()))
    }

    #[test]
    fn view_cursor_can_start_the_changes_feed() {
        let domain = DomainState::new("epoch");
        let live = LiveSnapshot::default();
        let view = Cache::default().render("home", &read(&domain), &live,
            Client::Ios, Lang::En, Transport::Loopback).unwrap();
        let raw = view.cursor.strip_prefix("c1.").unwrap();
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(raw).unwrap();
        let (epoch, revision, live_revision, status_version): (String, u64, String, String) =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(epoch, "epoch");
        assert_eq!(revision, domain.revision);
        assert_eq!(live_revision.len(), 16);
        assert_eq!(status_version, view.status.version);
    }

    #[test]
    fn clients_with_different_capabilities_never_share_body_version_or_cursor() {
        let cache = Cache::default();
        let domain = DomainState::new("epoch");
        let read = read(&domain);
        let live = LiveSnapshot::default();
        let legacy = Input::of(&read, &live);
        let modern = Input::of(&read, &live).with_capabilities(ClientCapabilities::parse("provider_accounts").unwrap());
        let first = cache.render_from("accounts", &read, &live, &legacy, Client::Tui, Lang::En, Transport::Loopback).unwrap();
        let second = cache.render_from("accounts", &read, &live, &modern, Client::Tui, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(first.version, second.version);
        assert_ne!(first.cursor, second.cursor);
        assert_eq!(cache.render_from("accounts", &read, &live, &legacy, Client::Tui, Lang::En, Transport::Loopback).unwrap(), first);
        assert_eq!(cache.render_from("accounts", &read, &live, &modern, Client::Tui, Lang::En, Transport::Loopback).unwrap(), second);
        assert_eq!(cache.build_count(), 2);
    }

    #[test]
    fn render_cache_hits_for_the_same_clock() {
        let cache = Cache::default();
        let domain = DomainState::new("epoch");
        let live = LiveSnapshot::default();
        let read = read(&domain);
        let first = cache.render("home", &read, &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        let second = cache.render("home", &read, &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_eq!(first, second);
        assert_eq!(cache.builds.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn a_reloaded_snapshot_rebuilds_bodies_and_versions_without_changing_wire_identity() {
        let cache = Cache::default();
        let domain = DomainState::new("epoch");
        let before = read(&domain);
        let after = read(&domain);
        let live = LiveSnapshot::default();
        let old = Input::of(&before, &live);
        let new = Input::of(&after, &live);
        let first = cache.render_from("home", &before, &live, &old, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_eq!(old.cursor, new.cursor, "snapshot identity stays out of the wire cursor");
        assert!(cache.version("home", &new, Client::Ios, Lang::En, Transport::Loopback).is_none());
        let second = cache.render_from("home", &after, &live, &new, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_eq!(cache.build_count(), 2);
        assert_eq!(first, second, "identical domain and live data still serialize identically");
        assert!(cache.version("home", &old, Client::Ios, Lang::En, Transport::Loopback).is_none());
        assert_eq!(cache.version("home", &new, Client::Ios, Lang::En, Transport::Loopback), Some(Version::of(&second)));
    }
    #[test]
    fn render_clock_separates_language_epoch_revision_and_live_inbox_times() {
        let cache = Cache::default();
        let mut domain = DomainState::new("epoch");
        let mut live = LiveSnapshot::default();
        let mut snapshot = read(&domain);
        let first = cache.render("home", &snapshot, &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        let spanish = cache.render("home", &snapshot, &live, Client::Ios, Lang::Es, Transport::Loopback).unwrap();
        assert_ne!(first.status.version, spanish.status.version);
        assert_ne!(first, spanish);
        assert_eq!(cache.render("home", &snapshot, &live, Client::Ios, Lang::En, Transport::Loopback).unwrap(), first);
        assert_eq!(cache.builds.load(std::sync::atomic::Ordering::Relaxed), 2);
        domain.revision += 1;
        snapshot = read(&domain);
        let revised = cache.render("home", &snapshot, &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(revised.cursor, first.cursor);
        domain.data_epoch = "restored-epoch".into();
        snapshot = read(&domain);
        let restored = cache.render("home", &snapshot, &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(restored.cursor, revised.cursor);
        live.inbox_times.push(alondra_view_model::InboxTime { id: "interaction:notice".into(), at: "2026-09-30T10:00:00Z".into() });
        let changed = cache.render("home", &snapshot, &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(changed.cursor, restored.cursor);
        assert_eq!(cache.builds.load(std::sync::atomic::Ordering::Relaxed), 5);
    }

    #[test]
    fn a_version_outlives_its_view_body() {
        let cache = Cache::default();
        let domain = DomainState::new("epoch");
        let read = read(&domain);
        let live = |n: usize| LiveSnapshot {
            inbox_times: vec![alondra_view_model::InboxTime { id: format!("interaction:{n}"), at: "2026-09-30T10:00:00Z".into() }],
            ..LiveSnapshot::default()
        };
        let first = cache.render("home", &read, &live(0), Client::Ios, Lang::En, Transport::Loopback).unwrap();
        for n in 1..=MAX_ENTRIES {
            cache.render("home", &read, &live(n), Client::Ios, Lang::En, Transport::Loopback).unwrap();
        }
        assert_eq!(cache.serialized_retention().0, MAX_ENTRIES, "the first body left");
        let input = Input::of(&read, &live(0));
        assert_eq!(cache.version("home", &input, Client::Ios, Lang::En, Transport::Loopback), Some(Version::of(&first)));
        assert_eq!(cache.version("home", &input, Client::Ios, Lang::Es, Transport::Loopback), None, "another language is another view");
        assert_eq!(cache.version("home", &Input::of(&read, &live(MAX_ENTRIES + 1)), Client::Ios, Lang::En, Transport::Loopback), None,
            "a view never rendered from these inputs has no version");
    }

}
