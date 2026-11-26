//! The MCP servers of each node that runs through the Agent SDK, as its Claude reported them, and
//! the sign-ins Claude opened for them. Claude keeps every credential: Alondra keeps only how each
//! server is and where the user signs in. The browser's return from a sign-in carries a one-time
//! code; it reaches Claude's own listener and is never recorded here.
use super::*;

pub(crate) fn bounded(field: &str, value: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return refuse("invalid_input", field, &format!("give a non-empty value of at most {max} bytes without control characters"));
    }
    Ok(())
}
pub(crate) fn valid_servers(servers: &[McpServerState]) -> Result<()> {
    if servers.len() > MAX_MCP_SERVERS {
        return refuse("invalid_input", "servers", &format!("at most {MAX_MCP_SERVERS} MCP servers"));
    }
    for server in servers {
        bounded("servers.name", &server.name, MAX_NAME_BYTES)?;
        for (field, value) in [("servers.error", &server.error), ("servers.source", &server.source), ("servers.transport", &server.transport)] {
            if let Some(value) = value {
                bounded(field, value, MAX_NAME_BYTES)?;
            }
        }
        if let Some(url) = &server.url {
            bounded("servers.url", url, MAX_SERVER_URL_BYTES)?;
            if url.contains(['?', '#', '@']) {
                return refuse("invalid_input", "servers.url", "keep only the scheme, host and path of the address");
            }
        }
    }
    Ok(())
}
pub(crate) fn valid_sign_in(sign_in: &McpSignIn) -> Result<()> {
    match sign_in {
        McpSignIn::Browser { url, callback_port, state } => {
            bounded("sign_in.url", url, MAX_SIGN_IN_URL_BYTES)?;
            if !["https://", "http://localhost", "http://127.0.0.1"].iter().any(|scheme| url.starts_with(scheme)) {
                return refuse("invalid_input", "sign_in.url", "the user signs in only on an https page");
            }
            if *callback_port == Some(0) || callback_port.is_some() != state.is_some() {
                return refuse("invalid_input", "sign_in.callback_port", "a sign-in that comes back to Claude has both its port and its state");
            }
            if let Some(state) = state {
                bounded("sign_in.state", state, MAX_NAME_BYTES)?;
            }
            Ok(())
        }
        McpSignIn::Connected => Ok(()),
        McpSignIn::Refused { error } => bounded("sign_in.error", error, MAX_NAME_BYTES),
    }
}
/// The node of the task's current SDK execution of `generation`.
pub(crate) fn mcp_node(domain: &DomainState, task_id: &TaskId, generation: u64) -> Result<AgentId> {
    domain.sdk_lifecycle_at(task_id, generation)?;
    domain.task(task_id)?.agent_id.clone().ok_or_else(|| DomainError::new("no_task", format!("Task {task_id} has no node")))
}
pub(crate) fn record_mcp_servers(domain: &mut DomainState, actor: &Actor, task_id: &TaskId, generation: u64, servers: &[McpServerState]) -> Result<()> {
    system(actor)?;
    valid_servers(servers)?;
    let agent = mcp_node(domain, task_id, generation)?;
    let node = domain.mcp.entry(agent).or_default();
    if node.generation != generation {
        *node = NodeMcp { generation, ..NodeMcp::default() };
    }
    node.servers = node.merged(servers);
    let open: Vec<String> = node.sign_ins.keys().cloned().collect();
    for name in open {
        if node.server(&name).is_none_or(|server| server.status == McpStatus::Connected) {
            node.sign_ins.remove(&name);
        }
    }
    Ok(())
}
pub(crate) fn record_mcp_sign_in(domain: &mut DomainState, actor: &Actor, task_id: &TaskId, generation: u64, server: &str, sign_in: &McpSignIn) -> Result<()> {
    system(actor)?;
    bounded("server", server, MAX_NAME_BYTES)?;
    valid_sign_in(sign_in)?;
    let agent = mcp_node(domain, task_id, generation)?;
    let node = domain.mcp.entry(agent).or_default();
    if node.generation != generation {
        *node = NodeMcp { generation, ..NodeMcp::default() };
    }
    node.refused.remove(server);
    match sign_in {
        McpSignIn::Browser { url, callback_port, state } => {
            node.sign_ins.insert(server.into(), OpenSignIn { url: url.clone(), callback_port: *callback_port, state: state.clone() });
        }
        McpSignIn::Connected => {
            node.sign_ins.remove(server);
        }
        McpSignIn::Refused { error } => {
            node.sign_ins.remove(server);
            node.refused.insert(server.into(), (error.clone(), domain.revision + 1));
        }
    }
    Ok(())
}
/// The browser's return reached Claude's listener: the sign-in is no longer open.
pub(crate) fn complete_mcp_sign_in(domain: &mut DomainState, actor: &Actor, task_id: &TaskId, server: &str, state: &str) -> Result<()> {
    domain.mcp_complete_guard(actor, task_id, server, state)?;
    if let Some(node) = domain.task(task_id)?.agent_id.clone().and_then(|agent| domain.mcp.get_mut(&agent)) {
        node.sign_ins.remove(server);
    }
    Ok(())
}
