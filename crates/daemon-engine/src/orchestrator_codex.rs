use crate::domain_store::{BatchOwner, batch_effects::BatchReport};
use crate::orchestrator_process::{Output, ProcessOwner, Stream, bounded};
use crate::orchestrator_provider::Batch;
use alondra_agent_runtime::owned_host::VerifiedArtifact;
use alondra_provider_codex::{BatchHost, CodexConfiguration, HostEvent, McpServer, OrchestratorProfile};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const WAKE: Duration = Duration::from_millis(250);

pub(crate) async fn execute(batch: Batch, spawned: impl FnOnce() -> anyhow::Result<()> + Send + 'static, cancel: impl std::future::Future<Output=()>) -> Output {
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&cancelled);
    let mut running = tokio::task::spawn_blocking(move || run(batch, spawned, &stop));
    tokio::select! {
        biased;
        finished = &mut running => return finished.unwrap_or_else(|error| failed(format!("The Codex batch stopped: {error}"))),
        () = cancel => cancelled.store(true, Ordering::SeqCst),
    }
    let mut output = running.await.unwrap_or_else(|error| failed(format!("The Codex batch stopped: {error}")));
    output.stream.error = Some("Orchestrator execution was cancelled; reconcile its effects before retrying".into());
    output
}
fn failed(why: String) -> Output { Output { stream: Stream::default(), diagnostic: bounded(why), exit_ok: false } }

