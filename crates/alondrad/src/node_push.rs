//! What alondrad writes down a node's open connection (`alondra node connect`): one JSON line per
//! message, which the node mod submits to its session and then acknowledges, and one per
//! interrupt, which the mod turns into an abort of the running turn. Outbox messages are found in
//! the domain store by the connection itself and stay pending until acknowledged; the messages
//! the user or the orchestrator sends directly have no outbox row, so they are kept here until
//! acknowledged and written again on the session's next connection.
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};

pub(crate) const OUTBOX_PREFIX: &str = "o-";
const DIRECT_PREFIX: &str = "d-";

pub(crate) fn outbox_id(message: i64) -> String {
    format!("{OUTBOX_PREFIX}{message}")
}

pub(crate) fn message_line(id: &str, text: &str, as_user: bool) -> String {
    serde_json::json!({"id": id, "text": text, "as_user": as_user}).to_string()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Unreachable;

struct Link {
    number: u64,
    lines: mpsc::UnboundedSender<String>,
}

struct Direct {
    line: String,
    acked: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
pub(crate) struct Pushes {
    links: Mutex<HashMap<String, Link>>,
    direct: Mutex<HashMap<String, BTreeMap<u64, Direct>>>,
    counter: AtomicU64,
}

impl Pushes {
    /// Opens the session's channel as connection `number`, replacing an older one, and queues
    /// every direct message it has not acknowledged.
    pub(crate) fn attach(&self, session: &str, number: u64) -> mpsc::UnboundedReceiver<String> {
        let (lines, receiver) = mpsc::unbounded_channel();
        for waiting in self.direct.lock().expect("direct pushes poisoned").get(session).into_iter().flat_map(BTreeMap::values) {
            let _ = lines.send(waiting.line.clone());
        }
        self.links.lock().expect("push links poisoned").insert(session.to_owned(), Link { number, lines });
        receiver
    }

    pub(crate) fn detach(&self, session: &str, number: u64) {
        let mut links = self.links.lock().expect("push links poisoned");
        if links.get(session).is_some_and(|link| link.number == number) {
            links.remove(session);
        }
    }

    pub(crate) fn connected(&self, session: &str) -> bool {
        self.links.lock().expect("push links poisoned").contains_key(session)
    }

    fn write(&self, session: &str, line: String) -> Result<(), Unreachable> {
        let links = self.links.lock().expect("push links poisoned");
        links.get(session).ok_or(Unreachable)?.lines.send(line).map_err(|_| Unreachable)
    }

    /// Writes a message to a connected session and keeps it until acknowledged. The receiver
    /// resolves once the session has acknowledged it.
    pub(crate) fn message(&self, session: &str, text: &str, as_user: bool) -> Result<oneshot::Receiver<()>, Unreachable> {
        if !self.connected(session) {
            return Err(Unreachable);
        }
        let number = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let line = message_line(&format!("{DIRECT_PREFIX}{number}"), text, as_user);
        let (acked, receiver) = oneshot::channel();
        self.direct.lock().expect("direct pushes poisoned").entry(session.to_owned()).or_default()
            .insert(number, Direct { line: line.clone(), acked: Some(acked) });
        self.write(session, line)?;
        Ok(receiver)
    }

    /// An interrupt is only meaningful to the turn running now, so it is never kept or resent.
    pub(crate) fn interrupt(&self, session: &str) -> Result<(), Unreachable> {
        self.write(session, serde_json::json!({"interrupt": true}).to_string())
    }

    /// Forgets the acknowledged direct messages and answers the ones waiting for it.
    pub(crate) fn acknowledged(&self, session: &str, ids: &[String]) {
        let mut direct = self.direct.lock().expect("direct pushes poisoned");
        let Some(waiting) = direct.get_mut(session) else { return };
        for number in ids.iter().filter_map(|id| id.strip_prefix(DIRECT_PREFIX)?.parse::<u64>().ok()) {
            if let Some(Direct { acked: Some(acked), .. }) = waiting.remove(&number) {
                let _ = acked.send(());
            }
        }
    }
}

/// A node mod connected for a session, for tests of what is written down its connection.
#[cfg(test)]
pub(crate) mod testing {
    use crate::domain_runtime::DomainRuntime;
    use serde_json::Value;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// Records every line written down its connection and acknowledges each message, the way the
    /// mod does once its session takes it, unless told to stay silent.
    pub(crate) struct Node {
        pub(crate) lines: Arc<Mutex<Vec<Value>>>,
        pub(crate) silent: Arc<AtomicBool>,
    }

    impl Node {
        pub(crate) fn connect(domain: &Arc<DomainRuntime>, session: &str) -> Node {
            let node = Node { lines: Default::default(), silent: Default::default() };
            let mut receiver = domain.live.pushes.attach(session, 1);
            let (lines, silent, domain, session) = (node.lines.clone(), node.silent.clone(), domain.clone(), session.to_owned());
            tokio::spawn(async move {
                while let Some(line) = receiver.recv().await {
                    let line: Value = serde_json::from_str(&line).unwrap();
                    if let (Some(id), false) = (line["id"].as_str(), silent.load(Ordering::Relaxed)) {
                        domain.live.pushes.acknowledged(&session, &[id.to_owned()]);
                    }
                    lines.lock().unwrap().push(line);
                }
            });
            node
        }

        pub(crate) fn texts(&self) -> Vec<String> {
            self.lines.lock().unwrap().iter().filter_map(|line| line["text"].as_str().map(str::to_owned)).collect()
        }

        pub(crate) fn interrupts(&self) -> usize {
            self.lines.lock().unwrap().iter().filter(|line| line["interrupt"] == true).count()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_direct_message_waits_for_its_ack_and_is_written_again_on_the_next_connection() {
        let pushes = Pushes::default();
        assert_eq!(pushes.message("s", "hi", true).unwrap_err(), Unreachable, "nothing is kept for a session that is not connected");
        let mut first = pushes.attach("s", 1);
        let mut acked = pushes.message("s", "hi", true).unwrap();
        let line: serde_json::Value = serde_json::from_str(&first.try_recv().unwrap()).unwrap();
        assert_eq!((line["text"].as_str(), line["as_user"].as_bool()), (Some("hi"), Some(true)));
        assert!(acked.try_recv().is_err());

        pushes.detach("s", 1);
        assert!(!pushes.connected("s"));
        let mut second = pushes.attach("s", 2);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&second.try_recv().unwrap()).unwrap()["id"], line["id"], "unacknowledged: resent");

        pushes.acknowledged("s", &[line["id"].as_str().unwrap().to_owned()]);
        assert!(acked.try_recv().is_ok());
        let mut third = pushes.attach("s", 3);
        assert!(third.try_recv().is_err(), "acknowledged: not resent");
    }

    #[test]
    fn only_the_current_connection_detaches_and_an_interrupt_is_never_kept() {
        let pushes = Pushes::default();
        assert_eq!(pushes.interrupt("s"), Err(Unreachable));
        let _old = pushes.attach("s", 1);
        let mut current = pushes.attach("s", 2);
        pushes.detach("s", 1);
        assert!(pushes.connected("s"), "a replaced connection closing late changes nothing");
        pushes.interrupt("s").unwrap();
        assert_eq!(current.try_recv().unwrap(), r#"{"interrupt":true}"#);
        pushes.detach("s", 2);
        let mut next = pushes.attach("s", 3);
        assert!(next.try_recv().is_err());
    }
}
