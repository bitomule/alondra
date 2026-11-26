//! Hands the browser's return from an MCP server's sign-in to Claude's own listener on this Mac,
//! exactly as a browser on this Mac would. The return carries a one-time code: it lives only in
//! memory, for that one request, and is never written down or logged. Claude exchanges it and
//! keeps the credential; Alondra records only that the sign-in is no longer open. The request
//! itself goes through the `SignInReturns` port.
use crate::domain_runtime::DomainRuntime;
use alondra_domain_model::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION, TaskId};

#[derive(Debug, PartialEq, Eq)]
pub enum SignInError {
    /// Not the address a sign-in comes back to: `http://localhost:<port>/callback?…`.
    NotAReturn,
    /// The domain refuses it: no sign-in open to that server, or another sign-in's return.
    Refused(String),
    /// Claude's listener is gone or did not take it.
    Undelivered(String),
}

/// The return of a sign-in, as the browser was sent to it.
#[derive(Debug, PartialEq, Eq)]
struct Return {
    port: u16,
    state: String,
    target: String,
}

fn parse(address: &str) -> Result<Return, SignInError> {
    let url = url::Url::parse(address).map_err(|_| SignInError::NotAReturn)?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
    let port = url.port().filter(|_| url.scheme() == "http" && loopback && url.path() == "/callback" && url.fragment().is_none());
    let port = port.ok_or(SignInError::NotAReturn)?;
    let pairs: Vec<(String, String)> = url.query_pairs().map(|(key, value)| (key.into_owned(), value.into_owned())).collect();
    let state = pairs.iter().find(|(key, _)| key == "state").map(|(_, value)| value.clone()).ok_or(SignInError::NotAReturn)?;
    if !pairs.iter().any(|(key, _)| key == "code" || key == "error") {
        return Err(SignInError::NotAReturn);
    }
    Ok(Return { port, state, target: format!("/callback?{}", url.query().unwrap_or_default()) })
}

/// Hands `address`, where the browser came back to after the user signed in to `server` for the
/// task's node, to Claude's listener; then the sign-in is no longer open.
pub fn complete(domain: &DomainRuntime, task: &TaskId, server: &str, address: &str) -> Result<(), SignInError> {
    let back = parse(address)?;
    let state = domain.state().map_err(|error| SignInError::Refused(error.to_string()))?;
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("mcp-sign-in-{}", alondra_foundation::random_hex(16)).into(),
        expected_revision: state.revision,
        command: Command::CompleteMcpSignIn { task_id: task.clone(), server: server.into(), state: back.state.clone() },
    };
    state.guard(&Actor::System, &envelope).map_err(|error| SignInError::Refused(error.message))?;
    let port = state.mcp.get(state.tasks.get(task).and_then(|task| task.agent_id.as_ref()).ok_or(SignInError::NotAReturn)?)
        .and_then(|node| node.sign_ins.get(server))
        .and_then(|open| open.callback_port);
    if port != Some(back.port) {
        return Err(SignInError::Refused(format!("This return is not from the sign-in to {server}")));
    }
    let status = domain.ports().sign_in_returns().hand_back(back.port, &back.target).map_err(SignInError::Undelivered)?;
    if !(200..400).contains(&status) {
        return Err(SignInError::Undelivered(format!("Claude refused the sign-in ({status})")));
    }
    domain.execute_as_system(envelope).map(drop).map_err(|error| SignInError::Refused(format!("{error:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_return_to_claude_s_listener_with_its_state_and_code_is_one() {
        assert_eq!(
            parse("http://localhost:49200/callback?code=one-time&state=s1"),
            Ok(Return { port: 49200, state: "s1".into(), target: "/callback?code=one-time&state=s1".into() })
        );
        assert_eq!(parse("http://127.0.0.1:49200/callback?error=access_denied&state=s1").map(|back| back.state), Ok("s1".into()));
        for refused in [
            "https://localhost:49200/callback?code=c&state=s",
            "http://example.com:49200/callback?code=c&state=s",
            "http://localhost/callback?code=c&state=s",
            "http://localhost:49200/other?code=c&state=s",
            "http://localhost:49200/callback?code=c",
            "http://localhost:49200/callback?state=s",
            "http://localhost:49200/callback?code=c&state=s#fragment",
            "com.bitomule.alondra:/mcp-callback?code=c&state=s",
            "not a url",
        ] {
            assert_eq!(parse(refused), Err(SignInError::NotAReturn), "{refused}");
        }
    }
}