#[derive(Default)]
struct NativeTurn {
    thread: Option<String>,
    turn: Option<String>,
    consumed: bool,
    process: Option<String>,
    completed: bool,
    accepted_model: Option<String>,
    rerouted: bool,
    usage_baseline: Option<(i64, i64, i64)>,
}
impl NativeTurn {
    fn fold(&mut self, message: &Value, input: &str, session: &str, stream: &mut Stream) -> anyhow::Result<BatchReport> {
        let method = message["method"].as_str().unwrap_or("unknown");
        let params = &message["params"];
        let mut report = BatchReport { kind: bounded_public(method,128), ..BatchReport::default() };
        if method == "alondra/threadReady" {
            anyhow::ensure!(params["logicalConversationId"] == session, "The native thread belongs to a different batch");
            let thread = params["threadId"].as_str().filter(|id| !id.is_empty()).ok_or_else(|| anyhow::anyhow!("The native thread identity is missing"))?;
            anyhow::ensure!(self.thread.as_deref().is_none_or(|current| current == thread), "A batch cannot replace its native thread");
            if self.thread.is_none() { self.usage_baseline = Some((0, 0, 0)); }
            self.thread = Some(thread.into());
            if let Some(process) = native_label(params,"processGeneration") {
                anyhow::ensure!(self.process.as_deref().is_none_or(|current|current==process),"A batch cannot replace its native process");
                self.process=Some(process.into());
            }
            stream.initialized = true;
            return Ok(report);
        }
        if matches!(method, "alondra/accountBlocked" | "alondra/inputRefused") {
            anyhow::ensure!(!self.consumed && self.turn.is_none(), "A native policy refusal cannot undo an accepted turn");
            if method == "alondra/inputRefused" {
                anyhow::ensure!(params["inputId"] == input && params["nativeSubmitted"] == false, "A native refusal must positively identify its unsubmitted batch input");
            }
            let code = native_label(params, "code").ok_or_else(|| anyhow::anyhow!("The native policy refusal has no reason"))?;
            let explanation = match code {
                "native_account_identity_changed" => "Restore this session's original Codex account before retrying",
                "paid_authorization_required" => "This Codex account uses an API key and needs explicit payment authorization for a new node",
                "native_auth_mode_unsupported" => "This Codex authentication mode is unsupported; use a managed ChatGPT login",
                "native_model_provider_unsupported" | "native_provider_auth_unsupported" => "Restore standard OpenAI account configuration before retrying",
                _ => "Codex refused its native account policy; restore the original account configuration before retrying",
            };
            stream.error = Some(bounded(format!("{explanation} ({code})")));
            stream.denied=true;
            report.kind = "native_policy_refused".into();
            report.failure = stream.error.clone();
            return Ok(report);
        }
        if let Some(thread) = params["threadId"].as_str() {
            anyhow::ensure!(self.thread.as_deref() == Some(thread), "A Codex event belongs to a different thread");
        }
        match method {
            "alondra/inputAccepted" => {
                self.scope(params, false)?;
                anyhow::ensure!(params["inputId"] == input, "A native acceptance names a different batch input");
                let turn = params["turnId"].as_str().filter(|id| !id.is_empty()).ok_or_else(|| anyhow::anyhow!("The accepted turn identity is missing"))?;
                anyhow::ensure!(self.turn.as_deref().is_none_or(|current| current == turn), "The batch opened a second native turn");
                self.turn = Some(turn.into());
                if let Some(model) = native_label(params, "model") {
                    anyhow::ensure!(self.accepted_model.as_deref().is_none_or(|accepted| accepted == model), "The accepted native model changed without a reroute");
                    self.accepted_model = Some(model.into());
                    if !self.rerouted { stream.model = Some(model.into()); }
                }
            }
            "model/rerouted" => {
                self.scope(params, true)?;
                let from = native_label(params, "fromModel").ok_or_else(|| anyhow::anyhow!("The native reroute source model is missing"))?;
                let target = native_label(params, "toModel").ok_or_else(|| anyhow::anyhow!("The native reroute target model is missing"))?;
                let reason = native_label(params, "reason").ok_or_else(|| anyhow::anyhow!("The native reroute reason is missing"))?;
                if stream.model.as_deref() != Some(target) { self.usage_baseline = None; stream.window = None; stream.input_tokens = None; }
                stream.model = Some(target.into());
                self.rerouted = true;
                report.public_result = Some(json!({"from_model": from, "to_model": target, "reason": reason}).to_string());
            }
            "turn/started" => {
                let turn = params.pointer("/turn/id").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("The native turn identity is missing"))?;
                anyhow::ensure!(self.turn.as_deref().is_none_or(|current| current == turn), "The batch opened a second native turn");
                self.turn = Some(turn.into());
            }
            "item/started" | "item/completed" => {
                if let Some(turn) = params["turnId"].as_str() {
                    anyhow::ensure!(self.turn.as_deref().is_none_or(|current| current == turn), "The native item belongs to a different turn");
                    self.turn = Some(turn.into());
                }
                let item = &params["item"];
                match item["type"].as_str() {
                    Some("userMessage") if item["clientId"] == input => {
                        let thread=params["threadId"].as_str().filter(|id|!id.is_empty()).ok_or_else(||anyhow::anyhow!("Consumed input has no native thread identity"))?;
                        let turn=params["turnId"].as_str().filter(|id|!id.is_empty()).ok_or_else(||anyhow::anyhow!("Consumed input has no native turn identity"))?;
                        anyhow::ensure!(self.thread.as_deref()==Some(thread) && self.turn.as_deref()==Some(turn),"Consumed input belongs to a different native execution");
                        self.consumed = true;
                    }
                    Some("agentMessage") if method == "item/completed" => {
                        stream.note = item["text"].as_str().map(|text| bounded_public(text, 256 * 1024));
                        report.public_result = stream.note.clone();
                    }
                    Some("mcpToolCall") => {
                        let stopped_before_alondra = item["status"] == "failed" && !item["error"].is_null() && item["result"].is_null();
                        if stopped_before_alondra {
                            stream.denied = true;
                            stream.error = Some("Alondra refused a required permission; another provider cannot bypass that decision".into());
                        }
                        let tool = item["tool"].as_str().unwrap_or("unknown");
                        if alondra_core::orchestrator_batch::changes_something(tool) {
                            stream.acted = true;
                            if method == "item/started" { stream.alondra_calls.push(tool.into()); }
                        }
                    }
                    _ => {}
                }
            }
            "thread/tokenUsage/updated" => {
                self.scope(params, true)?;
                let usage = &params["tokenUsage"];
                stream.window = usage["modelContextWindow"].as_u64();
                if let Some(tokens) = usage.pointer("/last/inputTokens").and_then(Value::as_u64) {
                    stream.first_input_tokens.get_or_insert(tokens);
                    stream.input_tokens = Some(tokens);
                }
                self.measure(&usage["total"], stream)?;
            }
            "turn/completed" => {
                let thread=params["threadId"].as_str().filter(|id|!id.is_empty()).ok_or_else(||anyhow::anyhow!("The completed turn has no native thread identity"))?;
                let turn=params.pointer("/turn/id").and_then(Value::as_str).filter(|id|!id.is_empty()).ok_or_else(||anyhow::anyhow!("The completed turn has no native turn identity"))?;
                anyhow::ensure!(self.thread.as_deref()==Some(thread) && self.turn.as_deref()==Some(turn), "The completed turn is not this batch's turn");
                stream.success = params.pointer("/turn/status").and_then(Value::as_str) == Some("completed");
                anyhow::ensure!(!stream.success || self.consumed, "A successful terminal turn without positive input consumption cannot settle its batch");
                self.completed = true;
                stream.result = true;
                report.turn_completed = true;
                if !stream.success {
                    stream.native_request_rejected = params.pointer("/turn/status").and_then(Value::as_str) == Some("failed")
                        && matches!(params.pointer("/turn/error/codexErrorInfo").and_then(Value::as_str), Some("badRequest" | "contextWindowExceeded"));
                    failure(&params["turn"]["error"],stream);
                    if stream.error.is_none() { stream.error = Some(format!("The Codex turn ended with status {}",params["turn"]["status"])); }
                } else if !stream.denied && !stream.native_request_rejected {
                    stream.error = None;
                }
            }
            "error" => failure(&params["error"], stream),
            "alondra/transportFailed" => { stream.error = Some(format!("Codex transport failed: {}", params["code"])); }
            _ => {}
        }
        report.input_consumed = self.consumed;
        report.native_turn = self.turn.clone();
        report.failure = stream.error.clone();
        Ok(report)
    }

    fn scope(&self, params: &Value, require_turn: bool) -> anyhow::Result<()> {
        let thread = params["threadId"].as_str().filter(|id| !id.is_empty());
        anyhow::ensure!(thread.is_some() && thread == self.thread.as_deref(), "The native metadata belongs to a different or missing thread");
        if require_turn {
            let turn = params["turnId"].as_str().filter(|id| !id.is_empty());
            anyhow::ensure!(turn.is_some() && turn == self.turn.as_deref(), "The native metadata belongs to a different or missing turn");
        }
        Ok(())
    }

    fn measure(&mut self, total: &Value, stream: &mut Stream) -> anyhow::Result<()> {
        let (Some(input), Some(cached), Some(output)) = (total["inputTokens"].as_i64(), total["cachedInputTokens"].as_i64(), total["outputTokens"].as_i64()) else { return Ok(()) };
        if input < cached || cached < 0 || output < 0 || input.checked_add(output).is_none() { return Ok(()) }
        if total.get("cacheWriteInputTokens").is_some_and(|write| write.as_i64() != Some(0)) { self.usage_baseline = None; return Ok(()) }
        let current = (input - cached, cached, output);
        let before = self.usage_baseline.replace(current);
        let Some((before_input, before_cached, before_output)) = before else { return Ok(()) };
        let (Some(input), Some(cached), Some(output)) = (current.0.checked_sub(before_input), current.1.checked_sub(before_cached), current.2.checked_sub(before_output)) else { return Ok(()) };
        if input < 0 || cached < 0 || output < 0 { return Ok(()) }
        let Some(model) = stream.model.as_deref() else { return Ok(()) };
        if input == 0 && cached == 0 && output == 0 { return Ok(()) }
        let at = match stream.spent.iter().position(|usage| usage.model == model) {
            Some(at) => at,
            None => { stream.spent.push(alondra_core::metrics::Usage { model: model.into(), cost_micros: None, input: 0, output: 0, cache_read: 0, cache_write: 0 }); stream.spent.len() - 1 }
        };
        let usage = &mut stream.spent[at];
        let input = usage.input.checked_add(input).ok_or_else(|| anyhow::anyhow!("Native usage counters overflow"))?;
        let cached = usage.cache_read.checked_add(cached).ok_or_else(|| anyhow::anyhow!("Native usage counters overflow"))?;
        let output = usage.output.checked_add(output).ok_or_else(|| anyhow::anyhow!("Native usage counters overflow"))?;
        usage.input = input; usage.cache_read = cached; usage.output = output;
        Ok(())
    }
}
fn native_label<'a>(params: &'a Value, name: &str) -> Option<&'a str> {
    params[name].as_str().filter(|value| !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control))
}
fn bounded_public(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) { end -= 1; }
    text[..end].into()
}
fn failure(error: &Value, stream: &mut Stream) {
    if error.is_null() { return; }
    let kind = error["codexErrorInfo"].as_str();
    stream.limited |= kind == Some("usageLimitExceeded");
    let message = error["message"].as_str().unwrap_or("The native turn failed");
    let guidance = match kind {
        Some("badRequest") => " Check this batch's selected model, reasoning effort and input before explicitly retrying; its catalog does not prove request acceptance.",
        Some("contextWindowExceeded") => " This request exceeds the model's context window. Reduce the next batch's input or explicitly choose a compatible model before retrying.",
        _ => "",
    };
    stream.error = Some(bounded(format!("{message}{guidance}")));
}

