//! Whether a Claude session still has a shell of its own running. Claude Code runs every Bash
//! call, foreground or background, in a `zsh -c` (or bash/sh) that is a direct child of the
//! session's process, and the node mod's `alondra node connect` is another child of it. A build
//! can stay silent for an hour and still be work; a shell that is gone is not.
use std::process::Command;

#[derive(Debug, PartialEq)]
pub(crate) struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub command: String,
}

pub(crate) fn snapshot() -> Vec<Process> {
    Command::new("ps").args(["-axo", "pid=,ppid=,command="]).output().ok()
        .map(|output| parse(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

pub(crate) fn parse(table: &str) -> Vec<Process> {
    table.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        let (pid, ppid) = (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?);
        Some(Process { pid, ppid, command: fields.collect::<Vec<_>>().join(" ") })
    }).collect()
}

fn is_shell_call(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let program = words.next().and_then(|path| path.rsplit('/').next());
    matches!(program, Some("zsh" | "bash" | "sh")) && words.any(|word| word.starts_with('-') && !word.starts_with("--") && word.ends_with('c'))
}

/// Whether the session whose mod connection is process `mod_pid` has a shell call running. None
/// when that process is not in the table: nothing is known.
pub(crate) fn shell_running(table: &[Process], mod_pid: u32) -> Option<bool> {
    let session = table.iter().find(|process| process.pid == mod_pid)?.ppid;
    Some(table.iter().any(|process| process.ppid == session && is_shell_call(&process.command)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
27019     1 claude bg-spare --bg-spare /tmp/x.claim.sock
27045 27019 npm exec @upstash/context7-mcp@latest
27046 27045 sh -c context7-mcp
33881 27019 /opt/homebrew/opt/alondra/bin/alondra node connect
84509     1 claude bg-spare --bg-spare /tmp/y.claim.sock
27050 84509 /opt/homebrew/opt/alondra/bin/alondra mcp-node
33879 84509 /opt/homebrew/opt/alondra/bin/alondra node connect
65945 27019 /bin/zsh -c -l setopt NO_EXTENDED_GLOB && eval 'bazel test //...'
";

    #[test]
    fn a_shell_call_that_is_a_direct_child_of_the_session_means_it_runs_something() {
        assert_eq!(shell_running(&parse(TABLE), 33881), Some(true), "the zsh -c of a Bash call is the session's child");
    }

    #[test]
    fn helper_servers_and_their_own_shells_are_not_a_running_call_and_an_unknown_process_is_unknown() {
        let table = parse(TABLE);
        assert_eq!(shell_running(&table, 33879), Some(false), "only an mcp server and the mod: no call runs");
        assert_eq!(shell_running(&table, 1), None);
    }

    #[test]
    fn a_shell_two_levels_down_belongs_to_a_helper_not_to_the_session() {
        let table = parse("100 1 claude\n101 100 alondra node connect\n102 100 npm exec x\n103 102 sh -c x\n");
        assert_eq!(shell_running(&table, 101), Some(false));
    }
}
