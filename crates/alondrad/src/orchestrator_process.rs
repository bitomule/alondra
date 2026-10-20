//! Claude's print protocol and owned process. No terminal or background-agent transport.
use std::path::Path;
use std::process::Stdio;
use std::os::fd::AsRawFd;
use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

const MAX_LINE: usize = 4 * 1024 * 1024;
const MAX_DIAGNOSTIC: usize = 8192;

/// This lock remains inherited by Claude across an abrupt daemon exit. A replacement
/// daemon must not act on the same conversation until the old process has exited.
pub struct Ownership { file: std::fs::File, lent: std::sync::atomic::AtomicBool }
/// A forked child holds a duplicate of this descriptor until it execs, and closing only
/// our copy would keep the lock alive in it. Unlocking releases every duplicate, so it is
/// only done while no orchestrator child has been handed the lock.
impl Drop for Ownership {
    fn drop(&mut self) {
        if !self.lent.load(std::sync::atomic::Ordering::SeqCst) {
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN); }
        }
    }
}
impl Ownership {
    pub fn acquire(directory: &Path) -> Result<Option<Self>> {
        std::fs::create_dir_all(directory)?;
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(directory.join("executor.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::WouldBlock { return Ok(None); }
            return Err(e.into());
        }
        Ok(Some(Self { file, lent: false.into() }))
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ProcessOwner {
    daemon_pid: u32,
    daemon_started: u64,
    child_pid: u32,
    child_started: u64,
    run_id: String,
}

#[cfg(target_os = "macos")]
fn identity(pid: u32, _expected_run: Option<&str>) -> Option<u64> {
    // Kernel birth time includes microseconds. macOS may redact argv/environment even for
    // a same-user child, so environment inspection is not a reliable ownership primitive.
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let read = unsafe { libc::proc_pidinfo(pid as i32, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size as i32) };
    if read != size as i32 { return None; }
    let info = unsafe { info.assume_init() };
    Some(info.pbi_start_tvsec.saturating_mul(1_000_000).saturating_add(info.pbi_start_tvusec))
}

#[cfg(not(target_os = "macos"))]
fn identity(pid: u32, expected_run: Option<&str>) -> Option<u64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    let refresh = if expected_run.is_some() { ProcessRefreshKind::nothing().with_environ(UpdateKind::Always) } else { ProcessRefreshKind::nothing() };
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, refresh);
    let process = system.process(pid)?;
    if let Some(run) = expected_run {
        let marker = format!("ALONDRA_ORCHESTRATOR_EXECUTION={run}");
        if !process.environ().iter().any(|value| value == std::ffi::OsStr::new(&marker)) { return None; }
    }
    Some(process.start_time())
}

impl ProcessOwner {
    pub(crate) fn record(directory: &Path, child_pid: u32, run_id: &str) -> Result<()> {
        let daemon_pid = std::process::id();
        let owner = Self {
            daemon_pid,
            daemon_started: identity(daemon_pid, None).context("cannot identify orchestrator daemon process")?,
            child_pid,
            child_started: identity(child_pid, Some(run_id)).context("cannot identify owned Claude process")?,
            run_id: run_id.into(),
        };
        let temporary = directory.join(format!("owner-{}.tmp", uuid()?));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        file.write_all(&serde_json::to_vec(&owner)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, directory.join("owner.json"))?;
        std::fs::File::open(directory)?.sync_all()?;
        Ok(())
    }
}

/// At recovery, under this daemon's ownership: a process a previous daemon recorded for the run it
/// left unfinished is ended if it still runs, and its record goes. One the lock already ended, or
/// one that is not the recorded process any more, is left alone.
pub fn end_recorded(directory: &Path, active_run: &str) -> Result<()> {
    let path = directory.join("owner.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let owner: ProcessOwner = serde_json::from_slice(&bytes)?;
    if owner.run_id == active_run
        && identity(owner.daemon_pid, None) != Some(owner.daemon_started)
        && identity(owner.child_pid, Some(&owner.run_id)) == Some(owner.child_started)
        && unsafe { libc::getpgid(owner.child_pid as i32) } == owner.child_pid as i32
        && unsafe { libc::kill(-(owner.child_pid as i32), libc::SIGKILL) } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    std::fs::remove_file(&path)?;
    Ok(())
}

/// Called only while the inherited execution lock is held by a previous process. A PID
/// alone is never authority to signal: verify its kernel birth identity, durable execution and group.
pub fn terminate_orphan(directory: &Path, active_run: Option<&str>) -> Result<()> {
    let owner: ProcessOwner = serde_json::from_slice(&std::fs::read(directory.join("owner.json"))
        .context("execution lock is held, but its owner metadata is unavailable")?)?;
    if active_run != Some(owner.run_id.as_str()) { bail!("execution lock owner does not match the durable execution"); }
    if identity(owner.daemon_pid, None) == Some(owner.daemon_started) { bail!("another live daemon owns the orchestrator execution"); }
    // Failure to inspect a live PID is not proof that the owning daemon disappeared.
    if identity(owner.daemon_pid, None).is_none() {
        let exists = unsafe { libc::kill(owner.daemon_pid as i32, 0) };
        if exists == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) { bail!("cannot prove that the prior daemon exited"); }
    }
    if identity(owner.child_pid, Some(&owner.run_id)) != Some(owner.child_started) {
        bail!("execution lock remains held; its process identity cannot be verified safely");
    }
    if unsafe { libc::getpgid(owner.child_pid as i32) } != owner.child_pid as i32 {
        bail!("orchestrator owner is not the expected process-group leader");
    }
    if unsafe { libc::kill(-(owner.child_pid as i32), libc::SIGKILL) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// How a batch's turn ended, from what Claude reported and how its process exited; never from a
/// clock. `domain_orchestrator::tick` carries each one out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchEnd {
    /// The turn finished with its result: the batch was handed over.
    Handled,
    /// Nothing the run did can have changed anything: the batch is offered again on its own.
    NotStarted { why: String },
    /// Claude did not finish and may have changed something: the user decides.
    Interrupted { calls: Vec<String>, why: String },
    /// One of Alondra's own tools was refused to Claude, after it may have changed something.
    Denied { calls: Vec<String>, why: String },
    /// Usage ran out before Claude answered: the batch waits for the reset.
    Limited { reset: Option<i64> },
    /// The resumed conversation is gone: the next run starts a new one.
    SessionMissing,
    /// The user stopped the run; `effects` are the changes the daemon let it make, if it made any.
    Cancelled { effects: Option<Vec<String>> },
}

#[derive(Default, Debug)]
pub struct Stream {
    restricted: bool,
    /// The id the SDK gives the batch's one prompt: a result that names another is not this batch's.
    input: Option<String>,
    pub initialized: bool,
    pub result: bool,
    pub success: bool,
    pub error: Option<String>,
    /// The context of the batch's first model call, and of its latest one.
    pub first_input_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub window: Option<u64>,
    /// What Claude said at the end of a successful turn.
    pub note: Option<String>,
    pub model: Option<String>,
    pub compacted: bool,
    pub post_compact_tokens: Option<u64>,
    pub acted: bool,
    pub missing_session: bool,
    pub denied: bool,
    /// Claude said the plan's usage ran out, and the reset it reported in Unix milliseconds.
    pub limited: bool,
    pub limit_reset: Option<i64>,
    /// The model answered at least once, so the batch may have done something.
    pub worked: bool,
    /// The Alondra tools Claude called that change something, in order.
    pub alondra_calls: Vec<String>,
}
impl Stream {
    /// The parser of a batch run through the Agent SDK host, whose prompt the SDK calls `input`.
    pub fn for_input(input: &str) -> Self {
        Self { restricted: true, input: Some(input.to_owned()), ..Self::default() }
    }
    /// The context the batch read, once the model answered at least once.
    pub fn context(&self) -> Option<crate::domain_orchestrator::BatchContext> {
        Some(crate::domain_orchestrator::BatchContext { first: self.first_input_tokens?, last: self.input_tokens?, window: self.window })
    }
    /// Whether usage ran out, with the reset Claude reported if it did.
    pub fn limit(&self, diagnostic: &str) -> Option<Option<i64>> {
        if self.limited {
            return Some(self.limit_reset);
        }
        let said = |text: &str| crate::usage_pause::says_usage_limit(text).then(|| crate::usage_pause::reset_in(text, alondra_core::now_millis()));
        self.error.as_deref().and_then(said).or_else(|| said(diagnostic))
    }
    fn session_missing(&self, exit_ok: bool, diagnostic: &str, session: &str) -> bool {
        !self.initialized && !self.acted && !self.compacted && self.missing_session && !exit_ok
            && (self.input.is_some() || diagnostic.lines().any(|line| line.trim() == format!("No conversation found with session ID: {session}")))
    }
    /// How the batch's turn ended.
    pub fn end(&self, exit_ok: bool, diagnostic: &str, session: &str, resumed: bool) -> BatchEnd {
        let effects = self.may_have_effects().then(|| self.alondra_calls.clone());
        if resumed && self.session_missing(exit_ok, diagnostic, session) {
            return BatchEnd::SessionMissing;
        }
        if exit_ok && self.success && self.result && self.initialized && self.error.is_none() {
            return BatchEnd::Handled;
        }
        if let Some(reset) = self.limit(diagnostic) && !self.worked {
            return BatchEnd::Limited { reset };
        }
        let why = self.error.clone().unwrap_or_else(|| if diagnostic.trim().is_empty() {
            "Claude exited without a successful terminal result".into()
        } else {
            diagnostic.to_owned()
        });
        match effects {
            None => BatchEnd::NotStarted { why },
            Some(calls) if self.denied => BatchEnd::Denied { calls, why },
            Some(calls) => BatchEnd::Interrupted { calls, why },
        }
    }
    /// Whether the run may have changed something: it called a tool that does, or Claude was
    /// answering when the run ended without its result, so what came next is unknown.
    pub fn may_have_effects(&self) -> bool {
        !self.alondra_calls.is_empty() || (self.acted && !self.result)
    }
    /// Session fencing applies to every session-bearing record, including terminal results.
    pub fn accept(&mut self, line: &[u8], session: &str) -> Result<bool> {
        let v: Value = serde_json::from_slice(line).context("invalid Claude JSONL")?;
        if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) { return Ok(false); }
        if let Some(id) = v.get("session_id").and_then(Value::as_str) {
            if id != session { bail!("Claude emitted a different session ID"); }
        }
        match (v["type"].as_str(), v["subtype"].as_str()) {
            (Some("system"), Some("init")) => {
                if v["session_id"] != session { bail!("Claude initialization omitted its session ID"); }
                if v["permissionMode"] != "auto" { bail!("Claude did not initialize in auto permission mode"); }
                let tools = v["tools"].as_array();
                // Claude prefixes plugin MCP names. Accept only the two exact launch forms
                // observed for this server, paired with their matching tool names.
                let required: &[&str] = if self.restricted { &["create_task", "ask"] } else { &["fleet", "ask"] };
                let available = [
                    ("plugin:alondra-orchestrator:alondra-orchestrator", "mcp__plugin_alondra-orchestrator_alondra-orchestrator"),
                    ("alondra-orchestrator", "mcp__alondra-orchestrator"),
                ].iter().any(|(server, prefix)| {
                    v["mcp_servers"].as_array().is_some_and(|servers| servers.iter().any(|s| s["name"] == *server && s["status"] == "connected"))
                        && tools.is_some_and(|tools| required.iter().all(|name| {
                            let expected = format!("{prefix}__{name}");
                            tools.iter().any(|t| t.as_str() == Some(expected.as_str()))
                        }))
                });
                if !available {
                    let servers: Vec<String> = v["mcp_servers"].as_array().into_iter().flatten()
                        .map(|server| format!("{} {}", server["name"].as_str().unwrap_or("?"), server["status"].as_str().unwrap_or("?")))
                        .collect();
                    bail!("required Alondra MCP tools are unavailable (MCP servers: {})", if servers.is_empty() { "none".into() } else { servers.join(", ") });
                }
                let servers = v["mcp_servers"].as_array().expect("checked above");
                if self.restricted && servers.iter().any(|server| {
                    let name = server["name"].as_str().unwrap_or_default();
                    server["status"] == "connected"
                        && name != "alondra-orchestrator"
                        && name != "plugin:alondra-orchestrator:alondra-orchestrator"
                }) {
                    bail!("an unrelated MCP server is connected to the orchestrator");
                }
                if self.restricted && tools.expect("checked above").iter().any(|tool| {
                    let name = tool.as_str().unwrap_or_default();
                    !RESTRICTED_TOOLS.split(',').any(|allowed| allowed == name) && !alondra_tool(name)
                }) {
                    bail!("orchestrator has an unexpected built-in or MCP tool");
                }
                self.initialized = true;
                self.model = v["model"].as_str().map(str::to_owned);
                return Ok(true);
            }
            (Some("system"), Some("compact_boundary")) => {
                self.compacted = true;
                self.post_compact_tokens = v.pointer("/compact_metadata/post_tokens").and_then(Value::as_u64);
            }
            (Some("rate_limit_event"), _) => {
                let info = &v["rate_limit_info"];
                if info["status"] == "rejected" {
                    self.limited = true;
                    self.limit_reset = info["resetsAt"].as_i64().or_else(|| info["resets_at"].as_i64()).map(|seconds| seconds * 1000);
                }
            }
            (Some("assistant"), _) => {
                self.acted = true;
                let calls = v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten()
                    .filter(|block| block["type"] == "tool_use")
                    .filter_map(|block| block["name"].as_str().filter(|name| alondra_tool(name)))
                    .filter_map(|name| name.rsplit("__").next())
                    .filter(|tool| alondra_core::orchestrator_batch::changes_something(tool));
                self.alondra_calls.extend(calls.map(str::to_owned));
                if v["error"] == "rate_limit" {
                    self.limited = true;
                } else {
                    self.worked = true;
                }
                if let Some(u) = v.pointer("/message/usage").filter(|u| u.is_object()) {
                    // Latest main-agent call only; aggregate result usage can include many calls.
                    if let Some(input) = u["input_tokens"].as_u64() {
                        let context = input.saturating_add(u["cache_creation_input_tokens"].as_u64().unwrap_or(0)).saturating_add(u["cache_read_input_tokens"].as_u64().unwrap_or(0));
                        self.first_input_tokens.get_or_insert(context);
                        self.input_tokens = Some(context);
                    }
                }
                if let Some(model) = v.pointer("/message/model").and_then(Value::as_str) { self.model = Some(model.into()); }
            }
            (Some("result"), _) => {
                if self.result { bail!("multiple Claude terminal results"); }
                if v["session_id"] != session { bail!("Claude result omitted its session ID"); }
                if let Some(input) = &self.input
                    && !v["user_message_uuid"].as_str().is_some_and(|id| id.eq_ignore_ascii_case(input))
                    && !(v["subtype"] == "error_during_execution" && v["num_turns"] == 0)
                {
                    bail!("Claude reported the result of another input");
                }
                self.result = true;
                self.missing_session = v["subtype"] == "error_during_execution" && v["is_error"] == true
                    && v["num_turns"] == 0 && v["duration_api_ms"] == 0
                    && v["errors"].as_array().is_some_and(|errors| errors.iter().any(|error| error.as_str() == Some(format!("No conversation found with session ID: {session}").as_str())));
                self.success = v["subtype"] == "success" && v["is_error"] == false;
                if self.success {
                    self.note = v["result"].as_str().map(str::to_owned);
                }
                // A refused optional tool is one Claude worked around; only Alondra's own
                // tools are required for a batch to be handled.
                if v["permission_denials"].as_array().is_some_and(|denials| denials.iter()
                    .any(|denial| denial["tool_name"].as_str().is_some_and(alondra_tool))) {
                    self.success = false;
                    self.denied = true;
                    self.error = Some("Claude denied a required tool permission in auto mode".into());
                }
                if !self.success && v.get("result").and_then(Value::as_str).is_some_and(crate::usage_pause::says_usage_limit) {
                    self.limited = true;
                    self.limit_reset = self.limit_reset.or_else(|| v["result"].as_str().and_then(|text| crate::usage_pause::reset_in(text, alondra_core::now_millis())));
                }
                if !self.success && self.error.is_none() {
                    self.error = Some(bounded(v.get("errors").or_else(|| v.get("result")).unwrap_or(&Value::Null).to_string()));
                }
                if let Some(model) = &self.model { self.window = v["modelUsage"][model]["contextWindow"].as_u64(); }
            }
            _ => {}
        }
        Ok(false)
    }
}

pub(crate) const RESTRICTED_TOOLS: &str = "Skill,Read,Glob,Grep";

fn alondra_tool(name: &str) -> bool {
    name.starts_with("mcp__alondra-orchestrator__") || name.starts_with("mcp__plugin_alondra-orchestrator_alondra-orchestrator__")
}

/// The projects root is readable and never writable: nodes do the work in it.
pub(crate) fn read_only(root: &Path) -> Value {
    let pattern = format!("/{}/**", root.display());
    serde_json::json!({
        "additionalDirectories": [root],
        "allow": [format!("Read({pattern})")],
        "deny": [format!("Edit({pattern})")],
    })
}

pub(crate) fn bounded(mut text: String) -> String {
    if text.len() > MAX_DIAGNOSTIC {
        let mut at = MAX_DIAGNOSTIC;
        while !text.is_char_boundary(at) { at -= 1; }
        text.truncate(at);
    }
    text
}

pub fn uuid() -> Result<String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 16];
    SystemRandom::new().fill(&mut bytes).map_err(|_| anyhow::anyhow!("cannot generate execution identity"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

pub struct Invocation<'a> {
    pub executable: &'a Path,
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub plugins: Vec<&'a Path>,
    pub domain: bool,
    pub projects_root: Option<&'a Path>,
    /// The bearer bound to this execution; only a v2 invocation carries one.
    pub capability: Option<&'a str>,
    pub session: &'a str,
    pub resume: bool,
    pub execution: &'a str,
    pub prompt: &'a str,
    pub ownership: &'a Ownership,
}
impl Invocation<'_> {
    fn command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(self.executable);
        // `--strict-mcp-config` discards user/managed MCP servers. Supply only
        // Alondra's server, with no credential in the command line or config.
        cmd.current_dir(self.cwd).args(["-p", "--output-format", "stream-json", "--verbose", "--model", "opus", "--permission-mode", "auto", "--permission-prompts", "none"]);
        if self.domain {
            let mcp_config = serde_json::json!({"mcpServers":{"alondra-orchestrator":{
                "command":self.plugins[0].join("bin/alondra"), "args":["mcp-orchestrator"]
            }}}).to_string();
            cmd.args(["--restricted", "--tools", RESTRICTED_TOOLS, "--strict-mcp-config"])
                .args(["--mcp-config", &mcp_config]);
        } else {
            cmd.args(["--setting-sources", "user"]);
        }
        cmd.args(["--disallowedTools", "AskUserQuestion", "ExitPlanMode"])
            .args(crate::plugins::launch_flags_with(&self.plugins, self.projects_root.filter(|_| self.domain).map(read_only)))
            .args([if self.resume { "--resume" } else { "--session-id" }, self.session])
            .env("ALONDRA_HOME", self.home)
            .env("ALONDRA_ORCHESTRATOR", "1")
            .env("ALONDRA_ORCHESTRATOR_EXECUTION", self.execution)
            .env("ALONDRA_SESSION_ID", alondra_core::orchestrator::NODE_ID)
            .env_remove("CLAUDECODE")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).process_group(0);
        match self.capability.filter(|_| self.domain) {
            Some(capability) => { cmd.env("ALONDRA_DOMAIN_CAPABILITY", capability); }
            None => { cmd.env_remove("ALONDRA_DOMAIN_CAPABILITY"); }
        }
        if self.domain { cmd.env("ALONDRA_DOMAIN_PROTOCOL", "2"); }
        else { cmd.env_remove("ALONDRA_DOMAIN_PROTOCOL"); }
        self.ownership.lent.store(true, std::sync::atomic::Ordering::SeqCst);
        let fd = self.ownership.file.as_raw_fd();
        // Only async-signal-safe libc operations may run between fork and exec.
        unsafe { cmd.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 { return Err(std::io::Error::last_os_error()); }
            Ok(())
        }); }
        cmd
    }
}