fn run(batch: Batch, spawned: impl FnOnce() -> anyhow::Result<()>, cancelled: &AtomicBool) -> Output {
    let configuration = match configuration(&batch) { Ok(value) => value, Err(error) => return failed(format!("{error:#}")) };
    let profile = match profile(&batch) { Ok(value) => value, Err(error) => return failed(format!("{error:#}")) };
    let run = batch.owner.run().to_owned();
    let mut host = match BatchHost::start(batch.owner, &configuration, &batch.session, &profile, &batch.prompt) {
        Ok(value) => value, Err(error) => return failed(format!("The Codex host did not confirm the batch offer: {error:?}")),
    };
    let _host = match crate::domain_orchestrator::HostGuard::attach(Arc::clone(&batch.domain), &run, host.stopper()) {
        Ok(guard) => guard,
        Err(error) => { stop(host, &batch.directory); return failed(format!("Cannot own the orchestrator host: {error:#}")); }
    };
    let mut output = Output { stream: Stream::for_input(host.input()), diagnostic: String::new(), exit_ok: false };
    let mut native = NativeTurn::default();
    if let Err(error) = ProcessOwner::record(&batch.directory, host.process_id(), &run).and_then(|()| spawned()) {
        output.stream.storage_failure=true;
        output.stream.error = Some(bounded(format!("Cannot record the Codex host: {error:#}")));
        stop(host, &batch.directory);
        return output;
    }
    loop {
        if cancelled.load(Ordering::SeqCst) { break; }
        match host.readable(WAKE) {
            Ok(false) => continue, Ok(true) => {},
            Err(error) => { output.stream.error.get_or_insert_with(|| format!("The Codex host disconnected: {error:?}")); break; }
        }
        let pending = match host.next_pending() {
            Ok(value) => value,
            Err(error) => { output.stream.error.get_or_insert_with(|| format!("The Codex host stopped: {error:?}")); break; }
        };
        let mut exited = false;
        let report = match pending.event {
            HostEvent::Sdk(message) => native.fold(&message, host.input(), &batch.session, &mut output.stream),
            HostEvent::Diagnostic { text, .. } => Ok(output.diagnosed(&text)),
            HostEvent::UnexpectedDecision => { exited = true; Ok(output.unexpected_decision()) }
            HostEvent::Exited { code, signal } => {
                output.exit_ok = code == Some(0) || (native.consumed && output.stream.result && output.stream.success && signal.as_deref() == Some("SIGTERM"));
                exited = true;
                Ok(BatchReport { kind: "process_exited".into(), input_consumed: native.consumed, turn_completed: output.stream.result, native_turn: native.turn.clone(), failure: output.stream.error.clone(), public_result: None, ..Default::default() })
            }
        };
        let mut report = match report { Ok(value) => value, Err(error) => {
            output.stream.error = Some(bounded(error.to_string()));
            BatchReport { kind: "invalid_native_transition".into(), failure: output.stream.error.clone(), ..BatchReport::default() }
        }};
        report.stop = if output.stream.denied { Some(alondra_core::orchestrator_batch::BatchStop::Denied) }
            else if output.stream.native_request_rejected { Some(alondra_core::orchestrator_batch::BatchStop::Interrupted) }
            else { None };
        if let Err(error) = batch.domain.publish_batch_report(&run, pending.cursor, &pending.evidence, &report) {
            output.stream.storage_failure=true;
            output.stream.error = Some(bounded(format!("The batch report was not committed; its host evidence remains unacknowledged: {error:#}")));
            break;
        }
        #[cfg(any(test, feature = "test-hooks"))]
        if let Err(error) = alondra_daemon_engine_model::faults::write(&batch.home, "ack") {
            output.stream.storage_failure = true;
            output.stream.error = Some(format!("The batch ACK could not be written: {error}"));
            break;
        }
        if let Err(error) = host.acknowledge(pending.cursor) {
            output.stream.storage_failure=true;
            output.stream.error = Some(format!("The batch ACK is uncertain: {error:?}"));
            break;
        }
        if exited || matches!(report.kind.as_str(), "invalid_native_transition" | "native_policy_refused") { break; }
    }
    stop(host, &batch.directory);
    output
}
fn stop(host: BatchHost<BatchOwner>, directory: &Path) {
    match host.stop() {
        Ok(())=>{let _=std::fs::remove_file(directory.join("owner.json"));}
        Err(error)=>log!("orchestrator: Codex did not stop cleanly; its process owner record is retained: {error:?}"),
    }
}
fn configuration(batch: &Batch) -> anyhow::Result<CodexConfiguration> {
    use std::os::unix::fs::DirBuilderExt;
    let backend = batch.backend.for_provider(alondra_core::accounts::Provider::Codex, &batch.home)?.for_start()?
        .for_account_provider(&batch.home, batch.selection.account.provider, &batch.selection.account.account, &crate::accounts::registered_provider(&batch.home, batch.selection.account.provider))?;
    std::fs::create_dir_all(&backend.state_root)?;
    let state_directory = backend.state_root.join(format!("orchestrator-{}", crate::auth::random_hex(8)));
    std::fs::DirBuilder::new().mode(0o700).create(&state_directory)?;
    let mut native_environment = backend.native_environment.clone();
    native_environment.extend(BTreeMap::from([
        ("ALONDRA_HOME".into(), batch.home.display().to_string()),
        ("ALONDRA_ORCHESTRATOR".into(), "1".into()),
        ("ALONDRA_PROVIDER".into(), "codex".into()),
        ("ALONDRA_ORCHESTRATOR_EXECUTION".into(), batch.owner.run().into()),
        ("ALONDRA_PROVIDER".into(), "codex".into()),
        ("ALONDRA_SESSION_ID".into(), alondra_core::orchestrator::NODE_ID.into()),
        ("ALONDRA_DOMAIN_CAPABILITY".into(), batch.capability.clone()),
        ("ALONDRA_DOMAIN_PROTOCOL".into(), "2".into()),
    ]));
    for name in ["ALONDRA_PORT", "ALONDRA_SANDBOX"] {
        if !native_environment.contains_key(name) && let Ok(value) = std::env::var(name) { native_environment.insert(name.into(), value); }
    }
    Ok(CodexConfiguration {
        artifact: VerifiedArtifact::verify(&backend.node, &backend.host, &backend.node_sha256, &backend.host_sha256)?,
        state_directory, host_environment: backend.host_environment, native_executable: backend.native,
        native_directory: batch.directory.clone(), native_environment, profile_id: "alondra".into(),
        account_id: batch.selection.account.account.as_str().to_owned(), host_release: env!("CARGO_PKG_VERSION").into(),
        io_timeout: Duration::from_secs(backend.io_timeout_seconds), confinement: backend.confinement, worker: None,
    })
}
fn profile(batch: &Batch) -> anyhow::Result<OrchestratorProfile> {
    let instructions = std::fs::read_to_string(batch.orchestrator_plugin.join("skills/alondra-orchestrator/SKILL.md"))?;
    anyhow::ensure!(!instructions.trim().is_empty() && instructions.len() <= 256 * 1024, "The orchestrator skill is missing or too large");
    Ok(OrchestratorProfile {
        model: batch.selection.model.model.model.clone(), reasoning_effort: batch.selection.model.reasoning_effort.clone(),
        tools: vec![], disallowed_tools: vec![], plugins: vec![],
        mcp_servers: BTreeMap::from([("alondra-orchestrator".into(), McpServer { command: batch.node_plugin.join("bin/alondra"), args: vec!["mcp-orchestrator".into()] })]),
        settings: json!({"codex":{"developerInstructions": instructions,"nativeAccountPolicy":{"expectedEmail":batch.selection.account_status.as_ref().and_then(|account|account.email.as_deref()),"expectedWorkspaceId":batch.selection.account_status.as_ref().and_then(|account|account.native_identity.as_deref())}}}).as_object().unwrap().clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(native: &mut NativeTurn, stream: &mut Stream) {
        native.fold(&json!({"method":"alondra/threadReady","params":{"threadId":"native","logicalConversationId":"batch","processGeneration":"process"}}),"input","batch",stream).unwrap();
        native.fold(&json!({"method":"alondra/inputAccepted","params":{"threadId":"native","turnId":"turn","inputId":"input","model":"advertised-model"}}),"input","batch",stream).unwrap();
    }

    #[test]
    fn only_a_correlated_terminal_failed_turn_parks_a_native_request_rejection() {
        for kind in ["badRequest", "contextWindowExceeded"] {
            let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input"); ready(&mut native, &mut stream);
            native.fold(&json!({"method":"error","params":{"threadId":"native","turnId":"turn","willRetry":true,"error":{"codexErrorInfo":kind,"message":"The native request is being retried"}}}), "input", "batch", &mut stream).unwrap();
            assert!(!stream.native_request_rejected && !stream.result, "a nonterminal error is not a terminal rejection");
            let wrong = json!({"method":"turn/completed","params":{"threadId":"other","turn":{"id":"turn","status":"failed","error":{"codexErrorInfo":kind}}}});
            assert!(native.fold(&wrong,"input","batch",&mut stream).is_err()); assert!(!stream.native_request_rejected);
            native.fold(&json!({"method":"turn/completed","params":{"threadId":"native","turn":{"id":"turn","status":"failed","error":{"codexErrorInfo":kind,"message":"The native request was rejected"}}}}), "input", "batch", &mut stream).unwrap();
            assert!(stream.native_request_rejected && stream.result && !stream.denied && !stream.limited);

            let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input"); ready(&mut native, &mut stream);
            native.fold(&json!({"method":"error","params":{"threadId":"native","turnId":"turn","willRetry":true,"error":{"codexErrorInfo":kind,"message":"A temporary native error"}}}), "input", "batch", &mut stream).unwrap();
            native.fold(&json!({"method":"item/started","params":{"threadId":"native","turnId":"turn","item":{"type":"userMessage","clientId":"input"}}}), "input", "batch", &mut stream).unwrap();
            native.fold(&json!({"method":"turn/completed","params":{"threadId":"native","turn":{"id":"turn","status":"completed"}}}), "input", "batch", &mut stream).unwrap();
            assert!(!stream.native_request_rejected && stream.error.is_none());
            assert!(matches!(stream.end(true,""),crate::orchestrator_process::BatchEnd::Handled));
        }
    }

    #[test]
    fn native_policy_refusal_is_terminal_and_preserves_positive_no_submission_evidence() {
        let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input");
        let refused = json!({"method":"alondra/inputRefused","params":{"inputId":"input","nativeSubmitted":false,"code":"paid_authorization_required"}});
        let report = native.fold(&refused,"input","batch",&mut stream).unwrap();
        assert_eq!(report.kind,"native_policy_refused"); assert!(report.failure.as_ref().unwrap().contains("explicit payment authorization"));
        assert!(!native.consumed && !report.input_consumed && !stream.result && !stream.limited);assert!(stream.denied,"an account policy denial cannot trigger provider retry");
        for invalid in [json!({"inputId":"other","nativeSubmitted":false,"code":"paid_authorization_required"}),json!({"inputId":"input","nativeSubmitted":true,"code":"paid_authorization_required"})] {
            assert!(NativeTurn::default().fold(&json!({"method":"alondra/inputRefused","params":invalid}),"input","batch",&mut Stream::for_input("input")).is_err());
        }
    }
    #[test]
    fn accepted_batch_without_correlated_consumption_cannot_settle_the_inbox() {
        let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input");
        ready(&mut native,&mut stream);
        assert!(!native.consumed);
        let completed = json!({"method":"turn/completed","params":{"threadId":"native","turn":{"id":"turn","status":"completed"}}});
        assert!(native.fold(&completed,"input","batch",&mut stream).is_err());
        assert!(!stream.result);
        native.fold(&json!({"method":"item/started","params":{"threadId":"native","turnId":"turn","item":{"type":"userMessage","clientId":"other"}}}),"input","batch",&mut stream).unwrap();
        assert!(!native.consumed);
        native.fold(&json!({"method":"item/started","params":{"threadId":"native","turnId":"turn","item":{"type":"userMessage","clientId":"input"}}}),"input","batch",&mut stream).unwrap();
        let report = native.fold(&completed,"input","batch",&mut stream).unwrap();
        assert!(report.input_consumed && report.turn_completed && stream.success);
    }
    #[test]
    fn batch_history_excludes_private_reasoning_and_native_quota_is_not_permission_to_retry_a_denial() {
        let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input");
        ready(&mut native,&mut stream);
        let reasoning = native.fold(&json!({"method":"item/completed","params":{"threadId":"native","turnId":"turn","item":{"type":"reasoning","text":"private reasoning"}}}),"input","batch",&mut stream).unwrap();
        assert!(reasoning.public_result.is_none()); assert!(stream.note.is_none());
        native.fold(&json!({"method":"item/completed","params":{"threadId":"native","turnId":"turn","item":{"type":"mcpToolCall","tool":"launch","status":"failed","error":{"message":"stopped before reaching the server"}}}}),"input","batch",&mut stream).unwrap();
        assert!(stream.denied);
        native.fold(&json!({"method":"error","params":{"threadId":"native","error":{"codexErrorInfo":"usageLimitExceeded","message":"Quota exhausted"}}}),"input","batch",&mut stream).unwrap();
        assert!(stream.limited && stream.denied);
    }
    #[test]
    fn a_tool_call_alondra_answered_is_never_a_denial_and_the_batch_ends_handled() {
        for arguments in [json!({"interaction_id":"card"}), json!({"reason":"forbidden: permission denied, user denied"})] {
            let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input");
            ready(&mut native,&mut stream);
            native.fold(&json!({"method":"item/started","params":{"threadId":"native","turnId":"turn","item":{"type":"userMessage","clientId":"input"}}}),"input","batch",&mut stream).unwrap();
            let refusal = json!({"content":[{"type":"text","text":"Alondra error: Alondra refused it [forbidden]: Only a card's author withdraws it"}],"isError":true});
            native.fold(&json!({"method":"item/completed","params":{"threadId":"native","turnId":"turn","item":{"type":"mcpToolCall","tool":"withdraw","status":"failed","arguments":arguments,"result":refusal,"error":null}}}),"input","batch",&mut stream).unwrap();
            assert!(!stream.denied && stream.error.is_none(), "Alondra answered the call: {:?}", stream.error);
            native.fold(&json!({"method":"turn/completed","params":{"threadId":"native","turn":{"id":"turn","status":"completed"}}}),"input","batch",&mut stream).unwrap();
            assert!(matches!(stream.end(true,""), crate::orchestrator_process::BatchEnd::Handled));
        }
    }
    #[test]
    fn native_request_and_context_rejections_do_not_exhaust_a_batch_account() {
        for (kind, guidance) in [("badRequest", "catalog does not prove request acceptance"), ("contextWindowExceeded", "Reduce the next batch's input")] {
            let mut stream = Stream::for_input("input");
            failure(&json!({"codexErrorInfo":kind,"message":"Native request rejected"}), &mut stream);
            assert!(!stream.limited && !stream.success && !stream.denied);
            let error = stream.error.as_deref().unwrap(); assert!(error.starts_with("Native request rejected")); assert!(error.contains(guidance));
        }
    }
    #[test]
    fn only_the_pinned_native_account_limit_error_marks_a_batch_exhausted() {
        let mut stream = Stream::for_input("input");
        failure(&json!({"codexErrorInfo":"usageLimitExceeded","message":"Account quota exhausted"}), &mut stream);
        assert!(stream.limited);
        for kind in ["rateLimitExceeded", "sessionBudgetExceeded", "contextWindowExceeded", "usageLimitReached"] {
            let mut stream = Stream::for_input("input");
            failure(&json!({"codexErrorInfo":kind,"message":"Native error"}), &mut stream);
            assert!(!stream.limited, "{kind} is not evidence of an account quota limit");
        }
    }
    #[test]
    fn codex_subscription_tokens_have_no_invented_price_and_partial_updates_preserve_usage() {
        let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input");
        stream.model = Some("advertised-model".into()); ready(&mut native,&mut stream);
        native.fold(&json!({"method":"thread/tokenUsage/updated","params":{"threadId":"native","turnId":"turn","tokenUsage":{"modelContextWindow":32000,"last":{"inputTokens":100,"totalTokens":125},"total":{"inputTokens":100,"cachedInputTokens":40,"outputTokens":25}}}}),"input","batch",&mut stream).unwrap();
        assert_eq!(stream.first_input_tokens,Some(100)); assert_eq!(stream.window,Some(32000));
        assert_eq!(stream.spent[0].tokens(),125); assert_eq!(stream.spent[0].cost_micros,None);
        let previous = stream.spent.clone();
        native.fold(&json!({"method":"thread/tokenUsage/updated","params":{"threadId":"native","turnId":"turn","tokenUsage":{"total":{"inputTokens":100}}}}),"input","batch",&mut stream).unwrap();
        assert_eq!(stream.spent,previous);
    }
    #[test]
    fn missing_native_thread_or_turn_cannot_consume_or_settle_a_batch() {
        for params in [
            json!({"item":{"type":"userMessage","clientId":"input"}}),
            json!({"threadId":"native","item":{"type":"userMessage","clientId":"input"}}),
            json!({"threadId":"native","turnId":"","item":{"type":"userMessage","clientId":"input"}}),
        ] {
            let mut native=NativeTurn::default();let mut stream=Stream::for_input("input");ready(&mut native,&mut stream);
            assert!(native.fold(&json!({"method":"item/completed","params":params}),"input","batch",&mut stream).is_err());
            assert!(!native.consumed && !stream.result);
        }
        let mut native=NativeTurn::default();let mut stream=Stream::for_input("input");ready(&mut native,&mut stream);
        native.consumed=true;
        assert!(native.fold(&json!({"method":"turn/completed","params":{"turn":{"status":"completed"}}}),"input","batch",&mut stream).is_err());
        assert!(!stream.result);
    }


    fn sample(native: &mut NativeTurn, stream: &mut Stream, input: i64, output: i64) {
        native.fold(&json!({"method":"thread/tokenUsage/updated","params":{"threadId":"native","turnId":"turn","tokenUsage":{"total":{"inputTokens":input,"cachedInputTokens":0,"outputTokens":output}}}}),"input","batch",stream).unwrap();
    }
    fn routed() -> Value { json!({"method":"model/rerouted","params":{"threadId":"native","turnId":"turn","fromModel":"advertised-model","toModel":"actual-target","reason":"highRiskCyberActivity"}}) }

    #[test]
    fn native_acceptance_and_rerouting_report_actual_models_without_relabeling_earlier_usage() {
        let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input"); ready(&mut native, &mut stream);
        assert_eq!(stream.model.as_deref(), Some("advertised-model"));
        sample(&mut native, &mut stream, 100, 10);
        let report = native.fold(&routed(), "input", "batch", &mut stream).unwrap();
        assert_eq!(stream.model.as_deref(), Some("actual-target"));
        assert_eq!(serde_json::from_str::<Value>(report.public_result.as_deref().unwrap()).unwrap()["reason"], "highRiskCyberActivity");
        sample(&mut native, &mut stream, 200, 20);
        assert_eq!(stream.spent.len(), 1, "the interval crossing the reroute is uncertain");
        sample(&mut native, &mut stream, 220, 22);
        native.fold(&routed(), "input", "batch", &mut stream).unwrap();
        sample(&mut native, &mut stream, 230, 23);
        assert_eq!(stream.spent.iter().map(|usage| (usage.model.as_str(), usage.input, usage.output, usage.cost_micros)).collect::<Vec<_>>(),
            [("advertised-model", 100, 10, None), ("actual-target", 30, 3, None)]);
        assert!(!stream.limited && !stream.denied && !native.consumed, "model metadata grants no quota, permission or input consumption");
    }

    #[test]
    fn metadata_for_another_or_missing_thread_or_turn_cannot_change_the_effective_model_or_metrics() {
        let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input"); ready(&mut native, &mut stream); sample(&mut native, &mut stream, 100, 10);
        for (field, value) in [("threadId", json!("other")), ("turnId", json!("other")), ("threadId", Value::Null), ("turnId", Value::Null)] {
            let mut event = routed(); event["params"][field] = value;
            assert!(native.fold(&event, "input", "batch", &mut stream).is_err());
        }
        assert_eq!(stream.model.as_deref(), Some("advertised-model"));
        sample(&mut native, &mut stream, 110, 11);
        assert_eq!(stream.spent[0].input, 110);
        assert_eq!(stream.spent.len(), 1);
    }

    #[test]
    fn native_reroute_before_delayed_acceptance_is_not_overwritten_by_its_configured_model() {
        let mut native = NativeTurn::default(); let mut stream = Stream::for_input("input");
        native.fold(&json!({"method":"alondra/threadReady","params":{"threadId":"native","logicalConversationId":"batch","processGeneration":"process"}}),"input","batch",&mut stream).unwrap();
        native.fold(&json!({"method":"turn/started","params":{"threadId":"native","turn":{"id":"turn"}}}),"input","batch",&mut stream).unwrap();
        native.fold(&routed(), "input", "batch", &mut stream).unwrap();
        native.fold(&json!({"method":"alondra/inputAccepted","params":{"threadId":"native","turnId":"turn","inputId":"input","model":"advertised-model"}}),"input","batch",&mut stream).unwrap();
        assert_eq!(stream.model.as_deref(), Some("actual-target"));
        sample(&mut native, &mut stream, 100, 10); sample(&mut native, &mut stream, 120, 12);
        assert_eq!(stream.spent.iter().map(|usage| (usage.model.as_str(), usage.input)).collect::<Vec<_>>(), [("actual-target",20)]);
    }


    #[test]
    fn native_non_account_errors_with_legacy_limit_text_cannot_exhaust_the_account() {
        for kind in ["sessionBudgetExceeded", "rateLimitExceeded", "contextWindowExceeded"] {
            let mut stream = Stream::for_input("input");
            failure(&json!({"codexErrorInfo":kind,"message":"You've hit your session limit"}), &mut stream);
            assert_eq!(stream.limit(), None, "{kind} has no native account-quota evidence");
            let output = Output { stream, diagnostic: "You've hit your weekly limit".into(), exit_ok: false };
            assert!(!matches!(output.end(), crate::orchestrator_process::BatchEnd::Limited { .. }));
        }
        let early = failed("You've hit your session limit before a native host was confirmed".into());
        assert_eq!(early.stream.limit(), None);
        assert!(!matches!(early.end(), crate::orchestrator_process::BatchEnd::Limited { .. }));
        let mut stream = Stream::for_input("input");
        failure(&json!({"codexErrorInfo":"usageLimitExceeded","message":"Native account quota exhausted"}), &mut stream);
        assert_eq!(stream.limit(), Some(None));
        assert!(matches!(Output { stream, diagnostic: String::new(), exit_ok: false }.end(), crate::orchestrator_process::BatchEnd::Limited { .. }));
    }

}

#[cfg(test)]
mod policy_integration{
    use super::*;
    use alondra_core::accounts::{AccountId,AccountKey,AccountState,AccountStatus,Provider};
    use alondra_core::domain::{Actor,Command};
    use alondra_core::model_selection::{AccountModelCatalog,CatalogModel};
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn a_normal_codex_batch_persists_its_native_exit_without_a_storage_failure() {
        let mode = "complete";
        let f=crate::domain_orchestrator::testing::fixture();
        let key=AccountKey::native(Provider::Codex);
        let text=r#"[accounts]
connected=["codex"]
[model_profiles.codex.orchestrator]
mode="ordered_candidates"
candidates=[{model="fixture-model",reasoning_effort="high"}]
"#;
        std::fs::write(alondra_core::options::path_in(&f.executor.home),text).unwrap();
        let options=alondra_core::options::OptionsFile::parse(text).unwrap();
        f.run(Actor::System,"codex-policy-roster",Command::RecordAccounts{accounts:vec![AccountStatus{provider:Provider::Codex,id:AccountId::default_account(),name:"Native fixture".into(),state:AccountState::SignedIn,native_identity:Some("fixture-workspace".into()),email:Some("first@example.test".into()),plan:None,billing:Default::default(),sign_ins: 0, profile: None}],reserve:alondra_core::usage::default_reserve()});
        f.run(Actor::System,"codex-policy-order",Command::RecordAccountConfiguration{order:vec![key.clone()],version:"codex-policy-fixture".into()});
        let version=alondra_core::model_selection::profiles_version(&options.model_profiles);
        f.run(Actor::System,"codex-policy-profiles",Command::RecordModelProfiles{profiles:options.model_profiles,version});
        f.run(Actor::System,"codex-policy-catalog",Command::RecordModelCatalog{catalog:AccountModelCatalog{account:key,revision:"codex-policy-models".into(),models:vec![CatalogModel{model:"fixture-model".into(),display_name:"Native fixture".into(),reasoning_efforts:vec!["high".into()],default_reasoning_effort:Some("high".into()),context_window:None,input_modalities:vec!["text".into()],is_default:true,hidden:false}],native_default:Some("fixture-model".into()),measured_at:1}});
        let skill=f.root.join("assets").join(crate::plugins::ORCHESTRATOR).join("skills/alondra-orchestrator");std::fs::create_dir_all(&skill).unwrap();std::fs::write(skill.join("SKILL.md"),"You are the Alondra orchestrator. Use only Alondra role tools.").unwrap();
        let native=f.root.join("codex-native");let fake=std::fs::canonicalize(std::env::var("FIXTURE_CODEX_NATIVE").unwrap()).unwrap();std::fs::copy(fake,&native).unwrap();std::fs::set_permissions(&native,std::fs::Permissions::from_mode(0o700)).unwrap();
        let trace=f.root.join("codex-transcript");let mut backend=f.domain().sdk().unwrap().0.clone();
        let host=std::fs::canonicalize(std::env::var("FIXTURE_CODEX_HOST").unwrap()).unwrap();backend.provider=Provider::Codex;backend.origin=None;backend.node=host.clone();backend.host=host;backend.node_sha256=crate::sdk_owner::backend::digest(&backend.node).unwrap();backend.host_sha256=backend.node_sha256.clone();backend.native=native;backend.native_sha256=crate::sdk_owner::backend::digest(&backend.native).unwrap();backend.native_environment.insert("ALONDRA_FAKE_MODE".into(),mode.into());backend.native_environment.insert("ALONDRA_FAKE_TRANSCRIPT".into(),trace.display().to_string());
        backend.native_environment.insert("ALONDRA_FAKE_FINAL_TEXT".into(), "Normal Codex batch handled".into());
        std::fs::write(f.executor.home.join("codex-backend.json"),serde_json::to_vec(&backend).unwrap()).unwrap();
        let state_root = backend.state_root.clone();
        f.tell("normal-codex-request", "Finish this batch through its normal native result");
        f.tick().await;
        let runtime = crate::domain_orchestrator::load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none(), "{runtime:?}");
        assert!(runtime.failure.is_none() && runtime.last_error.is_none(), "{runtime:?}");
        assert_eq!(runtime.note.as_deref(), Some("Normal Codex batch handled"), "the normal Handled path retains its native result; storage recovery does not set this note");
        assert_eq!(f.pending(), 0);
        assert_eq!(f.attempts(), ["handed"]);
        assert!(f.domain().state().unwrap().requests[&"normal-codex-request".into()].closed);
        let run = runtime.last_run_id.as_deref().unwrap();
        let directory = std::fs::read_dir(&state_root).unwrap().map(|entry| entry.unwrap().path())
            .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("orchestrator-")).unwrap();
        let mut reader = alondra_codex_protocol::FramedReader::new(std::fs::File::open(directory.join("outbox")).unwrap());
        let mut terminal = None;
        while let Some(frame) = reader.read_received().unwrap() {
            match &frame.frame().body {
                alondra_codex_protocol::Body::Event(event) if matches!(event.observation, alondra_codex_protocol::Observation::ProcessExited { .. }) => terminal = Some(event.cursor),
                _ => {}
            }
        }
        let terminal = terminal.expect("the normal native exit remains durable");
        let connection = rusqlite::Connection::open(crate::domain_database_migration::active_path(&f.executor.home)).unwrap();
        let report: String = connection.query_row("SELECT report FROM orchestrator_batch_reports WHERE run=?1 AND cursor=?2", rusqlite::params![run, i64::try_from(terminal).unwrap()], |row| row.get(0)).unwrap();
        let report: Value = serde_json::from_str(&report).unwrap();
        assert_eq!(report["kind"], "process_exited");
        assert_eq!(report["input_consumed"], true);
        assert_eq!(report["turn_completed"], true);
        assert!(report["failure"].is_null());
        assert!(!f.executor.home.join("runtime/orchestrator/owner.json").exists());
    }

    #[tokio::test]
    async fn actual_native_request_rejections_park_the_executor_without_retry_or_account_rotation() {
        for mode in ["bad_request", "context_exceeded"] {
            let f=crate::domain_orchestrator::testing::fixture();
            let key=AccountKey::native(Provider::Codex);
            let text=r#"[accounts]
connected=["codex"]
[model_profiles.codex.orchestrator]
mode="ordered_candidates"
candidates=[{model="fixture-model",reasoning_effort="high"}]
"#;
            std::fs::write(alondra_core::options::path_in(&f.executor.home),text).unwrap();
            let options=alondra_core::options::OptionsFile::parse(text).unwrap();
            f.run(Actor::System,"codex-policy-roster",Command::RecordAccounts{accounts:vec![AccountStatus{provider:Provider::Codex,id:AccountId::default_account(),name:"Native fixture".into(),state:AccountState::SignedIn,native_identity:Some("fixture-workspace".into()),email:Some("first@example.test".into()),plan:None,billing:Default::default(),sign_ins: 0, profile: None}],reserve:alondra_core::usage::default_reserve()});
            f.run(Actor::System,"codex-policy-order",Command::RecordAccountConfiguration{order:vec![key.clone()],version:"codex-policy-fixture".into()});
            let version=alondra_core::model_selection::profiles_version(&options.model_profiles);
            f.run(Actor::System,"codex-policy-profiles",Command::RecordModelProfiles{profiles:options.model_profiles,version});
            f.run(Actor::System,"codex-policy-catalog",Command::RecordModelCatalog{catalog:AccountModelCatalog{account:key,revision:"codex-policy-models".into(),models:vec![CatalogModel{model:"fixture-model".into(),display_name:"Native fixture".into(),reasoning_efforts:vec!["high".into()],default_reasoning_effort:Some("high".into()),context_window:None,input_modalities:vec!["text".into()],is_default:true,hidden:false}],native_default:Some("fixture-model".into()),measured_at:1}});
            let skill=f.root.join("assets").join(crate::plugins::ORCHESTRATOR).join("skills/alondra-orchestrator");std::fs::create_dir_all(&skill).unwrap();std::fs::write(skill.join("SKILL.md"),"You are the Alondra orchestrator. Use only Alondra role tools.").unwrap();
            let native=f.root.join("codex-native");let fake=std::fs::canonicalize(std::env::var("FIXTURE_CODEX_NATIVE").unwrap()).unwrap();std::fs::copy(fake,&native).unwrap();std::fs::set_permissions(&native,std::fs::Permissions::from_mode(0o700)).unwrap();
            let trace=f.root.join("codex-transcript");let mut backend=f.domain().sdk().unwrap().0.clone();
            let host=std::fs::canonicalize(std::env::var("FIXTURE_CODEX_HOST").unwrap()).unwrap();backend.provider=Provider::Codex;backend.origin=None;backend.node=host.clone();backend.host=host;backend.node_sha256=crate::sdk_owner::backend::digest(&backend.node).unwrap();backend.host_sha256=backend.node_sha256.clone();backend.native=native;backend.native_sha256=crate::sdk_owner::backend::digest(&backend.native).unwrap();backend.native_environment.insert("ALONDRA_FAKE_MODE".into(),mode.into());backend.native_environment.insert("ALONDRA_FAKE_TRANSCRIPT".into(),trace.display().to_string());
            std::fs::write(f.executor.home.join("codex-backend.json"),serde_json::to_vec(&backend).unwrap()).unwrap();
            let accounts = f.domain().state().unwrap().accounts.clone();
            let usage = f.domain().state().unwrap().account_usage.clone();
            f.tell("rejected-request", "Run this request only after correcting its native rejection");
            f.tick().await;
            let runtime = crate::domain_orchestrator::load(&f.executor.home).unwrap();
            assert_eq!(runtime.failure, Some(crate::domain_orchestrator::Failure::Interrupted), "{:?}", runtime.last_error);
            assert!(runtime.retry_at.is_none(), "a precise request rejection must never schedule another automatic attempt");
            assert!(runtime.active_run.is_some(), "the existing actionable interruption remains pending");
            assert!(runtime.batch_handoffs.is_empty(), "request rejection cannot trigger quota account rotation");
            assert_eq!(f.domain().state().unwrap().accounts, accounts); assert_eq!(f.domain().state().unwrap().account_usage, usage);
            let run = runtime.last_run_id.clone();
            f.tick().await;
            let after = crate::domain_orchestrator::load(&f.executor.home).unwrap(); assert_eq!(after.last_run_id, run); assert!(after.retry_at.is_none());
            let events: Vec<Value> = std::fs::read_to_string(&trace).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
            assert_eq!(events.iter().filter(|event|event["method"] == "thread/start").count(), 1);
            assert_eq!(events.iter().filter(|event|event["method"] == "turn/start").count(), 1);
        }
    }

    #[tokio::test]
    async fn actual_batch_host_policy_denial_is_durable_before_ack_and_never_reoffered(){
        for (refuse_report, failed_write) in [(false,None),(true,None),(false,Some(("delivery_finish",1))),(false,Some(("runtime_write",3))),(true,Some(("runtime_write",3)))]{
            let f=crate::domain_orchestrator::testing::fixture();
            let key=AccountKey::native(Provider::Codex);
            let text=r#"[accounts]
connected=["codex"]
[model_profiles.codex.orchestrator]
mode="ordered_candidates"
candidates=[{model="fixture-model",reasoning_effort="high"}]
"#;
            std::fs::write(alondra_core::options::path_in(&f.executor.home),text).unwrap();
            let options=alondra_core::options::OptionsFile::parse(text).unwrap();
            f.run(Actor::System,"codex-policy-roster",Command::RecordAccounts{accounts:vec![AccountStatus{provider:Provider::Codex,id:AccountId::default_account(),name:"Native fixture".into(),state:AccountState::SignedIn,native_identity:Some("fixture-workspace".into()),email:Some("first@example.test".into()),plan:None,billing:Default::default(),sign_ins: 0, profile: None}],reserve:alondra_core::usage::default_reserve()});
            f.run(Actor::System,"codex-policy-order",Command::RecordAccountConfiguration{order:vec![key.clone()],version:"codex-policy-fixture".into()});
            let version=alondra_core::model_selection::profiles_version(&options.model_profiles);
            f.run(Actor::System,"codex-policy-profiles",Command::RecordModelProfiles{profiles:options.model_profiles,version});
            f.run(Actor::System,"codex-policy-catalog",Command::RecordModelCatalog{catalog:AccountModelCatalog{account:key,revision:"codex-policy-models".into(),models:vec![CatalogModel{model:"fixture-model".into(),display_name:"Native fixture".into(),reasoning_efforts:vec!["high".into()],default_reasoning_effort:Some("high".into()),context_window:None,input_modalities:vec!["text".into()],is_default:true,hidden:false}],native_default:Some("fixture-model".into()),measured_at:1}});
            let skill=f.root.join("assets").join(crate::plugins::ORCHESTRATOR).join("skills/alondra-orchestrator");std::fs::create_dir_all(&skill).unwrap();std::fs::write(skill.join("SKILL.md"),"You are the Alondra orchestrator. Use only Alondra role tools.").unwrap();
            let native=f.root.join("codex-native");let fake=std::fs::canonicalize(std::env::var("FIXTURE_CODEX_NATIVE").unwrap()).unwrap();std::fs::copy(fake,&native).unwrap();std::fs::set_permissions(&native,std::fs::Permissions::from_mode(0o700)).unwrap();
            let account=f.root.join("native-account-report.json");std::fs::write(&account,serde_json::to_vec(&json!({"accountRead":{"account":{"type":"apiKey"},"requiresOpenaiAuth":true},"authStatus":{"authMethod":"apikey","authToken":null,"requiresOpenaiAuth":true}})).unwrap()).unwrap();
            let trace=f.root.join("codex-transcript");let mut backend=f.domain().sdk().unwrap().0.clone();let state_root=backend.state_root.clone();
            let host=std::fs::canonicalize(std::env::var("FIXTURE_CODEX_HOST").unwrap()).unwrap();backend.provider=Provider::Codex;backend.origin=None;backend.node=host.clone();backend.host=host;backend.node_sha256=crate::sdk_owner::backend::digest(&backend.node).unwrap();backend.host_sha256=backend.node_sha256.clone();backend.native=native;backend.native_sha256=crate::sdk_owner::backend::digest(&backend.native).unwrap();backend.native_environment.insert("ALONDRA_FAKE_ACCOUNT_FILE".into(),account.display().to_string());backend.native_environment.insert("ALONDRA_FAKE_TRANSCRIPT".into(),trace.display().to_string());
            std::fs::write(f.executor.home.join("codex-backend.json"),serde_json::to_vec(&backend).unwrap()).unwrap();
            f.tell("policy-request","Do not run a model until native payment and identity policy is valid");
            if refuse_report{rusqlite::Connection::open(crate::domain_database_migration::active_path(&f.executor.home)).unwrap().execute_batch("CREATE TRIGGER reject_policy_report BEFORE INSERT ON orchestrator_batch_reports BEGIN SELECT RAISE(ABORT,'injected report failure'); END;").unwrap();}
            let fault = failed_write.map(|(point, occurrence)| alondra_daemon_engine_model::faults::Injection::at(&f.executor.home,point,occurrence));
            let result = f.executor.tick().await;
            if let Some(fault) = &fault { assert!(result.is_err()); assert!(fault.hits()>0); } else { result.unwrap(); }
            drop(fault);
            let cold_restart = refuse_report && failed_write.is_some();
            if cold_restart {
                let restarted = crate::domain_orchestrator::Executor { domain: Arc::new(crate::domain_runtime::DomainRuntime::open(&f.executor.home).unwrap()), home: f.executor.home.clone(), claude_home: f.executor.claude_home.clone() };
                restarted.repair_incomplete().await.unwrap();
                let runtime=crate::domain_orchestrator::load(&f.executor.home).unwrap();
                assert_eq!(runtime.failure,Some(crate::domain_orchestrator::Failure::Interrupted));
                assert!(runtime.active_run.is_some(),"losing both the refusal report and runtime write still cannot permit native replay after restart");
            } else if failed_write.is_some() { f.tick().await; }
            let runtime=crate::domain_orchestrator::load(&f.executor.home).unwrap();let run=runtime.last_run_id.as_deref().unwrap();assert_eq!(runtime.failure,Some(if cold_restart {crate::domain_orchestrator::Failure::Interrupted} else {crate::domain_orchestrator::Failure::Denied}),"actual host diagnostic: {:?}",runtime.last_error);assert!(runtime.retry_at.is_none(),"payment denial requires a user decision, never another automatic provider attempt");
            let reports:Vec<(u64,Vec<u8>,String)>={let conn=rusqlite::Connection::open(crate::domain_database_migration::active_path(&f.executor.home)).unwrap();let mut query=conn.prepare("SELECT cursor,digest,report FROM orchestrator_batch_reports WHERE run=?1 ORDER BY cursor").unwrap();query.query_map([run],|row|Ok((u64::try_from(row.get::<_,i64>(0)?).unwrap(),row.get(1)?,row.get(2)?))).unwrap().map(|row|row.unwrap()).collect()};
            let directory=std::fs::read_dir(&state_root).unwrap().map(|entry|entry.unwrap().path()).find(|path|path.file_name().unwrap().to_string_lossy().starts_with("orchestrator-")).unwrap();
            let mut reader=alondra_codex_protocol::FramedReader::new(std::fs::File::open(directory.join("outbox")).unwrap());let mut acknowledged=0;let mut refusal=None;
            while let Some(frame)=reader.read_received().unwrap(){match &frame.frame().body{
                alondra_codex_protocol::Body::Acknowledge(ack)=>acknowledged=acknowledged.max(ack.cursor),
                alondra_codex_protocol::Body::Event(event)=>if let alondra_codex_protocol::Observation::Sdk{payload,..}=&event.observation && payload["method"]=="alondra/inputRefused"{assert_eq!(payload["params"]["nativeSubmitted"],false);assert_eq!(payload["params"]["code"],"paid_authorization_required");use sha2::Digest;refusal=Some((event.cursor,sha2::Sha256::digest(frame.payload()).to_vec()));},
                _=>{}
            }}
            let (cursor,digest)=refusal.expect("the exact positive no-submission native frame remains inspectable");
            if refuse_report{assert!(reports.is_empty());assert!(acknowledged<cursor,"failed durable report must leave native refusal unacknowledged");}
            else{let report=reports.iter().find(|row|row.0==cursor).unwrap();assert_eq!(report.1,digest);let body:Value=serde_json::from_str(&report.2).unwrap();assert_eq!(body["kind"],"native_policy_refused");assert_eq!(body["input_consumed"],false);assert_eq!(body["turn_completed"],false);assert!(acknowledged>=cursor);}
            f.tick().await;
            let calls:Vec<Value>=std::fs::read_to_string(&trace).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();assert_eq!(calls.iter().filter(|request|request["method"]=="initialize").count(),1);assert!(!calls.iter().any(|request|matches!(request["method"].as_str(),Some("thread/start"|"thread/resume"|"turn/start"))));assert!(f.starts().is_empty(),"a Codex-only denial never starts the disconnected Claude backend");
        }
    }
}
