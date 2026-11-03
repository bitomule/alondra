//! Whether a Claude session still runs work of its own. Claude Code starts every Bash call,
//! foreground or background, in a process group of its own, a direct child of the session's
//! process; its MCP servers stay in the session's group, and the node mod's `alondra node connect`
//! is another child of it. A build can stay silent for an hour and still be work; a call that is
//! gone is not. The table comes from libproc and sysctl, never from the text of `ps`.
pub use alondra_process_group::Entry as Process;

pub fn snapshot() -> Vec<Process> {
    alondra_process_group::table().unwrap_or_default()
}

/// The direct children of the session whose mod connection is process `mod_pid` that are in a
/// group other than the session's, the mod left out. None when the mod or its session is not in
/// the table: nothing is known.
fn calls(table: &[Process], mod_pid: i32) -> Option<Vec<i32>> {
    let session = table.iter().find(|process| process.pid == mod_pid)?.parent;
    let group = table.iter().find(|process| process.pid == session)?.group;
    Some(table.iter()
        .filter(|process| process.parent == session && process.pid != mod_pid && process.group != group)
        .map(|process| process.pid).collect())
}

/// Whether the session whose mod connection is process `mod_pid` has a call running.
pub fn work_running(table: &[Process], mod_pid: u32) -> Option<bool> {
    Some(!calls(table, i32::try_from(mod_pid).ok()?)?.is_empty())
}

/// Every process the session's calls started, the calls included, deepest first: what a paused
/// node still runs after its turn was interrupted. The session, its mod connection and its MCP
/// servers are left alone.
pub fn work_descendants(table: &[Process], mod_pid: u32) -> Vec<u32> {
    let Some(mut found) = i32::try_from(mod_pid).ok().and_then(|pid| calls(table, pid)) else { return vec![] };
    let mut index = 0;
    while index < found.len() {
        let parent = found[index];
        let children: Vec<i32> = table.iter().filter(|process| process.parent == parent && !found.contains(&process.pid)).map(|process| process.pid).collect();
        found.extend(children);
        index += 1;
    }
    found.reverse();
    found.into_iter().filter_map(|pid| u32::try_from(pid).ok()).collect()
}

pub fn signal(pids: &[u32], signal: libc::c_int) {
    for pid in pids {
        unsafe { libc::kill(*pid as libc::pid_t, signal) };
    }
}

/// Whether the session whose mod connection is process `mod_pid` still runs: the mod's parent is
/// the session, and a mod whose session died is left to launchd. None when the mod's process is
/// not in the table.
pub fn session_running(table: &[Process], mod_pid: u32) -> Option<bool> {
    let mod_pid = i32::try_from(mod_pid).ok()?;
    let session = table.iter().find(|process| process.pid == mod_pid)?.parent;
    Some(session != 1 && table.iter().any(|process| process.pid == session))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: i32, parent: i32, group: i32, argv: &[&str]) -> Process {
        Process { pid, parent, group, argv: argv.iter().map(|arg| arg.to_string()).collect() }
    }

    /// Two sessions as measured on a Mac: their MCP servers in the session's group, whatever
    /// their arguments, and a Bash call in a group of its own.
    fn table() -> Vec<Process> {
        vec![
            row(27019, 1, 27000, &["claude", "--bg"]),
            row(27045, 27019, 27000, &["npm exec @upstash/context7-mcp@latest   "]),
            row(27046, 27045, 27000, &["sh", "-c", "context7-mcp"]),
            row(27047, 27019, 27000, &["sh", "-lc", "server"]),
            row(33881, 27019, 27000, &["/opt/homebrew/opt/alondra/bin/alondra", "node", "connect"]),
            row(84509, 1, 84500, &["claude", "--bg"]),
            row(27050, 84509, 84500, &["/opt/homebrew/opt/alondra/bin/alondra", "mcp-node"]),
            row(33879, 84509, 84500, &["/opt/homebrew/opt/alondra/bin/alondra", "node", "connect"]),
            row(65945, 27019, 65945, &["/bin/zsh", "-c", "-l", "eval 'bazel test //...'"]),
            row(65950, 65945, 65945, &["bazel", "test", "//..."]),
            row(65951, 65950, 65945, &["rustc", "--crate-name", "alondrad"]),
        ]
    }

    #[test]
    fn a_call_in_a_group_of_its_own_under_the_session_means_it_runs_something() {
        assert_eq!(work_running(&table(), 33881), Some(true));
    }

    #[test]
    fn what_a_paused_node_still_runs_is_its_calls_and_their_children_never_the_session_its_mod_or_its_mcp_servers() {
        assert_eq!(work_descendants(&table(), 33881), [65951, 65950, 65945], "deepest first");
        assert!(work_descendants(&table(), 33879).is_empty(), "the other session runs no call");
        assert!(work_descendants(&table(), 1).is_empty());
    }

    // D-15: today a `sh -lc server` the session started counts as work and is killed in the
    // pause, and a `node script.js` a tool started is not.
    #[test]
    fn work_is_decided_by_the_process_group_never_by_what_the_command_looks_like() {
        let mut table = table();
        table.push(row(70000, 84509, 70000, &["node", "script.js"]));
        table.push(row(70001, 84509, 84500, &["/bin/zsh", "-c", "context7-mcp"]));
        assert_eq!(work_descendants(&table, 33879), [70000], "the script is work, the MCP server that looks like a shell is not");
        assert!(!work_descendants(&table, 33881).contains(&27047), "a shell that is an MCP server is never killed");
    }

    #[test]
    fn mcp_servers_and_their_own_children_are_not_a_running_call_and_an_unknown_process_is_unknown() {
        assert_eq!(work_running(&table(), 33879), Some(false), "only an MCP server and the mod: no call runs");
        assert_eq!(work_running(&table(), 1), None);
        assert_eq!(work_running(&[row(101, 100, 100, &["alondra", "node", "connect"])], 101), None, "its session is not in the table");
    }

    #[test]
    fn a_session_runs_while_its_mod_has_it_as_parent_and_is_gone_once_the_mod_is_left_to_launchd() {
        assert_eq!(session_running(&table(), 33881), Some(true));
        assert_eq!(session_running(&[row(101, 1, 101, &[])], 101), Some(false), "its session died");
        assert_eq!(session_running(&[row(101, 100, 100, &[])], 101), Some(false), "its session is not in the table");
        assert_eq!(session_running(&table(), 1), None);
    }
}
