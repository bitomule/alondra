//! What closing a `claude --bg` session asks of the machine and what it comes to: the engine closes
//! it, and a test stands in for Claude Code and git.
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;

pub type Answer<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a `claude` command answered: whether it exited with 0, and what it printed.
#[derive(Clone, Debug, Default)]
pub struct Ran {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    pub fn ok(stdout: impl Into<String>) -> Self {
        Self { success: true, stdout: stdout.into(), stderr: String::new() }
    }

    pub fn failed(stderr: impl Into<String>) -> Self {
        Self { success: false, stdout: String::new(), stderr: stderr.into() }
    }

    /// What it printed, to show.
    pub fn text(&self) -> String {
        format!("{}{}", self.stdout, self.stderr).trim().to_string()
    }
}

/// What closing a session asks of the machine, so a test can stand in for Claude Code and git.
pub trait Sessions: Send + Sync + 'static {
    /// Err when `claude` could not run or did not answer in time.
    fn claude<'a>(&'a self, args: Vec<String>) -> Answer<'a, Result<Ran, String>>;
    fn leftovers<'a>(&'a self, session_id: &'a str) -> Answer<'a, Option<String>>;

    /// The JSON of `claude agents --json --all`, only when it exited with 0.
    fn agents<'a>(&'a self) -> Answer<'a, Result<String, String>> {
        Box::pin(async move { listing(self.claude(agents_args()).await?) })
    }
}

/// The arguments of `claude agents --json --all`.
pub fn agents_args() -> Vec<String> {
    vec!["agents".into(), "--json".into(), "--all".into()]
}

/// The JSON `claude agents` printed, only when it exited with 0.
pub fn listing(ran: Ran) -> Result<String, String> {
    if ran.success { Ok(ran.stdout) } else { Err(format!("claude agents failed: {}", ran.text())) }
}

/// A machine-readable reason a close was refused, so each client can show it in the user's
/// language instead of the English `error` string.
#[derive(Serialize, Debug, PartialEq)]
pub struct Reason {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub enum CloseOutcome {
    /// `claude rm` removed it and `claude agents` no longer lists it.
    Closed { output: String },
    /// `claude agents` does not list it: an interactive session, or one already deleted.
    NotListed,
    /// Something real stops the close: busy, a pending permission, unsaved work.
    Refused { reasons: Vec<Reason>, message: String },
    /// `claude` failed or its answer was not the expected one, with the HTTP status that says how.
    Failed { status: u16, message: String },
}
