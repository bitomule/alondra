use alondra_core::domain::ReadSnapshot;
use alondra_core::views::{
    self, BuildError, Client, Lang, LiveSnapshot, RenderContext, Transport, View,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

const MAX_ENTRIES: usize = 256;

#[derive(Default)]
pub(crate) struct Cache {
    entries: Mutex<Entries>,
    #[cfg(test)]
    builds: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) build_threads: Mutex<Vec<std::thread::ThreadId>>,
}

#[derive(Default)]
struct Entries {
    values: HashMap<String, View>,
    newest: VecDeque<String>,
}

impl Entries {
    fn get(&mut self, key: &str) -> Option<View> {
        let view = self.values.get(key)?.clone();
        self.newest.retain(|entry| entry != key);
        self.newest.push_back(key.to_owned());
        Some(view)
    }

    fn insert(&mut self, key: String, view: View) {
        if self.values.contains_key(&key) {
            self.newest.retain(|entry| entry != &key);
        }
        self.newest.push_back(key.clone());
        self.values.insert(key, view);
        while self.values.len() > MAX_ENTRIES {
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

    #[cfg(test)]
    pub(crate) fn serialized_retention(&self) -> (usize, usize) {
        let entries = self.entries.lock().expect("view render cache poisoned");
        let bytes = entries.values.values().map(|view| serde_json::to_vec(view).expect("view serialization").len()).sum();
        (entries.values.len(), bytes)
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
        let domain = read.state();
        let live_hash =
            Sha256::digest(serde_json::to_vec(live).expect("live snapshot serialization"));
        let clock = serde_json::to_vec(&(
            domain.data_epoch.clone(),
            domain.revision,
            live_hash.as_slice(),
        ))
        .expect("view clock serialization");
        let cursor = format!(
            "c1.{}",
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, clock,)
        );
        let cache_key = format!("{key}|{client:?}|{lang:?}|{transport:?}|{cursor}");
        if let Some(view) = self.entries.lock().expect("view render cache poisoned").get(&cache_key) {
            return Ok(view);
        }
        #[cfg(test)]
        self.builds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(test)]
        self.build_threads.lock().unwrap().push(std::thread::current().id());
        let mut view = views::build(
            key,
            RenderContext {
                domain,
                index: read.index(),
                live,
                client,
                lang,
                transport,
                cursor: &cursor,
                rendered_at: "",
            },
        )?;
        // The view cursor is a valid feed cursor. Its status version is known only
        // after rendering, while the three-field clock above remains the cache key.
        let live_revision = live_hash[..8].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let feed_clock = serde_json::to_vec(&(
            domain.data_epoch.0.clone(), domain.revision, live_revision, view.status.version.clone(),
        )).expect("feed cursor serialization");
        view.cursor = format!("c1.{}", base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD, feed_clock,
        ));
        self.entries.lock().expect("view render cache poisoned").insert(cache_key, view.clone());
        Ok(view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::DomainState;
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
    fn render_cache_hits_for_the_same_clock() {
        let cache = Cache::default();
        let domain = DomainState::new("epoch");
        let live = LiveSnapshot::default();
        let first = cache.render("home", &read(&domain), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        let second = cache.render("home", &read(&domain), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_eq!(first, second);
        assert_eq!(cache.builds.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
    #[test]
    fn render_clock_separates_language_epoch_revision_and_live_inbox_times() {
        let cache = Cache::default();
        let mut domain = DomainState::new("epoch");
        let mut live = LiveSnapshot::default();
        let first = cache.render("home", &read(&domain), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        let spanish = cache.render("home", &read(&domain), &live, Client::Ios, Lang::Es, Transport::Loopback).unwrap();
        assert_ne!(first.status.version, spanish.status.version);
        assert_ne!(first, spanish);
        assert_eq!(cache.render("home", &read(&domain), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap(), first);
        assert_eq!(cache.builds.load(std::sync::atomic::Ordering::Relaxed), 2);
        domain.revision += 1;
        let revised = cache.render("home", &read(&domain), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(revised.cursor, first.cursor);
        domain.data_epoch = "restored-epoch".into();
        let restored = cache.render("home", &read(&domain), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(restored.cursor, revised.cursor);
        live.inbox_times.push(views::InboxTime { id: "interaction:notice".into(), at: "2026-09-30T10:00:00Z".into() });
        let changed = cache.render("home", &read(&domain), &live, Client::Ios, Lang::En, Transport::Loopback).unwrap();
        assert_ne!(changed.cursor, restored.cursor);
        assert_eq!(cache.builds.load(std::sync::atomic::Ordering::Relaxed), 5);
    }

}
