//! The journal: what the host keeps in its folder before it sends it, which the daemon that takes
//! its execution over reads once the host ended.
use super::*;

#[test]
fn the_journal_an_ended_host_leaves_reads_back_with_every_event_it_sent() {
    let launched = launch("Write the release notes", None);
    let ended = launched.ended.as_ref().expect("the ended host's journal reads back");
    let journaled: std::collections::BTreeMap<u64, &[u8]> = ended.events().iter().map(|event| (event.cursor, event.payload.as_slice())).collect();
    assert!(!launched.sent.is_empty(), "the host sent the events of its turn");
    for (cursor, evidence) in &launched.sent {
        assert_eq!(journaled.get(cursor).copied(), Some(evidence.as_slice()), "the journal holds the event at cursor {cursor} as the host sent it");
    }
}
