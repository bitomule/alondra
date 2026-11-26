//! One orchestrator batch through the Agent SDK host, in a sandbox daemon with an SDK backend.
//! The batch's claim owns the host; its one prompt is the batch, and the host takes no other, so
//! a message that arrives during the turn waits for the next batch. What Claude reported goes
//! through the same parser as `claude -p`, and `domain_orchestrator::tick` decides from its end.
use crate::engine::OrchestratorOf as _;
use crate::orchestrator_provider::ProviderOwner;
use crate::domain_store::batch_effects::BatchReport;
use crate::orchestrator_process::{self as process, Output, Stream, bounded};
use crate::sdk_backend::SdkBackend;
use alondra_engine_ports::agent_host::{Preparing, VerifiedArtifact};
use alondra_engine_ports::agent_workers::WorkerConfiguration;
use alondra_provider_claude::{BatchHost, HostEvent, McpServer, OrchestratorProfile};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often a read wakes to look whether the user cancelled the run. It decides nothing.
const WAKE: Duration = Duration::from_millis(250);

pub(crate) use crate::orchestrator_provider::Batch;

/// Runs the batch's one turn: `spawned` once the host took the prompt, until Claude's process
/// ends or `cancel` resolves.
pub(crate) async fn execute(batch: Batch, spawned: impl FnOnce() -> anyhow::Result<()> + Send + 'static, cancel: impl std::future::Future<Output = ()>) -> Output {
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&cancelled);
    let mut run = alondra_core::clock::spawn_blocking_unwatched(move || run(batch, spawned, &stop));
    tokio::select! {
        biased;
        finished = &mut run => return finished.unwrap_or_else(|error| failed(format!("the SDK run stopped: {error}"))),
        () = cancel => cancelled.store(true, Ordering::SeqCst),
    }
    let mut output = run.await.unwrap_or_else(|error| failed(format!("the SDK run stopped: {error}")));
    output.stream.error = Some("Orchestrator execution was cancelled; check its effects before retrying".into());
    output
}

fn failed(why: String) -> Output {
    Output { stream: Stream::default(), diagnostic: bounded(why), exit_ok: false }
}

fn run(batch: Batch, spawned: impl FnOnce() -> anyhow::Result<()>, cancelled: &AtomicBool) -> Output {
    let (configuration, _preparing) = match configuration(&batch) {
        Ok(configuration) => configuration,
        Err(why) => return failed(why),
    };
    let profile = profile(&batch);
    let run = batch.owner.run().to_owned();
    let mut host = match BatchHost::start(ProviderOwner(batch.owner), &configuration, &batch.session, batch.resume, &profile, &batch.prompt) {
        Ok(host) => host,
        Err(error) => {
            remove_folder(&configuration.state_directory);
            return failed(format!("the SDK host did not take the batch: {error:?}"));
        }
    };
    let _host = match crate::domain_orchestrator::HostGuard::attach(Arc::clone(&batch.domain), &run, host.stopper()) {
        Ok(guard) => guard,
        Err(error) => { stop(host, &batch.directory, Some(&configuration.state_directory)); return failed(format!("Cannot own the orchestrator host: {error:#}")); }
    };
    let mut output = Output { stream: Stream::for_input(host.input()), diagnostic: String::new(), exit_ok: false };
    if let Err(error) = process::record_owner(&batch.directory, host.process_id(), &run, &batch.domain.orchestrator().injected).and_then(|()| spawned()) {
        output.stream.storage_failure=true;
        output.stream.error = Some(bounded(format!("cannot record the SDK host: {error:#}")));
        stop(host, &batch.directory, None);
        return output;
    }
    let mut consumed=false;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        match host.readable(WAKE) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(error) => {
                output.stream.error.get_or_insert_with(|| format!("the SDK host stopped: {error:?}"));
                break;
            }
        }
        let pending=match host.next_pending(){Ok(value)=>value,Err(error)=>{output.stream.error.get_or_insert_with(||format!("The SDK host stopped: {error:?}"));break}};
        let mut exited=false;
        let mut report=match pending.event {
            HostEvent::Sdk(message)=>{
                let accepted=serde_json::to_vec(&message).map_err(anyhow::Error::from).and_then(|line|output.stream.accept(&line,&batch.session));
                let valid=accepted.is_ok() && message["session_id"]==batch.session;
                if let Err(error)=accepted {output.stream.error.get_or_insert_with(||bounded(error.to_string()));exited=true;}
                if valid && message["parent_tool_use_id"].is_null() && ((message["type"]=="user" && message["uuid"].as_str().is_some_and(|id|id.eq_ignore_ascii_case(host.input()))) || (message["type"]=="result" && message["num_turns"].as_u64().is_some_and(|turns|turns>0) && message["user_message_uuid"].as_str().is_some_and(|id|id.eq_ignore_ascii_case(host.input())))) {consumed=true;}
                if output.stream.result && output.stream.success && !consumed {output.stream.error=Some("A successful Claude terminal result without positive input consumption cannot settle its batch".into());}
                BatchReport{kind:message["type"].as_str().filter(|kind|!kind.is_empty() && kind.len()<=128).unwrap_or("sdk_unknown").into(),input_consumed:consumed,turn_completed:output.stream.result,native_turn:None,failure:output.stream.error.clone(),public_result:output.stream.note.as_ref().map(|text|bounded(text.clone())),..Default::default()}
            }
            HostEvent::Diagnostic{text,..}=>output.diagnosed(&text),
            HostEvent::UnexpectedDecision=>{exited=true;output.unexpected_decision()}
            HostEvent::Exited{code,..}=>{
                output.exit_ok=code==Some(0);exited=true;
                BatchReport{kind:"process_exited".into(),input_consumed:consumed,turn_completed:output.stream.result,native_turn:None,failure:output.stream.error.clone(),public_result:output.stream.note.as_ref().map(|text|bounded(text.clone())),..Default::default()}
            }
        };
        if output.stream.denied { report.stop = Some(alondra_domain_model::orchestrator_batch::BatchStop::Denied); }
        if let Err(error)=batch.domain.publish_batch_report(&run,pending.cursor,&pending.evidence,&report){
            output.stream.storage_failure=true;
            output.stream.error=Some(bounded(format!("The batch report was not committed; its host evidence remains unacknowledged: {error:#}")));break;
        }
        if let Err(error) = batch.domain.orchestrator().injected.write("ack") {
            output.stream.storage_failure = true;
            output.stream.error = Some(format!("The batch ACK could not be written: {error}"));
            break;
        }
        if let Err(error)=host.acknowledge(pending.cursor){
            output.stream.storage_failure=true;output.stream.error=Some(format!("The batch ACK is uncertain: {error:?}"));break;}
        if exited {break}

    }
    stop(host, &batch.directory, (!output.stream.storage_failure).then_some(configuration.state_directory.as_path()));
    output
}

