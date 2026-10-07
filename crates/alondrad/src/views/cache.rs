use alondra_core::domain::DomainState;
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
    pub(crate) fn render(
        &self,
        key: &str,
        domain: &DomainState,
        live: &LiveSnapshot,
        client: Client,
        lang: Lang,
        transport: Transport,
    ) -> Result<View, BuildError> {
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
        let mut view = views::build(
            key,
            RenderContext {
                domain,
                projection: &domain.projection(),
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
    use base64::Engine as _;

    #[test]
    fn view_cursor_can_start_the_changes_feed() {
        let domain = DomainState::new("epoch");
        let live = LiveSnapshot::default();
        let view = Cache::default().render("home", &domain, &live,
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
        let first = cache.render(
            "home", &domain, &live, Client::Ios, Lang::En, Transport::Loopback,
        ).unwrap();
        let second = cache.render(
            "home", &domain, &live, Client::Ios, Lang::En, Transport::Loopback,
        ).unwrap();
        assert_eq!(first, second);
        assert_eq!(cache.builds.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
