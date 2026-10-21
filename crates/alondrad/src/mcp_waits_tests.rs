use super::*;

const LINEAR: &str = "https://mcp.linear.app/mcp";

fn server(name: &str, status: McpStatus, url: &str) -> McpServerState {
    McpServerState { name: name.into(), status, error: None, source: None, transport: Some("http".into()), url: Some(url.into()) }
}

// One node signed in to Linear: every other node that reports it signed out connects it again at
// once and, as Claude may still read its old sign-in for 30 s, once that is over; never more than
// three times, and never a node whose Linear is another server of the same name.
#[test]
fn a_server_one_node_connected_is_connected_again_on_the_nodes_that_report_it_signed_out() {
    let connected = [server("linear", McpStatus::Connected, LINEAR)];
    let signed_out = [server("linear", McpStatus::NeedsAuth, LINEAR), server("files", McpStatus::NeedsAuth, "https://files.example.com")];
    let elsewhere = [server("linear", McpStatus::NeedsAuth, "https://linear.internal.example.com/mcp")];
    let fleet = |second: &[McpServerState]| -> Vec<(TaskId, Vec<McpServerState>)> {
        vec![("t".into(), connected.to_vec()), ("u".into(), second.to_vec()), ("w".into(), elsewhere.to_vec())]
    };
    let mut reconnects = Reconnects::default();
    let mut due = |second: &[McpServerState], now: Millis| {
        let fleet = fleet(second);
        let nodes: Vec<(TaskId, &[McpServerState])> = fleet.iter().map(|(task, servers)| (task.clone(), servers.as_slice())).collect();
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
