//! What Alondra knows of an MCP server whichever agent runs it. The provider reads its own
//! configuration and says where each server comes from and how the user connects it; Alondra never
//! reads a provider's configuration or knows its file names.
use serde::{Deserialize, Serialize};

/// Whose server it is: the user's own, wherever the agent runs, or one project's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpScope {
    User,
    #[default]
    Project,
}

/// Where the provider found it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpOrigin {
    /// The user's own settings of the agent.
    UserSettings,
    /// The project's shared settings, kept in its repository.
    ProjectSettings,
    /// The user's settings for this one project.
    LocalSettings,
    /// A plugin the user installed.
    Plugin,
    /// A connector of the provider's account, connected on its website.
    Connector,
    /// The user's organization.
    Managed,
    #[default]
    #[serde(other)]
    Unknown,
}

/// How the user connects it when it needs to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpConnect {
    /// The agent opens a sign-in the user finishes in a browser.
    SignIn,
    /// It is connected on the provider's website; the agent then connects it again.
    Connector,
    /// Nothing to sign in to: it runs on the user's machine.
    #[default]
    #[serde(other)]
    None,
}

/// What the provider says of a server beyond its name and state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct McpTraits {
    #[serde(default)]
    pub scope: McpScope,
    #[serde(default)]
    pub origin: McpOrigin,
    #[serde(default)]
    pub connect: McpConnect,
    /// The agent can connect it again in a running session, reading its sign-in anew.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reconnect: bool,
    /// The agent can forget, and revoke, the sign-in it keeps for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sign_out: bool,
}
