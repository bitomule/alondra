//! Arguments: the command line a task's native Claude starts with, from what its launch chose.
use super::*;

fn worker(model: Option<&str>, effort: Option<&str>, disallowed_tools: &[&str]) -> WorkerProfile {
    WorkerProfile {
        plugins: Vec::new(),
        settings: serde_json::Map::new(),
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        disallowed_tools: disallowed_tools.iter().map(|tool| (*tool).to_owned()).collect(),
        handoff_context: None,
    }
}

fn flag(arguments: &[String], flag: &str) -> Option<String> {
    let inline = format!("{flag}=");
    arguments.iter().enumerate().find_map(|(at, argument)| {
        argument.strip_prefix(&inline).map(str::to_owned).or_else(|| (argument == flag).then(|| arguments.get(at + 1).cloned()).flatten())
    })
}

#[test]
fn the_model_effort_and_denied_tools_a_launch_chose_reach_the_native_claude() {
    let arguments = launch("Write the release notes", Some(worker(Some("opus"), Some("xhigh"), &["Bash", "WebFetch"]))).arguments;
    assert_eq!(flag(&arguments, "--model").as_deref(), Some("opus"), "{arguments:?}");
    assert_eq!(flag(&arguments, "--effort").as_deref(), Some("xhigh"), "{arguments:?}");
    assert_eq!(flag(&arguments, "--disallowedTools").map(|tools| tools.split(',').map(str::to_owned).collect::<Vec<_>>()), Some(vec!["Bash".to_owned(), "WebFetch".to_owned()]), "{arguments:?}");
    assert_eq!(flag(&arguments, "--permission-mode").as_deref(), Some("auto"), "{arguments:?}");
}

#[test]
fn a_launch_that_chose_nothing_leaves_claude_its_defaults_in_a_new_session() {
    let arguments = launch("Write the release notes", Some(worker(None, None, &[]))).arguments;
    assert_eq!(flag(&arguments, "--model"), None, "{arguments:?}");
    assert_eq!(flag(&arguments, "--effort"), None, "{arguments:?}");
    assert_eq!(flag(&arguments, "--disallowedTools"), None, "{arguments:?}");
    assert!(flag(&arguments, "--session-id").is_some(), "{arguments:?}");
    assert_eq!(flag(&arguments, "--resume"), None, "{arguments:?}");
}
