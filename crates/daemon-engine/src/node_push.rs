//! What alondrad writes down a node's open connection (`alondra node connect`): one JSON line per
//! message, which the node mod submits to its session and then acknowledges, and one per
//! interrupt, which the mod turns into an abort of the running turn. Outbox messages are found in
//! the domain store by the connection itself and stay pending until acknowledged; the messages
//! the user or the orchestrator sends directly have no outbox row, so they are kept here until
//! acknowledged and written again on the session's next connection.
use crate::locks::Poisonable as _;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};

pub(crate) const OUTBOX_PREFIX: &str = "o-";
const DIRECT_PREFIX: &str = "d-";

pub(crate) fn outbox_id(message: i64) -> String {
    format!("{OUTBOX_PREFIX}{message}")
}

/// Who sent a message to a node and when, ahead of its text: the node goes by the newest
/// instruction of whoever directs it.
pub fn stamped(from: &str, at: alondra_core::Millis, text: &str) -> String {
    format!("[From {from} · {}] {text}", crate::usage_pause::clock_text(at, alondra_core::now_millis(), false))
}

/// The author a node reads for a message that a command of this kind of actor wrote.
pub fn author_of(actor_kind: &str) -> String {
    match actor_kind {
        "user" => crate::domain_messages::user_name(),
        "orchestrator" => "the orchestrator".into(),
        _ => ALONDRA.into(),
    }
}

pub const ALONDRA: &str = "Alondra";