/// Stops the batch's host, and deletes its folder, `spent`, once it ended: one whose batch could
/// not record what it observed keeps its folder, whose outbox still holds it.
fn stop(host: BatchHost<ProviderOwner>, directory: &Path, spent: Option<&Path>) {
    match host.stop(){
        Ok(())=>{let _=std::fs::remove_file(directory.join("owner.json")); spent.into_iter().for_each(remove_folder);},
        Err(error)=>log!("orchestrator: the SDK host did not stop cleanly; its process record is retained: {error:?}"),
    }
}

/// Writes conversation `session` of the orchestrator, which ran on `account`, condensed into its
/// working directory `directory`, from Claude Code's transcript of it. `None` when the transcript
/// cannot be found or read: the new conversation then goes on from the domain alone.
pub(crate) fn keep_conversation(domain: &crate::domain_runtime::DomainRuntime, backend: &crate::sdk_backend::SdkBackend, home: &Path,
    account: &alondra_core::accounts::AccountId, directory: &Path, session: &str) -> Option<std::path::PathBuf> {
    let backend = backend.for_provider(alondra_core::accounts::Provider::Claude, home)
        .and_then(|backend| backend.for_account(home, account, &domain.engine().registered_accounts(home, alondra_core::accounts::Provider::Claude)))
        .map_err(|error| log!("orchestrator: cannot find the Claude profile of conversation {session}: {error:#}"))
        .ok()?;
    let Some(transcript) = alondra_engine_ports::claude_history::transcript(&backend.native_environment, session, directory) else {
        log!("orchestrator: Claude Code keeps no transcript of conversation {session}");
        return None;
    };
    crate::orchestrator_conversation::write_conversation(directory, session, &transcript)
        .map_err(|error| log!("orchestrator: cannot write conversation {session} down: {error}"))
        .ok()
}


/// The batch's host ended, and nothing reads its folder again: the batch alone knew its name.
pub(crate) fn remove_folder(state_directory: &Path) {
    if let Err(error) = std::fs::remove_dir_all(state_directory) {
        log!("orchestrator: could not delete the folder of the batch's host {}: {error}", state_directory.display());
    }
}

/// Whether Claude Code still keeps a transcript of conversation `session` on `account`; `None`
/// when that cannot be told.
pub(crate) fn conversation_kept(domain: &crate::domain_runtime::DomainRuntime, backend: &crate::sdk_backend::SdkBackend, home: &Path,
    account: &alondra_core::accounts::AccountId, directory: &Path, session: &str) -> Option<bool> {
    let backend = backend.for_provider(alondra_core::accounts::Provider::Claude, home)
        .and_then(|backend| backend.for_account(home, account, &domain.engine().registered_accounts(home, alondra_core::accounts::Provider::Claude)))
        .ok()?;
    Some(alondra_engine_ports::claude_history::available(&backend.native_environment, session, directory))
}

