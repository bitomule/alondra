//! Turns: what the host says from the prompt it is offered to the result of the turn.
use super::*;

/// The observations the engine reads a turn by: the native queue's lifecycle of each input, the
/// session's init, Claude's messages and the result, up to the first result.
fn turn(observations: &[String]) -> Vec<&str> {
    let read = |observation: &&String| {
        observation.starts_with("status command_lifecycle ") || observation.starts_with("initialized ") || observation.starts_with("message ") || observation.starts_with("result ")
    };
    let mut turn: Vec<&str> = Vec::new();
    for observation in observations.iter().filter(read) {
        turn.push(observation);
        if observation.starts_with("result ") {
            break;
        }
    }
    turn
}

#[test]
fn a_prompt_claude_has_no_script_for_is_queued_started_and_answered_with_a_result() {
    let launched = launch("Write the release notes", None);
    assert_eq!(
        turn(&launched.observations),
        ["status command_lifecycle queued", "status command_lifecycle started", "initialized system init", "result result success false completed"],
        "{:#?}",
        launched.observations
    );
}
