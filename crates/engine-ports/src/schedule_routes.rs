//! The schedule routes a client may call, as the server routes them and the engine answers them.

fn id(path: &str) -> Option<&str> {
    (!path.is_empty() && path.len() <= 256 && path.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')).then_some(path)
}

/// The schedule a `/schedules/{id}` path names, and `run` when it is `/schedules/{id}/runs`.
pub fn schedule_path(path: &str) -> Option<(&str, Option<&str>)> {
    let suffix = path.strip_prefix("/schedules/")?;
    if let Some(schedule_id) = suffix.strip_suffix("/runs") {
        return Some((id(schedule_id)?, Some("run")));
    }
    Some((id(suffix)?, None))
}

pub fn approved_read_path(path: &str) -> bool {
    path == "/schedules" || schedule_path(path).is_some_and(|(_, action)| action.is_none())
}

pub fn approved_mutation_path(method: &str, path: &str, body: bool) -> bool {
    match (method, path, body) {
        ("POST", "/schedules", true) => true,
        ("PUT", _, true) | ("DELETE", _, false) => schedule_path(path).is_some_and(|(_, action)| action.is_none()),
        ("POST", _, false) => schedule_path(path).is_some_and(|(_, action)| action == Some("run")),
        _ => false,
    }
}