fn configuration(batch: &Batch) -> Result<(WorkerConfiguration, Preparing), String> {
    let hosts = alondra_engine_ports::agent_host::of(batch.domain.ports()).map_err(|error| format!("starting the SDK host: {error}"))?;
    let backend = &batch.backend.for_provider(alondra_core::accounts::Provider::Claude,&batch.home).and_then(|backend|backend.for_start(&*hosts))
        .and_then(|backend| backend.for_account(&batch.home, &batch.selection.account.account, &batch.domain.engine().registered_accounts(&batch.home, alondra_core::accounts::Provider::Claude)))
        .map_err(|error| format!("{error:#}"))?;
    std::fs::create_dir_all(&backend.state_root).map_err(|error| format!("creating the SDK state root: {error}"))?;
    let state_directory = backend.state_root.join(format!("o-{}", alondra_foundation::random_hex(8)));
    let preparing = Preparing::hold(&state_directory);
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&state_directory)
            .map_err(|error| format!("creating the host's folder: {error}"))?;
    }
    let mut native_environment = backend.native_environment.clone();
    native_environment.extend(BTreeMap::from([
        ("ALONDRA_HOME".to_owned(), batch.home.display().to_string()),
        ("ALONDRA_ORCHESTRATOR".to_owned(), "1".to_owned()),
        ("ALONDRA_PROVIDER".to_owned(), "claude".to_owned()),
        ("ALONDRA_ORCHESTRATOR_EXECUTION".to_owned(), batch.owner.run().to_owned()),
        ("ALONDRA_SESSION_ID".to_owned(), alondra_core::orchestrator::NODE_ID.to_owned()),
        ("ALONDRA_DOMAIN_CAPABILITY".to_owned(), batch.capability.clone()),
        ("ALONDRA_DOMAIN_PROTOCOL".to_owned(), "2".to_owned()),
        // The conversation is resumed batch after batch, often minutes apart: its prompt cache lasts
        // an hour, as a node's does, also once an account pays beyond its plan.
        ("CLAUDE_CODE_PROMPT_CACHE_TTL".to_owned(), "1h".to_owned()),
        ("CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL".to_owned(), "5m".to_owned()),
    ]));
    for name in ["ALONDRA_PORT", "ALONDRA_SANDBOX"] {
        if native_environment.contains_key(name){continue}
        if let Ok(value) = std::env::var(name) {
            native_environment.insert(name.to_owned(), value);
        }
    }
    Ok((WorkerConfiguration {
        artifact: VerifiedArtifact::verify(&backend.node, &backend.host, &backend.node_sha256, &backend.host_sha256)
            .map_err(|error| format!("verifying the SDK host: {error}"))?,
        state_directory,
        host_environment: backend.host_environment.clone(),
        native_executable: backend.native.clone(),
        native_directory: batch.directory.clone(),
        native_environment,
        profile_id: "alondra".into(),
        account_id: batch.selection.account.account.to_string(),
        host_release: env!("CARGO_PKG_VERSION").into(),
        io_timeout: Duration::from_secs(backend.io_timeout_seconds),
        confinement: backend.confinement.clone(),
        hosts: alondra_engine_ports::agent_host::of(batch.domain.ports()).map_err(|error| format!("starting the SDK host: {error}"))?,
        worker: None,
    }, preparing))
}

/// Alondra's own MCP server, allowed by rule: auto mode never needs its safety check to let the
/// orchestrator use the tools its batch exists to call.
const OWN_TOOLS: &str = "mcp__alondra-orchestrator";

fn profile(batch: &Batch) -> OrchestratorProfile {
    let mut settings = crate::plugins::launch_settings_with(batch.projects_root.as_deref().map(process::read_only));
    let permissions = settings.as_object_mut().map(|settings| settings.entry("permissions").or_insert_with(|| serde_json::json!({})));
    if let Some(permissions) = permissions.and_then(serde_json::Value::as_object_mut) {
        let allow = permissions.entry("allow").or_insert_with(|| serde_json::json!([]));
        if let Some(allow) = allow.as_array_mut() {
            allow.push(OWN_TOOLS.into());
        }
    }
    OrchestratorProfile {
        model: batch.selection.model.model.model.clone(),
        reasoning_effort: batch.selection.model.reasoning_effort.clone(),
        tools: process::RESTRICTED_TOOLS.split(',').map(str::to_owned).collect(),
        disallowed_tools: vec!["AskUserQuestion".into(), "ExitPlanMode".into()],
        mcp_servers: BTreeMap::from([(
            "alondra-orchestrator".to_owned(),
            McpServer { command: batch.node_plugin.join("bin/alondra"), args: vec!["mcp-orchestrator".into()] },
        )]),
        plugins: vec![batch.node_plugin.clone(), batch.orchestrator_plugin.clone()],
        settings: settings.as_object().cloned().unwrap_or_default(),
    }
}

#[cfg(test)]
#[path = "orchestrator_sdk/tests.rs"]
mod tests;
