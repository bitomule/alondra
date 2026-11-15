//! Why a scenario failed, each reason with its own type: nothing decides by reading a message.

use std::collections::BTreeMap;

#[derive(Debug)]
pub enum ScenarioError {
    /// The format or something it names does not exist.
    Invalid(String),
    /// A step could not be carried out.
    Step { index: usize, reason: String },
    /// The daemon did not settle within the virtual time the scenario allows.
    NoRest { at_step: usize, spent_secs: u64 },
    /// `press` found no such card or action in the view the daemon served.
    ActionMissing { pr: u64, action: String, view: String },
    /// The daemon refused an action.
    ActionRefused { action: String, code: u16, body: String },
    /// What a checkpoint read differs from what the case accepted: per screen, client and language.
    CheckpointDiff { checkpoint: String, differences: BTreeMap<String, String> },
    /// An assertion of the case failed.
    ExpectFailed { checkpoint: String, expected: String, actual: String },
    /// The daemon sent the provider something other than the transcript expected next.
    TranscriptMismatch { expected: Option<String>, received: String },
    /// Something reached for a real service: the network, an outside process, the system clock.
    Leak(String),
}

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid scenario: {reason}"),
            Self::Step { index, reason } => write!(f, "step {index} failed: {reason}"),
            Self::NoRest { at_step, spent_secs } => {
                write!(f, "step {at_step}: the daemon kept changing for {spent_secs} virtual seconds")
            }
            Self::ActionMissing { pr, action, view } => {
                write!(f, "no action `{action}` on the card of PR {pr} in the view:\n{view}")
            }
            Self::ActionRefused { action, code, body } => write!(f, "the daemon refused `{action}` with {code}: {body}"),
            Self::CheckpointDiff { checkpoint, differences } => {
                writeln!(f, "checkpoint `{checkpoint}` differs from the accepted one:")?;
                for (file, diff) in differences {
                    writeln!(f, "--- {file}\n{diff}")?;
                }
                Ok(())
            }
            Self::ExpectFailed { checkpoint, expected, actual } => {
                write!(f, "checkpoint `{checkpoint}`: expected {expected}, found {actual}")
            }
            Self::TranscriptMismatch { expected, received } => {
                write!(f, "the transcript expected {expected:?} and the daemon sent {received}")
            }
            Self::Leak(what) => write!(f, "a real service was reached: {what}"),
        }
    }
}

impl std::error::Error for ScenarioError {}

impl From<crate::claude_peer::PeerError> for ScenarioError {
    fn from(error: crate::claude_peer::PeerError) -> Self {
        match error {
            crate::claude_peer::PeerError::Mismatch { expected, received } => {
                Self::TranscriptMismatch { expected: expected.map(|frame| frame.to_string()), received: received.to_string() }
            }
            crate::claude_peer::PeerError::Protocol(why) => Self::Invalid(why),
        }
    }
}
