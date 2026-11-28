//! The waiting reason of a task the disk holds: who reserves the space it needs.
use super::*;

const GB: u64 = 1_000_000_000;

fn holder(task: &str, title: &str, bytes: u64) -> alondra_view_model::LiveDiskHolder {
    alondra_view_model::LiveDiskHolder { task_id: Some(task.into()), title: Some(title.into()), bytes }
}

/// "Echo" waits alone in the queue for the disk, while `holders` reserve the rest.
fn echo_waits(free: u64, needed: u64, holders: Vec<alondra_view_model::LiveDiskHolder>) -> (DomainState, LiveSnapshot) {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    (task.state, task.final_presentation, task.title) = (TaskState::NotStarted, None, "Echo".into());
    let hold = LiveHold::Disk { free, needed, holders };
    (state, pacing_live(vec![LiveQueued { task_id: "t-1".into(), position: 1, of: 1, hold, pausable: true, node: None }], vec![], None))
}

fn four_reserve() -> (DomainState, LiveSnapshot) {
    echo_waits(12 * GB, 18 * GB, vec![holder("t-a", "Alpha", 4 * GB), holder("t-b", "Beta", 3 * GB), holder("t-c", "Gamma", 2 * GB), holder("t-d", "Delta", GB)])
}

/// The waiting line of "Echo" on its row in Tasks and on its own screen, on every client, and nothing after it.
fn reads(state: &DomainState, live: &LiveSnapshot, lang: Lang, line: &str) {
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let (_, row) = active_row(state, live, client, lang, "t-1");
        let listed = match client {
            Client::Ios => row.children.iter().find(|child| child.id == "activity").map(|child| child.title.clone()),
            _ => row.subtitle.clone(),
        };
        assert_eq!(listed.as_deref(), Some(line), "{client:?} {lang:?} row in Tasks");
        let said = detail_state_text(state, live, client, lang);
        assert!(said.ends_with(line), "{client:?} {lang:?}: {said}");
    }
}

// spec: disk-gate.waiting-reason/line-names-three
#[test]
fn the_line_of_a_task_the_disk_holds_names_the_three_that_reserve_most() {
    let (state, live) = four_reserve();
    reads(&state, &live, Lang::En, "1st of 1 · Disk · 12.0 GB free of 18.0 GB needed · reserved by “Alpha” 4.0 GB, “Beta” 3.0 GB, “Gamma” 2.0 GB and 1 more");
}

// spec: disk-gate.waiting-reason/line-names-three-spanish
#[test]
fn the_line_of_a_task_the_disk_holds_names_the_three_that_reserve_most_in_spanish() {
    let (state, live) = four_reserve();
    reads(&state, &live, Lang::Es, "1.º de 1 · Disco · 12,0 GB libres de 18,0 GB necesarios · reservado por «Alpha» 4,0 GB, «Beta» 3,0 GB, «Gamma» 2,0 GB y 1 más");
}

// spec: disk-gate.waiting-reason/nothing-else-reserves
#[test]
fn with_nothing_else_reserving_the_line_names_no_task() {
    let (state, live) = echo_waits(4 * GB, 5 * GB, vec![]);
    reads(&state, &live, Lang::En, "1st of 1 · Disk · 4.0 GB free of 5.0 GB needed");
}

#[test]
fn fewer_than_four_holders_are_all_named_and_a_scheduled_run_by_what_it_is() {
    let run = alondra_view_model::LiveDiskHolder { task_id: None, title: None, bytes: 2 * GB };
    let (state, live) = echo_waits(9 * GB, 12 * GB, vec![holder("t-a", "Alpha", 4 * GB), run.clone()]);
    reads(&state, &live, Lang::En, "1st of 1 · Disk · 9.0 GB free of 12.0 GB needed · reserved by “Alpha” 4.0 GB and a scheduled run 2.0 GB");
    let (state, live) = echo_waits(9 * GB, 12 * GB, vec![holder("t-a", "Alpha", 4 * GB), holder("t-b", "Beta", 3 * GB), run]);
    reads(&state, &live, Lang::Es, "1.º de 1 · Disco · 9,0 GB libres de 12,0 GB necesarios · reservado por «Alpha» 4,0 GB, «Beta» 3,0 GB y una ejecución programada 2,0 GB");
}

#[test]
fn a_hold_of_an_earlier_daemon_without_holders_still_reads() {
    let earlier: LiveHold = serde_json::from_str(r#"{"kind":"disk","free":4000000000,"needed":5000000000}"#).unwrap();
    assert_eq!(earlier, LiveHold::Disk { free: 4 * GB, needed: 5 * GB, holders: vec![] });
    let sent = serde_json::to_value(LiveHold::Disk { free: 4 * GB, needed: 5 * GB, holders: vec![holder("t-a", "Alpha", GB)] }).unwrap();
    assert_eq!(sent["holders"][0], serde_json::json!({"task_id": "t-a", "title": "Alpha", "bytes": GB}));
}
