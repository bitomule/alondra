//! The browser's return from an MCP server's sign-in, handed to Claude's own listener on this Mac.
//! The return carries a one-time code: it lives only in memory, for that one request.
pub trait SignInReturns: Send + Sync {
    /// Asks Claude's listener on loopback `port` for `target` (`/callback?…`), as a browser on this
    /// Mac would, and gives back the HTTP status it answers with once the exchange is over.
    fn hand_back(&self, port: u16, target: &str) -> Result<u16, String>;
    /// Whether something listens on loopback `port`, as a browser would reach it.
    fn listening(&self, port: u16) -> bool;
}
