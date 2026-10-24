use super::*;

const LINEAR: &str = "https://mcp.linear.app/mcp";

fn server(name: &str, status: McpStatus, url: &str) -> McpServerState {
    McpServerState { name: name.into(), status, error: None, source: None, transport: Some("http".into()), url: Some(url.into()), traits: Default::default() }
}

// One node signed in to Linear: every other node that reports it signed out connects it again at
// once and, as Claude may still read its old sign-in for 30 s, once that is over; never more than
// three times, and never a node whose Linear is another server of the same name.
#[test]
fn a_server_one_node_connected_is_connected_again_on_the_nodes_that_report_it_signed_out() {
    let connected = [server("linear", McpStatus::Connected, LINEAR)];
    let signed_out = [server("linear", McpStatus::NeedsAuth, LINEAR), server("files", McpStatus::NeedsAuth, "https://files.example.com")];
    let elsewhere = [server("linear", McpStatus::NeedsAuth, "https://linear.internal.example.com/mcp")];
    let servers_by_task = |second: &[McpServerState]| -> Vec<(TaskId, Vec<McpServerState>)> {
        vec![("t".into(), connected.to_vec()), ("u".into(), second.to_vec()), ("w".into(), elsewhere.to_vec())]
    };
    let mut reconnects = Reconnects::default();
    let mut due = |second: &[McpServerState], now: Millis| {
        let servers = servers_by_task(second);
        let nodes: Vec<(TaskId, &[McpServerState])> = servers.iter().map(|(task, servers)| (task.clone(), servers.as_slice())).collect();
        reconnects.due(&nodes, now)
    };
    let owed = vec![(TaskId::from("u"), "linear".to_owned())];
    assert_eq!(due(&signed_out, 1_000), (owed.clone(), Some(1_000 + SIGN_IN_CACHE)));
    assert_eq!(due(&signed_out, 1_000 + SIGN_IN_CACHE - 1), (vec![], Some(1_000 + SIGN_IN_CACHE)), "not inside Claude's cache");
    let again = 1_000 + SIGN_IN_CACHE;
    assert_eq!(due(&signed_out, again), (owed.clone(), Some(again + SIGN_IN_CACHE)));
    assert_eq!(due(&signed_out, again + SIGN_IN_CACHE), (owed, None), "the third and last");
    assert_eq!(due(&signed_out, again + 10 * SIGN_IN_CACHE), (vec![], None));
    assert_eq!(due(&connected, again + 11 * SIGN_IN_CACHE), (vec![], None));
    assert_eq!(due(&signed_out, again + 12 * SIGN_IN_CACHE).0.len(), 1, "a node signed out again starts over");
}

#[test]
fn nothing_is_owed_while_no_node_has_the_server_connected() {
    let signed_out = [server("linear", McpStatus::NeedsAuth, LINEAR)];
    let mut reconnects = Reconnects::default();
    assert_eq!(reconnects.due(&[("t".into(), &signed_out[..]), ("u".into(), &signed_out[..])], 1_000), (vec![], None));
}

fn due_after_sign_in(reconnects: &mut Reconnects, first: &[McpServerState], second: &[McpServerState], now: Millis) -> (Vec<(TaskId, String)>, Option<Millis>) {
    let elsewhere = [server("linear", McpStatus::NeedsAuth, "https://linear.internal.example.com/mcp")];
    reconnects.due(&[("t".into(), first), ("u".into(), second), ("w".into(), &elsewhere[..])], now)
}

// The user signed in to Linear on one node, but Claude connects it only after it has answered the
// browser, so that node still reports it signed out: it connects it again, as every other node
// that reports it signed out, at once and once Claude's cache is over, never more than three times.
#[test]
fn a_sign_in_that_ended_is_connected_again_on_every_node_that_reports_it_signed_out() {
    let signed_out = [server("linear", McpStatus::NeedsAuth, LINEAR)];
    let connected = [server("linear", McpStatus::Connected, LINEAR)];
    let mut reconnects = Reconnects::default();
    let mut due = |first: &[McpServerState], second: &[McpServerState], now: Millis| due_after_sign_in(&mut reconnects, first, second, now);
    assert_eq!(due(&signed_out, &signed_out, 1_000), (vec![], None), "nothing is owed before a sign-in ends");
    reconnects.signed_in(McpServerKey::of(&signed_out[0]));
    let mut due = |first: &[McpServerState], second: &[McpServerState], now: Millis| due_after_sign_in(&mut reconnects, first, second, now);
    let both = vec![(TaskId::from("t"), "linear".to_owned()), (TaskId::from("u"), "linear".to_owned())];
    assert_eq!(due(&signed_out, &signed_out, 2_000), (both.clone(), Some(2_000 + SIGN_IN_CACHE)));
    assert_eq!(due(&signed_out, &signed_out, 2_000 + SIGN_IN_CACHE - 1), (vec![], Some(2_000 + SIGN_IN_CACHE)), "not inside Claude's cache");
    let again = 2_000 + SIGN_IN_CACHE;
    assert_eq!(due(&signed_out, &signed_out, again), (both.clone(), Some(again + SIGN_IN_CACHE)));
    assert_eq!(due(&signed_out, &signed_out, again + SIGN_IN_CACHE), (both, None), "the third and last");
    assert_eq!(due(&signed_out, &signed_out, again + 10 * SIGN_IN_CACHE), (vec![], None));
    assert_eq!(due(&connected, &connected, again + 11 * SIGN_IN_CACHE), (vec![], None));
    assert_eq!(due(&signed_out, &signed_out, again + 12 * SIGN_IN_CACHE), (vec![], None), "a spent sign-in is not owed again");
    reconnects.signed_in(McpServerKey::of(&signed_out[0]));
    let owed = due_after_sign_in(&mut reconnects, &connected, &signed_out, again + 13 * SIGN_IN_CACHE).0;
    assert_eq!(owed, [(TaskId::from("u"), "linear".to_owned())], "a node that reports it connected is not owed");
}
