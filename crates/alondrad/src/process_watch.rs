//! Whether a Claude session still has a shell of its own running. Claude Code runs every Bash
//! call, foreground or background, in a `zsh -c` (or bash/sh) that is a direct child of the
//! session's process, and the node mod's `alondra node connect` is another child of it. A build
//! can stay silent for an hour and still be work; a shell that is gone is not.
use std::process::Command;

#[derive(Debug, PartialEq)]
pub(crate) struct Process {
    pub pid: u32,
    pub ppid: u32,
    /// Cumulative CPU time in hundredths of a second.
    pub cpu_cs: u64,
    pub command: String,
}

pub(crate) fn snapshot() -> Vec<Process> {
    Command::new("ps").args(["-axo", "pid=,ppid=,time=,command="]).output().ok()
        .map(|output| parse(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

pub(crate) fn parse(table: &str) -> Vec<Process> {
    table.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        let (pid, ppid) = (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?);
        let cpu_cs = cpu_centiseconds(fields.next()?)?;
        Some(Process { pid, ppid, cpu_cs, command: fields.collect::<Vec<_>>().join(" ") })
    }).collect()
}

/// `ps` prints CPU time as `M:SS.cc`, `H:MM:SS` or `D-HH:MM:SS`.
fn cpu_centiseconds(text: &str) -> Option<u64> {
    let (days, clock) = text.split_once('-').map_or((0, text), |(days, clock)| (days.parse().unwrap_or(0), clock));
    let mut seconds = 0.0_f64;
    for part in clock.split(':') {
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(((seconds + days as f64 * 86_400.0) * 100.0).round() as u64)
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

/// Every process the session's shell calls started, the shells included, deepest first: what a
/// paused node still runs after its turn was interrupted. The session, its mod connection and its
/// helper servers are left alone.
pub(crate) fn shell_descendants(table: &[Process], mod_pid: u32) -> Vec<u32> {
    let Some(session) = table.iter().find(|process| process.pid == mod_pid).map(|process| process.ppid) else { return vec![] };
    let mut found: Vec<u32> = table.iter().filter(|process| process.ppid == session && is_shell_call(&process.command)).map(|process| process.pid).collect();
    let mut index = 0;
    while index < found.len() {
        let parent = found[index];
        let children: Vec<u32> = table.iter().filter(|process| process.ppid == parent && !found.contains(&process.pid)).map(|process| process.pid).collect();
        found.extend(children);
        index += 1;
    }
    found.reverse();
    found
}

/// The `count` processes using the most memory, by program name and resident bytes.
pub(crate) fn top_memory(count: usize) -> Vec<(String, u64)> {
    let table = Command::new("ps").args(["-axo", "rss=,comm="]).output().ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned()).unwrap_or_default();
    top_of(&table, count)
}

pub(crate) fn top_of(table: &str, count: usize) -> Vec<(String, u64)> {
    let mut processes: Vec<(String, u64)> = table.lines().filter_map(|line| {
        let (rss, command) = line.trim_start().split_once(char::is_whitespace)?;
        let name = command.trim().rsplit('/').next()?.to_owned();
        Some((name, rss.parse::<u64>().ok()? * 1024))
    }).collect();
    processes.sort_by(|a, b| b.1.cmp(&a.1));
    processes.truncate(count);
    processes
}

pub(crate) fn signal(pids: &[u32], signal: libc::c_int) {
    for pid in pids {
        unsafe { libc::kill(*pid as libc::pid_t, signal) };
    }
}

/// The CPU time the session's own process has used in all, when the mod's process is in the table.
pub(crate) fn session_cpu(table: &[Process], mod_pid: u32) -> Option<u64> {
    let session = table.iter().find(|process| process.pid == mod_pid)?.ppid;
    table.iter().find(|process| process.pid == session).map(|process| process.cpu_cs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
27019     1 7:16.28 claude bg-spare --bg-spare /tmp/x.claim.sock
27045 27019 0:00.10 npm exec @upstash/context7-mcp@latest
27046 27045 0:00.00 sh -c context7-mcp
33881 27019 0:01.00 /opt/homebrew/opt/alondra/bin/alondra node connect
84509     1 1:02:03 claude bg-spare --bg-spare /tmp/y.claim.sock
27050 84509 0:00.20 /opt/homebrew/opt/alondra/bin/alondra mcp-node
33879 84509 0:01.00 /opt/homebrew/opt/alondra/bin/alondra node connect
65945 27019 0:00.00 /bin/zsh -c -l setopt NO_EXTENDED_GLOB && eval 'bazel test //...'
";

    #[test]
    fn a_shell_call_that_is_a_direct_child_of_the_session_means_it_runs_something() {
        assert_eq!(shell_running(&parse(TABLE), 33881), Some(true), "the zsh -c of a Bash call is the session's child");
    }

    #[test]
    fn what_a_paused_node_still_runs_is_its_shells_and_their_children_never_the_session_its_mod_or_its_helpers() {
        let table = parse(&format!("{TABLE}65950 65945 0:30.00 bazel test //...\n65951 65950 1:00.00 rustc --crate-name alondrad\n"));
        assert_eq!(shell_descendants(&table, 33881), [65951, 65950, 65945], "deepest first");
        assert!(shell_descendants(&table, 33879).is_empty(), "the other session runs no call");
        assert!(shell_descendants(&table, 1).is_empty());
    }

    #[test]
    fn the_processes_using_the_most_memory_come_first_by_name() {
        let top = top_of(" 1200 /Applications/Xcode.app/Contents/MacOS/Xcode\n  300 /bin/zsh\n900000 /usr/bin/rustc\n", 2);
        assert_eq!(top, [("rustc".to_owned(), 900_000 * 1024), ("Xcode".to_owned(), 1200 * 1024)]);
    }

    #[test]
    fn helper_servers_and_their_own_shells_are_not_a_running_call_and_an_unknown_process_is_unknown() {
        let table = parse(TABLE);
        assert_eq!(shell_running(&table, 33879), Some(false), "only an mcp server and the mod: no call runs");
        assert_eq!(shell_running(&table, 1), None);
    }

    #[test]
    fn a_shell_two_levels_down_belongs_to_a_helper_not_to_the_session() {
        let table = parse("100 1 0:10.00 claude\n101 100 0:01.00 alondra node connect\n102 100 0:01.00 npm exec x\n103 102 0:00.00 sh -c x\n");
        assert_eq!(shell_running(&table, 101), Some(false));
    }

    #[test]
    fn the_session_process_cpu_time_reads_every_ps_format() {
        let table = parse(TABLE);
        assert_eq!(session_cpu(&table, 33881), Some(43_628), "7:16.28");
        assert_eq!(session_cpu(&table, 33879), Some(372_300), "1:02:03");
        assert_eq!(cpu_centiseconds("2-01:00:00"), Some(17_280_000 + 360_000));
        assert_eq!(session_cpu(&table, 1), None);
    }
}
