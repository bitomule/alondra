//! What the daemon, the CLI and the clients all reach on this Mac by themselves, without the
//! engine's ports: Bonjour, the Claude Code install and its updater, the record of failed daemon
//! starts, the Homebrew install, the user the process runs as and the programs on a PATH. Only the
//! crates that use it link it, so editing it rebuilds them and not every crate above `core`.
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

pub mod bonjour;
pub mod brew;
pub mod claude_code;
pub mod claude_update;
pub mod launcher;
pub mod startup;
pub mod user;