struct ProcessGroup(i32, std::cell::Cell<bool>);
impl ProcessGroup {
    fn kill(&self) { if self.1.replace(false) { unsafe { libc::kill(-self.0, libc::SIGKILL); } } }
    /// macOS does not deliver a group SIGKILL to a child the shell is forking at that instant,
    /// so one signal can leave a descendant holding the execution lock. Signal again until the
    /// group is empty; a group left with only an unreaped zombie answers EPERM, not ESRCH.
    fn terminate(&self) -> impl std::future::Future<Output = ()> + Send + use<> {
        self.1.set(false);
        let group = self.0;
        async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            while unsafe { libc::kill(-group, libc::SIGKILL) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                if tokio::time::Instant::now() >= deadline { break; }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) { self.kill(); }
}

pub struct Output { pub stream: Stream, pub diagnostic: String, pub exit_ok: bool, pub spawned: bool }

pub async fn execute(invocation: Invocation<'_>, mut spawned: impl FnMut(u32) -> Result<()>, mut initialized: impl FnMut() -> Result<()>, cancel: impl std::future::Future<Output = ()>) -> Output {
    let mut stream = Stream::default();
    stream.restricted = invocation.domain;
    let stdin = match prompt_input(invocation.cwd, invocation.execution, invocation.prompt) {
        Ok(stdin) => stdin,
        Err(e) => return Output { stream, diagnostic: bounded(format!("cannot prepare the Claude prompt: {e}")), exit_ok: false, spawned: false },
    };
    let mut child = match invocation.command().stdin(stdin).spawn() {
        Ok(child) => child,
        Err(e) => return Output { stream, diagnostic: bounded(format!("cannot launch Claude: {e}")), exit_ok: false, spawned: false },
    };
    let group = ProcessGroup(child.id().expect("spawned child") as i32, std::cell::Cell::new(true));
    if let Err(error) = ProcessOwner::record(invocation.cwd, group.0 as u32, invocation.execution) {
        group.kill();
        let _ = child.kill().await;
        return Output { stream, diagnostic: bounded(format!("cannot persist process ownership: {error:#}")), exit_ok: false, spawned: true };
    }
    if child.try_wait().ok().flatten().is_none() {
        if let Err(error) = spawned(group.0 as u32) {
            // A one-shot fake or a failed launch can exit before macOS exposes
            // its process identity. It cannot subsequently host an MCP child.
            if child.try_wait().ok().flatten().is_none() {
                group.kill();
                let _ = child.kill().await;
                return Output { stream, diagnostic: bounded(format!("cannot register MCP process: {error:#}")), exit_ok: false, spawned: true };
            }
        }
    }
    let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut stderr = child.stderr.take().expect("piped stderr");
    // Drain indefinitely, retain only a bounded prefix. Pipe backpressure must never block Claude.
    let diagnostics = tokio::spawn(async move {
        let mut retained = Vec::new();
        let mut buf = [0u8; 4096];
        loop { match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => retained.extend_from_slice(&buf[..n.min(MAX_DIAGNOSTIC - retained.len())]),
        }}
        String::from_utf8_lossy(&retained).into_owned()
    });
    let read = async {
        let mut line = Vec::new();
        loop {
            let chunk = stdout.fill_buf().await?;
            if chunk.is_empty() {
                if !line.is_empty() { bail!("truncated Claude JSONL record"); }
                break;
            }
            let n = chunk.iter().position(|b| *b == b'\n').map_or(chunk.len(), |i| i + 1);
            if line.len() + n > MAX_LINE { bail!("Claude JSONL record exceeds safety limit"); }
            line.extend_from_slice(&chunk[..n]);
            stdout.consume(n);
            if line.last() == Some(&b'\n') {
                if line.iter().any(|b| !b.is_ascii_whitespace()) && stream.accept(&line, invocation.session)? { initialized()?; }
                line.clear();
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let process = async {
        read.await?;
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>(status.success())
    };
    let outcome = tokio::select! {
        biased;
        _ = cancel => Err(anyhow::anyhow!("Orchestrator execution was cancelled; check its effects before retrying")),
        result = tokio::time::timeout(std::time::Duration::from_secs(60 * 60), process) => result.unwrap_or_else(|_| Err(anyhow::anyhow!("Claude execution exceeded one hour"))),
    };
    let exit_ok = match outcome {
        Ok(ok) => ok,
        Err(e) => {
            group.kill();
            let _ = child.kill().await;
            stream.error = Some(bounded(e.to_string()));
            false
        }
    };
    // MCP helpers and any ordinary descendants must not survive their execution.
    group.terminate().await;
    // Do not hang on a grandchild which retained stderr after the main process ended.
    let mut diagnostics = diagnostics;
    let diagnostic = match tokio::time::timeout(std::time::Duration::from_secs(2), &mut diagnostics).await {
        Ok(Ok(text)) => text,
        _ => { diagnostics.abort(); "Claude stderr did not close".into() }
    };
    let _ = std::fs::remove_file(invocation.cwd.join("owner.json"));
    Output { stream, diagnostic: bounded(diagnostic), exit_ok, spawned: true }
}

/// Claude reads a piped prompt only if it arrives within 3 seconds; after that it runs
/// without one, and a resumed conversation then fails. A file is readable at once.
fn prompt_input(cwd: &Path, execution: &str, prompt: &str) -> std::io::Result<std::fs::File> {
    use std::io::{Seek, Write};
    let path = cwd.join(format!("prompt-{execution}"));
    let mut file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path)?;
    let written = file.write_all(prompt.as_bytes()).and_then(|()| file.rewind());
    std::fs::remove_file(&path)?;
    written.map(|()| file)
}

impl Output {
    pub fn end(&self, session: &str, resumed: bool) -> BatchEnd {
        self.stream.end(self.exit_ok, &self.diagnostic, session, resumed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","model":"opus","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask"]}"#;
    const WORKED: &str = r#"{"type":"assistant","session_id":"s","message":{"model":"opus","content":[{"type":"text","text":"On it"}],"usage":{"input_tokens":2}}}"#;
    const CREATED: &str = r#"{"type":"assistant","session_id":"s","message":{"model":"opus","content":[{"type":"tool_use","id":"t","name":"mcp__alondra-orchestrator__create_task","input":{}}],"usage":{"input_tokens":2}}}"#;
    const DONE: &str = r#"{"type":"result","session_id":"s","subtype":"success","is_error":false,"permission_denials":[]}"#;

    fn ended(input: Option<&str>, lines: &[&str], exit_ok: bool, diagnostic: &str, resumed: bool) -> BatchEnd {
        let mut stream = match input {
            Some(input) => Stream::for_input(input),
            None => Stream { restricted: true, ..Stream::default() },
        };
        for line in lines {
            if let Err(error) = stream.accept(line.as_bytes(), "s") {
                stream.error = Some(error.to_string());
                break;
            }
        }
        stream.end(exit_ok, diagnostic, "s", resumed)
    }

    #[test]
    fn each_way_a_batch_run_ends_is_one_closed_outcome() {
        let denied = DONE.replace(r#""permission_denials":[]"#, r#""permission_denials":[{"tool_name":"mcp__alondra-orchestrator__create_task"}]"#);
        let limited = r#"{"type":"rate_limit_event","session_id":"s","rate_limit_info":{"status":"rejected","resetsAt":1900000000}}"#;
        let limited_result = r#"{"type":"result","session_id":"s","subtype":"success","is_error":true,"result":"You have hit your session limit"}"#;
        let missing = r#"{"type":"result","session_id":"s","subtype":"error_during_execution","is_error":true,"num_turns":0,"duration_api_ms":0,"errors":["No conversation found with session ID: s"]}"#;
        let created = vec!["create_task".to_owned()];
        assert_eq!(ended(None, &[INIT, WORKED, DONE], true, "", false), BatchEnd::Handled);
        assert!(matches!(ended(None, &[], false, "cannot launch Claude", false), BatchEnd::NotStarted { why } if why == "cannot launch Claude"));
        assert!(matches!(ended(None, &[INIT, WORKED], false, "", false), BatchEnd::Interrupted { calls, .. } if calls.is_empty()),
            "Claude was answering when it ended: what came next is unknown");
        assert!(matches!(ended(None, &[INIT, CREATED], false, "", false), BatchEnd::Interrupted { calls, .. } if calls == created));
        assert!(matches!(ended(None, &[INIT, CREATED, &denied], true, "", false), BatchEnd::Denied { calls, .. } if calls == created));
        assert_eq!(ended(None, &[INIT, limited, limited_result], false, "", false), BatchEnd::Limited { reset: Some(1_900_000_000_000) });
        assert_eq!(ended(None, &[missing], false, "No conversation found with session ID: s", true), BatchEnd::SessionMissing);
        assert!(matches!(ended(None, &[missing], false, "", true), BatchEnd::NotStarted { .. }),
            "claude -p says it on stderr too, or it is not the lost conversation");
        assert_eq!(ended(Some("00000000-0000-4000-8000-000000000001"), &[missing], false, "", true), BatchEnd::SessionMissing,
            "through the SDK the result is the fact");
    }

    #[test]
    fn a_result_names_the_batch_it_ends() {
        let input = "00000000-0000-4000-8000-000000000001";
        let bound = DONE.replace(r#""permission_denials":[]"#, &format!(r#""permission_denials":[],"user_message_uuid":"{input}""#));
        let other = DONE.replace(r#""permission_denials":[]"#, r#""permission_denials":[],"user_message_uuid":"00000000-0000-4000-8000-0000000000ff""#);
        assert_eq!(ended(Some(input), &[INIT, WORKED, &bound], true, "", false), BatchEnd::Handled);
        assert!(matches!(ended(Some(input), &[INIT, WORKED, &other], true, "", false),
            BatchEnd::Interrupted { why, .. } if why.contains("another input")));
        assert!(matches!(ended(Some(input), &[INIT, WORKED, DONE], true, "", false), BatchEnd::Interrupted { .. }),
            "a result that names no input is not this batch's either");
    }

    #[test]
    fn a_process_a_dead_daemon_left_for_its_unfinished_run_is_ended_at_recovery() {
        let directory = crate::test_support::Directory::new();
        std::fs::create_dir_all(&directory).unwrap();
        let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let daemon = gone.id();
        gone.wait().unwrap();
        use std::os::unix::process::CommandExt;
        let mut left = std::process::Command::new("/bin/cat").stdin(Stdio::piped()).process_group(0).spawn().unwrap();
        let (child_pid, child_started) = (left.id(), identity(left.id(), None).unwrap());
        let record = |run: &str| std::fs::write(directory.join("owner.json"), serde_json::to_vec(&ProcessOwner {
            daemon_pid: daemon, daemon_started: 1, child_pid, child_started, run_id: run.into(),
        }).unwrap()).unwrap();
        record("another-run");
        end_recorded(&directory, "unfinished").unwrap();
        assert!(left.try_wait().unwrap().is_none(), "a process recorded for another run is left alone");
        assert!(!directory.join("owner.json").exists());
        record("unfinished");
        end_recorded(&directory, "unfinished").unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(left.wait().unwrap().signal(), Some(libc::SIGKILL));
        assert!(!directory.join("owner.json").exists());
        end_recorded(&directory, "unfinished").unwrap();
    }
    #[test]
    fn latest_main_call_not_aggregate_or_child_usage() {
        let mut s = Stream::default();
        s.accept(br#"{"type":"assistant","message":{"model":"opus","usage":{"input_tokens":3,"cache_read_input_tokens":50,"cache_creation_input_tokens":7}}}"#, "a").unwrap();
        s.accept(br#"{"type":"assistant","parent_tool_use_id":"child","message":{"usage":{"input_tokens":999}}}"#, "a").unwrap();
        s.accept(br#"{"type":"result","session_id":"a","subtype":"success","is_error":false,"usage":{"input_tokens":9999},"modelUsage":{"opus":{"contextWindow":1000000}}}"#, "a").unwrap();
        assert_eq!(s.input_tokens, Some(60));
        assert_eq!(s.window, Some(1000000));
        assert!(s.success);
    }
    /// The result Claude 2.1.287 emits after `--restricted` refuses a Read outside the working directories.
    #[test]
    fn a_rejected_rate_limit_or_a_limit_result_marks_the_stream_limited_with_its_reset() {
        let accept = |lines: &[&str]| {
            let mut s = Stream::default();
            for line in lines {
                s.accept(line.as_bytes(), "a").unwrap();
            }
            s
        };
        let event = accept(&[r#"{"type":"rate_limit_event","session_id":"a","rate_limit_info":{"status":"rejected","resetsAt":1790000000}}"#]);
        assert_eq!((event.limited, event.limit_reset, event.worked), (true, Some(1_790_000_000_000), false));
        let warning = accept(&[r#"{"type":"rate_limit_event","session_id":"a","rate_limit_info":{"status":"allowed_warning","resetsAt":1790000000}}"#]);
        assert!(!warning.limited, "approaching the limit is not the limit");
        let assistant = accept(&[r#"{"type":"assistant","session_id":"a","error":"rate_limit","message":{"content":[]}}"#]);
        assert_eq!((assistant.limited, assistant.worked), (true, false));
        let result = accept(&[r#"{"type":"result","session_id":"a","subtype":"success","is_error":true,"result":"You've hit your session limit · resets 3:45pm"}"#]);
        assert!(result.limited && result.limit_reset.is_some(), "{result:?}");
        let worked = accept(&[r#"{"type":"assistant","session_id":"a","message":{"model":"opus","content":[]}}"#, r#"{"type":"result","session_id":"a","subtype":"success","is_error":true,"result":"You've hit your weekly limit · resets Mon 12:00am"}"#]);
        assert!(worked.limited && worked.worked);
    }

    const REFUSED_READ_RESULT: &[u8] = br#"{"type":"result","session_id":"a","subtype":"success","is_error":false,"terminal_reason":"completed","num_turns":4,"result":"Done.","permission_denials":[{"tool_name":"Read","tool_use_id":"toolu_01Hi3ayKKFGKNW3JAR2tb9Z2","tool_input":{"file_path":"/Users/example/.zshrc","limit":1}}]}"#;

    #[test]
    fn a_refused_optional_tool_in_a_successful_run_is_success() {
        let mut s = Stream::default();
        s.accept(REFUSED_READ_RESULT, "a").unwrap();
        assert!(s.success, "{s:?}");
        assert_eq!(s.error, None);
    }

    #[test]
    fn a_refused_alondra_tool_fails_the_run() {
        for tool in ["mcp__alondra-orchestrator__create_task", "mcp__plugin_alondra-orchestrator_alondra-orchestrator__launch"] {
            let mut s = Stream::default();
            let line = std::str::from_utf8(REFUSED_READ_RESULT).unwrap().replace(r#""tool_name":"Read""#, &format!(r#""tool_name":"{tool}""#));
            s.accept(line.as_bytes(), "a").unwrap();
            assert!(!s.success);
            assert_eq!(s.error.as_deref(), Some("Claude denied a required tool permission in auto mode"));
            assert!(s.denied);
        }
    }

    #[test]
    fn the_projects_root_is_readable_and_never_writable() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let lock = Ownership::acquire(&dir).unwrap().unwrap();
        let root = Path::new("/Users/someone/Projects");
        let mut i = invocation(&dir, Path::new("claude"), &lock);
        i.projects_root = Some(root);
        let args: Vec<_> = i.command().as_std().get_args().map(|s| s.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|a| a == ["--tools", "Skill,Read,Glob,Grep"]), "{args:?}");
        assert!(args.iter().any(|a| a == "--restricted"));
        let parsed = |args: &[String]| -> Vec<Value> {
            args.windows(2).filter(|a| a[0] == "--settings").map(|a| serde_json::from_str(&a[1]).unwrap()).collect()
        };
        let [settings] = parsed(&args).try_into().expect("one --settings");
        assert_eq!(settings["permissions"], serde_json::json!({
            "additionalDirectories": ["/Users/someone/Projects"],
            "allow": ["Read(//Users/someone/Projects/**)"],
            "deny": ["Edit(//Users/someone/Projects/**)"],
        }));
        assert_eq!(settings["env"], serde_json::from_str::<Value>(&alondra_core::launch_settings()).unwrap()["env"]);
        i.projects_root = None;
        let args: Vec<_> = i.command().as_std().get_args().map(|s| s.to_string_lossy().into_owned()).collect();
        let [settings] = parsed(&args).try_into().expect("one --settings");
        assert!(settings.get("permissions").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn init_accepts_the_read_only_search_tools_and_nothing_that_writes() {
        let init = |tool: &str| format!(r#"{{"type":"system","subtype":"init","session_id":"a","permissionMode":"auto","mcp_servers":[{{"name":"alondra-orchestrator","status":"connected"}}],"tools":["Skill","Read","{tool}","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask"]}}"#);
        for tool in ["Glob", "Grep"] {
            let mut s = Stream { restricted: true, ..Stream::default() };
            assert!(s.accept(init(tool).as_bytes(), "a").unwrap());
        }
        for tool in ["Edit", "Write", "Bash", "NotebookEdit"] {
            let mut s = Stream { restricted: true, ..Stream::default() };
            assert!(s.accept(init(tool).as_bytes(), "a").is_err(), "{tool}");
        }
    }

    #[test]
    fn invalid_and_foreign_results_are_not_success() {
        let mut s = Stream::default();
        assert!(s.accept(b"oops", "a").is_err());
        assert!(s.accept(br#"{"type":"result","session_id":"b","subtype":"success"}"#, "a").is_err());
        assert!(!s.success);
    }
    #[test]
    fn compact_boundary_is_distinct_from_successful_noop() {
        let mut s = Stream::default();
        s.accept(br#"{"type":"system","subtype":"compact_boundary","session_id":"a","compact_metadata":{"post_tokens":100}}"#, "a").unwrap();
        assert!(s.compacted);
        assert_eq!(s.post_compact_tokens, Some(100));
        assert!(s.input_tokens.is_none());
    }
    #[test]
    fn orchestrator_init_rejects_node_and_accepts_exact_plugin_name() {
        let mut stream = Stream::default();
        let node_only = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"plugin:alondra:alondra","status":"connected"}],"tools":["mcp__plugin_alondra_alondra__fleet","mcp__plugin_alondra_alondra__ask"]}"#;
        assert!(stream.accept(node_only, "s").is_err());
        assert!(!stream.initialized);
        let orchestrator = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"plugin:alondra-orchestrator:alondra-orchestrator","status":"connected"}],"tools":["mcp__plugin_alondra-orchestrator_alondra-orchestrator__fleet","mcp__plugin_alondra-orchestrator_alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(orchestrator, "s").is_ok());
        assert!(stream.initialized);
    }
    #[test]
    fn orchestrator_init_requires_its_own_mcp_server_and_tools() {
        let mut stream = Stream::default();
        let node_only = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra","status":"connected"}],"tools":["mcp__alondra__fleet","mcp__alondra__ask"]}"#;
        assert!(stream.accept(node_only, "s").is_err());
        assert!(!stream.initialized);
        let orchestrator = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["mcp__alondra-orchestrator__fleet","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(orchestrator, "s").is_ok());
        assert!(stream.initialized);
    }
    #[test]
    fn restricted_init_rejects_shell_agent_and_foreign_mcp() {
        for extra in [
            r#""Bash""#,
            r#""Agent""#,
            r#""mcp__foreign__shell""#,
        ] {
            let mut stream = Stream { restricted: true, ..Stream::default() };
            let init = format!(r#"{{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{{"name":"alondra-orchestrator","status":"connected"}}],"tools":["Skill","Read","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask",{extra}]}}"#);
            assert!(stream.accept(init.as_bytes(), "s").is_err(), "{extra}");
        }
        let mut stream = Stream { restricted: true, ..Stream::default() };
        let foreign = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"},{"name":"foreign","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(foreign, "s").is_err());
        let mut stream = Stream { restricted: true, ..Stream::default() };
        let legacy_catalog = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__fleet","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(legacy_catalog, "s").is_err(), "a v2 invocation needs the v2 catalog");
        let generic_catalog = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__domain_command","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(generic_catalog, "s").is_err(), "a generic legacy tool is not a typed v2 catalog");
        let mut stream = Stream { restricted: true, ..Stream::default() };
        let v2 = br#"{"type":"system","subtype":"init","session_id":"s","permissionMode":"auto","mcp_servers":[{"name":"alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__alondra-orchestrator__create_task","mcp__alondra-orchestrator__ask"]}"#;
        assert!(stream.accept(v2, "s").unwrap());
    }
    #[test]
    fn ownership_is_exclusive() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let a = Ownership::acquire(&dir).unwrap().unwrap();
        assert!(Ownership::acquire(&dir).unwrap().is_none());
        drop(a);
        assert!(Ownership::acquire(&dir).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
    /// Holds a child between fork and exec, where it owns duplicates of every descriptor
    /// in this process, including other threads' close-on-exec lock descriptors.
    fn paused_fork() -> (std::thread::JoinHandle<()>, libc::c_int) {
        use std::os::unix::process::CommandExt;
        let (mut forked, mut release) = ([0; 2], [0; 2]);
        assert_eq!(unsafe { libc::pipe(forked.as_mut_ptr()) }, 0);
        assert_eq!(unsafe { libc::pipe(release.as_mut_ptr()) }, 0);
        let (forked_write, release_read) = (forked[1], release[0]);
        let child = std::thread::spawn(move || {
            let mut command = std::process::Command::new("/usr/bin/true");
            unsafe {
                command.pre_exec(move || {
                    let mut byte = 0u8;
                    libc::write(forked_write, (&raw const byte).cast(), 1);
                    libc::read(release_read, (&raw mut byte).cast(), 1);
                    Ok(())
                });
            }
            assert!(command.status().unwrap().success());
        });
        let mut byte = 0u8;
        assert_eq!(unsafe { libc::read(forked[0], (&raw mut byte).cast(), 1) }, 1);
        for fd in [forked[0], forked[1], release[0]] {
            unsafe { libc::close(fd) };
        }
        (child, release[1])
    }
    #[test]
    fn dropped_ownership_is_free_while_another_thread_is_forking() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let ownership = Ownership::acquire(&dir).unwrap().unwrap();
        let (child, release) = paused_fork();
        drop(ownership);
        let reacquired = Ownership::acquire(&dir).unwrap();
        unsafe {
            libc::write(release, [0u8].as_ptr().cast(), 1);
            libc::close(release);
        }
        child.join().unwrap();
        assert!(reacquired.is_some(), "a forked sibling must not keep a released ownership alive");
        std::fs::remove_dir_all(dir).unwrap();
    }
    fn fake(dir: &Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("claude");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn invocation<'a>(dir: &'a Path, exe: &'a Path, lock: &'a Ownership) -> Invocation<'a> {
        Invocation { executable: exe, cwd: dir, home: dir, plugins: vec![dir], domain: true, projects_root: None, capability: None, session: "test-session", resume: true, execution: "test-run", prompt: "work", ownership: lock }
    }
    #[tokio::test]
    async fn real_process_drains_stderr_and_checks_terminal_result() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let lock = Ownership::acquire(&dir).unwrap().unwrap();
        let exe = fake(&dir, r#"cat >/dev/null
printf '%s\n' '{"type":"system","subtype":"init","session_id":"test-session","permissionMode":"auto","mcp_servers":[{"name":"plugin:alondra-orchestrator:alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task","mcp__plugin_alondra-orchestrator_alondra-orchestrator__ask"]}'
i=0
while [ "$i" -lt 10000 ]; do printf 'bounded diagnostics\n' >&2; i=$((i+1)); done
printf '%s\n' '{"type":"assistant","message":{"usage":{"input_tokens":31}}}'
printf '%s\n' '{"type":"result","session_id":"test-session","subtype":"success","is_error":false}'"#);
        let mut starts = 0;
        let output = execute(invocation(&dir, &exe, &lock), |_| Ok(()), || { starts += 1; Ok(()) }, std::future::pending()).await;
        assert!(output.exit_ok);
        assert!(output.stream.success);
        assert_eq!(output.stream.input_tokens, Some(31));
        assert_eq!(starts, 1);
        assert_eq!(output.diagnostic.len(), MAX_DIAGNOSTIC);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn the_prompt_is_readable_the_moment_claude_starts() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let lock = Ownership::acquire(&dir).unwrap().unwrap();
        let exe = fake(&dir, r#"test -f /dev/stdin || exit 70
cat > received
printf '%s\n' '{"type":"result","session_id":"test-session","subtype":"success","is_error":false}'"#);
        let output = execute(invocation(&dir, &exe, &lock), |_| Ok(()), || Ok(()), std::future::pending()).await;
        assert!(output.exit_ok, "{}", output.diagnostic);
        assert_eq!(std::fs::read_to_string(dir.join("received")).unwrap(), "work");
        assert!(!std::fs::read_dir(&dir).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with("prompt-")),
            "the prompt leaves no file behind");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn cancelling_while_claude_forks_leaves_no_descendant_holding_the_lock() {
        for _ in 0..20 {
            let scratch_directory = crate::test_support::Directory::new();
            let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
            let lock = Ownership::acquire(&dir).unwrap().unwrap();
            let exe = fake(&dir, r#"cat >/dev/null
printf '%s\n' '{"type":"system","subtype":"init","session_id":"test-session","permissionMode":"auto","mcp_servers":[{"name":"plugin:alondra-orchestrator:alondra-orchestrator","status":"connected"}],"tools":["Skill","Read","mcp__plugin_alondra-orchestrator_alondra-orchestrator__create_task","mcp__plugin_alondra-orchestrator_alondra-orchestrator__ask"]}'
sleep 30 &
wait"#);
            let started = tokio::sync::Notify::new();
            let output = tokio::time::timeout(std::time::Duration::from_secs(25),
                execute(invocation(&dir, &exe, &lock), |_| Ok(()), || { started.notify_one(); Ok(()) }, started.notified())).await.unwrap();
            assert!(output.stream.error.unwrap().contains("cancelled"));
            drop(lock);
            // A sibling test forking at this moment holds a copy of the descriptor only until it execs.
            let released = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while Ownership::acquire(&dir).unwrap().is_none() { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
            }).await;
            assert!(released.is_ok(), "a child forked as the execution was cancelled kept the execution lock");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
    #[tokio::test]
    async fn malformed_output_kills_process_without_waiting_for_timeout() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let lock = Ownership::acquire(&dir).unwrap().unwrap();
        let exe = fake(&dir, "cat >/dev/null\nprintf 'invalid-json\\n'\nexec sleep 30");
        let output = tokio::time::timeout(std::time::Duration::from_secs(25), execute(invocation(&dir, &exe, &lock), |_| Ok(()), || Ok(()), std::future::pending())).await.unwrap();
        assert!(!output.exit_ok);
        assert!(output.stream.error.unwrap().contains("JSONL"));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn inherited_lock_survives_parent_ownership_drop() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let lock = Ownership::acquire(&dir).unwrap().unwrap();
        let exe = fake(&dir, "sleep 1");
        let mut child = invocation(&dir, &exe, &lock).command().spawn().unwrap();
        drop(lock);
        assert!(Ownership::acquire(&dir).unwrap().is_none(), "child must fence restarted daemon");
        child.wait().await.unwrap();
        assert!(Ownership::acquire(&dir).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn every_launch_including_resume_and_compaction_is_auto_and_unattended() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let lock = Ownership::acquire(&dir).unwrap().unwrap();
        for resume in [false, true] {
            let mut i = invocation(&dir, Path::new("claude"), &lock);
            i.resume = resume;
            i.prompt = "/compact";
            let c = i.command();
            let args: Vec<_> = c.as_std().get_args().map(|s| s.to_string_lossy().into_owned()).collect();
            assert!(args.windows(2).any(|a| a == ["--permission-mode", "auto"]));
            assert!(args.windows(2).any(|a| a == ["--permission-prompts", "none"]));
            assert!(args.windows(2).any(|a| a == [if resume { "--resume" } else { "--session-id" }, "test-session"]));
            assert!(!args.iter().any(|a| a == "--bg" || a == "--dangerously-skip-permissions"));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn orphan_cleanup_requires_dead_daemon_and_exact_child_identity() {
        let scratch_directory = crate::test_support::Directory::new();
        let dir = scratch_directory.parent().unwrap().join(uuid().unwrap());
        let lock = Ownership::acquire(&dir).unwrap().unwrap();
        let exe = fake(&dir, "exec sleep 30");
        let mut child = invocation(&dir, &exe, &lock).command().spawn().unwrap();
        ProcessOwner::record(&dir, child.id().unwrap(), "test-run").unwrap();
        assert!(terminate_orphan(&dir, Some("test-run")).unwrap_err().to_string().contains("live daemon"));
        assert!(child.try_wait().unwrap().is_none());
        let mut retired_daemon = tokio::process::Command::new("/bin/sh").args(["-c", "exit 0"]).spawn().unwrap();
        let retired_pid = retired_daemon.id().unwrap();
        retired_daemon.wait().await.unwrap();
        let mut owner: ProcessOwner = serde_json::from_slice(&std::fs::read(dir.join("owner.json")).unwrap()).unwrap();
        owner.daemon_pid = retired_pid;
        owner.daemon_started = 0;
        let actual_start = owner.child_started;
        owner.child_started += 1;
        std::fs::write(dir.join("owner.json"), serde_json::to_vec(&owner).unwrap()).unwrap();
        assert!(terminate_orphan(&dir, Some("test-run")).is_err());
        assert!(child.try_wait().unwrap().is_none());
        owner.child_started = actual_start;
        std::fs::write(dir.join("owner.json"), serde_json::to_vec(&owner).unwrap()).unwrap();
        assert!(terminate_orphan(&dir, Some("stale-run")).is_err());
        terminate_orphan(&dir, Some("test-run")).unwrap();
        assert!(!child.wait().await.unwrap().success());
        drop(lock);
        // Reaping the group leader can race with the kernel releasing the inherited
        // lock from the rest of its process group under a parallel test load.
        let reacquired = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(ownership) = Ownership::acquire(&dir).unwrap() {
                    break ownership;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("orphan process group did not release its execution lock");
        drop(reacquired);
        std::fs::remove_dir_all(dir).unwrap();
    }

}