pub(crate) fn message_line(id: &str, text: &str, as_user: bool) -> String {
    serde_json::json!({"id": id, "text": text, "as_user": as_user}).to_string()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Unreachable;

struct Link {
    number: u64,
    lines: mpsc::UnboundedSender<String>,
}

struct Direct {
    line: String,
    acked: Option<oneshot::Sender<()>>,
}

pub struct Pushes {
    /// A holder that panics leaves it whole, so it is recovered: one insert, remove or send.
    links: Mutex<HashMap<String, Link>>,
    /// A holder that panics leaves it whole, so it is recovered: inserts and removes one by one; an
    /// entry left behind is only sent again.
    direct: Mutex<HashMap<String, BTreeMap<u64, Direct>>>,
    counter: AtomicU64,
}

/// Direct message ids start from the clock (a thousand per millisecond), not from zero: the mod remembers the ids it already
/// submitted for the life of its session, so a restarted daemon must never reuse one.
impl Default for Pushes {
    fn default() -> Self {
        Pushes {
            links: Default::default(),
            direct: Default::default(),
            counter: AtomicU64::new(alondra_core::now_millis().max(0) as u64 * 1000),
        }
    }
}

impl Pushes {
    /// Opens the session's channel as connection `number`, replacing an older one, and queues
    /// every direct message it has not acknowledged.
    pub fn attach(&self, session: &str, number: u64) -> mpsc::UnboundedReceiver<String> {
        let (lines, receiver) = mpsc::unbounded_channel();
        for waiting in self.direct.lock_or_recover("the direct pushes").get(session).into_iter().flat_map(BTreeMap::values) {
            let _ = lines.send(waiting.line.clone());
        }
        self.links.lock_or_recover("the push links").insert(session.to_owned(), Link { number, lines });
        receiver
    }

    pub(crate) fn detach(&self, session: &str, number: u64) {
        let mut links = self.links.lock_or_recover("the push links");
        if links.get(session).is_some_and(|link| link.number == number) {
            links.remove(session);
        }
    }

    pub(crate) fn connected(&self, session: &str) -> bool {
        self.links.lock_or_recover("the push links").contains_key(session)
    }

    fn write(&self, session: &str, line: String) -> Result<(), Unreachable> {
        let links = self.links.lock_or_recover("the push links");
        links.get(session).ok_or(Unreachable)?.lines.send(line).map_err(|_| Unreachable)
    }

    /// Writes a message to a connected session and keeps it until acknowledged. The receiver
    /// resolves once the session has acknowledged it.
    pub fn message(&self, session: &str, from: &str, text: &str, as_user: bool) -> Result<oneshot::Receiver<()>, Unreachable> {
        if !self.connected(session) {
            return Err(Unreachable);
        }
        let number = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let line = message_line(&format!("{DIRECT_PREFIX}{number}"), &stamped(from, alondra_core::now_millis(), text), as_user);
        let (acked, receiver) = oneshot::channel();
        self.direct.lock_or_recover("the direct pushes").entry(session.to_owned()).or_default()
            .insert(number, Direct { line: line.clone(), acked: Some(acked) });
        self.write(session, line)?;
        Ok(receiver)
    }

    /// Whether a direct message waits for the session to acknowledge it.
    pub(crate) fn waiting(&self, session: &str) -> bool {
        self.direct.lock_or_recover("the direct pushes").get(session).is_some_and(|waiting| !waiting.is_empty())
    }

    /// An interrupt is only meaningful to the turn running now, so it is never kept or resent.
    pub fn interrupt(&self, session: &str) -> Result<(), Unreachable> {
        self.write(session, serde_json::json!({"interrupt": true}).to_string())
    }

    /// Forgets the acknowledged direct messages and answers the ones waiting for it.
    pub fn acknowledged(&self, session: &str, ids: &[String]) {
        let mut direct = self.direct.lock_or_recover("the direct pushes");
        let Some(waiting) = direct.get_mut(session) else { return };
        for number in ids.iter().filter_map(|id| id.strip_prefix(DIRECT_PREFIX)?.parse::<u64>().ok()) {
            if let Some(Direct { acked: Some(acked), .. }) = waiting.remove(&number) {
                let _ = acked.send(());
            }
        }
    }
}

/// A node mod connected for a session, for tests of what is written down its connection.
#[cfg(any(test, feature = "test-hooks"))]
pub mod testing {
    use crate::domain_runtime::DomainRuntime;
    use serde_json::Value;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// Records every line written down its connection and acknowledges each message, the way the
    /// mod does once its session takes it, unless told to stay silent.
    pub struct Node {
        pub(crate) lines: Arc<Mutex<Vec<Value>>>,
        pub(crate) silent: Arc<AtomicBool>,
    }

    impl Node {
        pub fn connect(domain: &Arc<DomainRuntime>, session: &str) -> Node {
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

        /// What each message said, without the `[From … · …]` stamp in front of it.
        pub(crate) fn texts(&self) -> Vec<String> {
            self.stamped().into_iter().map(|(_, text)| text).collect()
        }

        /// Who each message says it comes from.
        pub(crate) fn authors(&self) -> Vec<String> {
            self.stamped().into_iter().map(|(author, _)| author).collect()
        }

        fn stamped(&self) -> Vec<(String, String)> {
            self.lines.lock().unwrap().iter().filter_map(|line| line["text"].as_str()).map(|text| {
                let (stamp, said) = text.strip_prefix("[From ").and_then(|rest| rest.split_once("] ")).expect("every message is stamped");
                (stamp.rsplit_once(" · ").expect("the stamp has a time").0.to_owned(), said.to_owned())
            }).collect()
        }

        pub fn interrupts(&self) -> usize {
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
        assert_eq!(pushes.message("s", "Alice", "hi", true).unwrap_err(), Unreachable, "nothing is kept for a session that is not connected");
        let mut first = pushes.attach("s", 1);
        let mut acked = pushes.message("s", "Alice", "hi", true).unwrap();
        let line: serde_json::Value = serde_json::from_str(&first.try_recv().unwrap()).unwrap();
        let text = line["text"].as_str().unwrap();
        assert!(text.starts_with("[From Alice · ") && text.ends_with("] hi"), "who sent it and when: {text}");
        assert_eq!(line["as_user"].as_bool(), Some(true));
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

    /// A node with «no toques nada» from the orchestrator ignored a later «sube tu trabajo» from
    /// Claude: nothing said which instruction was newer or whose it was.
    #[test]
    fn every_message_to_a_node_says_who_sent_it_and_when() {
        let at = alondra_core::now_millis();
        let clock = crate::usage_pause::clock_text(at, at, false);
        assert_eq!(stamped("Claude, for Alice", at, "Push your work"), format!("[From Claude, for Alice · {clock}] Push your work"));
        assert_eq!(author_of("orchestrator"), "the orchestrator");
        assert_eq!(author_of("system"), ALONDRA);
        assert_eq!(author_of("user"), crate::domain_messages::user_name());
    }

    #[test]
    fn a_restarted_daemon_never_reuses_a_direct_message_id() {
        let id_of = |pushes: &Pushes, session: &str| {
            let mut link = pushes.attach(session, 1);
            let _acked = pushes.message(session, ALONDRA, "hi", true).unwrap();
            serde_json::from_str::<serde_json::Value>(&link.try_recv().unwrap()).unwrap()["id"].as_str().unwrap().to_owned()
        };
        let before = Pushes::default();
        let first = id_of(&before, "s");
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_ne!(first, id_of(&Pushes::default(), "s"), "ids of two daemon runs differ");
        assert_ne!(first, id_of(&before, "t"), "and so do two messages of one run");
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
