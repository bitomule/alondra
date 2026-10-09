use std::{
    collections::HashMap,
    sync::{Arc, Mutex, mpsc::SyncSender},
    thread,
    time::Duration,
};

use super::Msg;
use crate::client::{Client, Incompatible, Target, pick_protocol};

/// Fetches TUI views and drives the long poll for the open screen and selected detail.
pub(super) fn start(
    tx: SyncSender<Msg>,
    open_views: Arc<Mutex<HashMap<String, String>>>,
    target: Target,
) {
    let _ = thread::Builder::new()
        .name("alondra-views-feed".into())
        .spawn(move || run(tx, open_views, &target));
}

/// The daemon's version and range reach the TUI from every answer that carries them; the client
/// then asks for the newest protocol both speak.
fn seen(tx: &SyncSender<Msg>, client: Client, service: Option<&alondra_core::views::Service>) -> Client {
    let Some(service) = service else {
        return client;
    };
    let _ = tx.send(Msg::Service(service.clone()));
    match pick_protocol(service.views_protocol) {
        Some(protocol) => client.with_protocol(protocol),
        None => client,
    }
}

fn refused(tx: &SyncSender<Msg>, client: Client, error: &anyhow::Error) -> Client {
    seen(tx, client, error.downcast_ref::<Incompatible>().and_then(|e| e.service.as_ref()))
}

fn run(tx: SyncSender<Msg>, open_views: Arc<Mutex<HashMap<String, String>>>, target: &Target) {
    let mut client = Client::for_target(target, Duration::from_secs(70));
    let mut cursor: Option<String> = None;
    loop {
        match client.views("home", None) {
            Ok(view) => {
                client = seen(&tx, client, view.service.as_ref());
                open_views
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .insert(view.key.clone(), view.version.clone());
                let _ = tx.send(Msg::ViewHome(Ok(view)));
                break;
            }
            Err(error) => {
                client = refused(&tx, client, &error);
                let _ = tx.send(Msg::ViewHome(Err(format!("{error:#}"))));
                thread::sleep(Duration::from_secs(2));
            }
        }
    }
    loop {
        let snapshot = open_views
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let owned: Vec<(String, String)> = snapshot
            .iter()
            .map(|(key, version)| (key.clone(), version.clone()))
            .collect();
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(key, version)| (key.as_str(), version.as_str()))
            .collect();
        match client.feed(cursor.as_deref(), &borrowed, 20) {
            Ok(feed) if feed.superseded => continue,
            Ok(feed) => {
                client = seen(&tx, client, feed.service.as_ref());
                cursor = Some(feed.cursor.clone());
                let stale = if feed.reset {
                    owned.iter().map(|(key, _)| key.clone()).collect()
                } else {
                    feed.stale.clone()
                };
                let _ = tx.send(Msg::ViewFeed(Ok(feed)));
                for key in stale {
                    let known = snapshot.get(&key).map(String::as_str);
                    match client.views(&key, known) {
                        Ok(view)
                            if matches!(key.as_str(), "home" | "tasks" | "fleet" | "schedules") =>
                        {
                            let _ = tx.send(Msg::ViewHome(Ok(view)));
                        }
                        Ok(view) => {
                            let _ = tx.send(Msg::ViewCard {
                                key,
                                result: Ok(view),
                            });
                        }
                        Err(error) => {
                            let _ = tx.send(Msg::ViewCard {
                                key,
                                result: Err(format!("{error:#}")),
                            });
                        }
                    }
                }
            }
            Err(error) => {
                client = refused(&tx, client, &error);
                let _ = tx.send(Msg::ViewFeed(Err(format!("{error:#}"))));
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}
