//! A requested selection stays durable while its current execution drains, is fenced and hands off.
use alondra_engine_kernel::sdk_backend::SdkOf as _;
use alondra_daemon_engine_model::LaunchOptionsFile as _;
use alondra_engine_ports::gate::GateOf as _;
use crate::sdk_owner::SdkOwners as _;
use crate::engine::EngineOf as _;
use alondra_engine_kernel::domain_runtime::RuntimeRecords as _;
use alondra_core::clock;
use crate::locks::Poisonable as _;
use alondra_engine_ports::Runtime;
use crate::domain_store::{Attached,OwnerJournal,StoreError};
use crate::domain_store::provider_transition::{Kind,Plan,Phase,Transition,prepared_facts};
use alondra_engine_ports::agent_host::{EndedHost,Retained};
use alondra_core::accounts::AccountKey;
use alondra_domain_rules::*;
use alondra_core::model_selection::{ModelSelection,MoveRefused,PendingTaskSelection,TaskSelection};
use alondra_core::selection_transition::{ActiveSelection,SelectionTransition,TransitionCapabilities,classify_transition};
use alondra_domain::ActivityView;
use alondra_store::{AuthorityScope,ContextEntry,PreparedHandoff,HandoffBudget};
use serde_json::{Value,json};
use std::collections::BTreeMap;
use std::sync::{Arc,Mutex};

#[derive(Default)]
pub(crate) struct Transitions {
    advancing:tokio::sync::Mutex<()>,
    /// A holder that panics leaves it whole, so it is recovered: one insert or remove each.
    fences:Mutex<BTreeMap<String,EndedHost>>,
    pub(crate) crash_at:Mutex<Option<Phase>>,
}
impl Transitions {
    /// What a daemon that dies lets go of: the ended hosts it held as proof.
    pub(crate) fn die(&self){*self.crash_at.lock_or_recover("the transition's crash point")=None;self.fences.lock_or_recover("the transition fences").clear();}
}
fn failure(error:impl std::fmt::Display)->DomainError{DomainError::new("selection_transition",error.to_string())}
fn store_failure(error:StoreError)->DomainError{match error{StoreError::Domain(error)=>error,StoreError::Storage(error)=>failure(error)}}
fn scope(domain:&dyn Runtime)->Result<AuthorityScope,DomainError>{AuthorityScope::new(domain.read().state().data_epoch.clone()).map_err(failure)}
fn envelope(domain:&dyn Runtime,id:&str,command:Command)->CommandEnvelope{CommandEnvelope{protocol_version:PROTOCOL_VERSION,data_epoch:domain.read().state().data_epoch.clone(),command_id:id.into(),expected_revision:0,command}}
fn current(domain:&dyn Runtime,task:&TaskId)->Result<Option<Transition>,DomainError>{domain.store().lock_or_exit("the domain store").provider_transition(task).map_err(failure)}

pub(crate) fn denied_tools(domain:&dyn Runtime,task:&TaskId)->Result<Vec<String>,DomainError>{
    let state=domain.state();
    let task_data=state.tasks.get(task).ok_or_else(||DomainError::new("not_found","Task missing"))?;
    let execution=state.lifecycle_of(task)?.execution().cloned().ok_or_else(||failure("The task has no managed execution"))?;
    let (backend,_)=domain.sdk().ok_or_else(||failure("The provider runtime is unavailable"))?;
    let selected=backend.for_provider(task_data.provider,domain.home()).map_err(failure)?;
    alondra_daemon_engine_model::LaunchOptions::load(&selected.state_root.join(execution.instance().as_str())).map(|options|options.disallowed_tools).map_err(failure)
}

pub fn request(domain:&dyn Runtime,actor:&Actor,task:&TaskId,target_account:AccountKey,target_model:ModelSelection,intent:TaskSelection,reason:&str,expected_selection:&str,authorize:impl FnOnce()->Result<(),DomainError>)->Result<Value,DomainError>{
    let initial=domain.state();
    let held=initial.tasks.get(task).ok_or_else(||DomainError::new("not_found","Task missing"))?;
    let execution=initial.lifecycle_of(task)?.execution().cloned().ok_or_else(||failure("The task has no managed execution to change"))?;
    let (backend,_)=domain.sdk().ok_or_else(||failure("The provider runtime is unavailable"))?;
    let selected=backend.for_provider(held.provider,domain.home()).map_err(failure)?;
    let host_directory=selected.state_root.join(execution.instance().as_str());
    let id=format!("selection-{}",crate::auth::random_hex(16));
    let pending=PendingTaskSelection{account:target_account.clone(),model:target_model.clone(),reason:reason.into()};
    let command=envelope(domain,&id,Command::RequestTaskSelection{task_id:task.clone(),expected_selection:expected_selection.into(),selection:intent.clone(),pending});
    let ((),_)=domain.own(scope(domain)?,|owner|{
        authorize()?;
        if owner.state().lifecycle_of(task)?.execution()!=Some(&execution){return Err(DomainError::new("stale_execution","The active native session changed; refresh the model selection").into())}
        let source=owner.selection_source(task,host_directory)?;
        if target_account!=source.account{return Err(DomainError::new("immutable_account","Choose the account and provider when creating a new node; this session keeps its account").into())}
        let effective=source.selection.as_ref().and_then(|selection|selection.effective.as_ref()).ok_or_else(||DomainError::new("native_session_required","This older session cannot prove its active model; create a new node with the desired selection"))?;
        if effective.reasoning_effort!=target_model.reasoning_effort{return Err(DomainError::new("unsupported_live_effort","This native session cannot change reasoning effort with the verified model control; choose that effort when creating a new node").into())}
        if effective.model!=target_model.model && !source.turn_model{return Err(DomainError::new("unsupported_live_model","This native session cannot change its model in place; create a new node").into())}
        if target_account.provider.lacks_tool_denials() && !source.disallowed_tools.is_empty(){return Err(DomainError::new("unsupported_tool_policy",format!("{} cannot enforce this task's tool deny list; keep the current provider or explicitly revise the task policy",target_account.provider.display_name())).into())}
        owner.stage_command(actor,&command,Attached::None)?;
        owner.request_provider_transition(&Plan{id:id.clone(),kind:Kind::Model,task:task.clone(),expected_selection:expected_selection.into(),source,target_account,target_model,intent,reason:reason.into(),conversation_file:None,role:None})
    }).map_err(store_failure)?;
    Ok(json!({"task_id":task,"transition":id,"status":"pending"}))
}

/// Moves the task to `account`. A task that already ran keeps the move open while its execution
/// ends, and a new node on `account` takes it over from its record; one that never ran just starts
/// there. The command and the move's first step commit together, or neither does.
pub fn request_move(domain:&dyn Runtime,actor:&Actor,task:&TaskId,account:AccountKey,expected_selection:&str,authorize:impl FnOnce()->Result<(),DomainError>)->Result<Value,DomainError>{
    let initial=domain.state();
    let (intent,pending)=initial.moved_selection(actor,task,&account)?;
    let id=format!("move-{}",crate::auth::random_hex(16));
    let command=envelope(domain,&id,Command::MoveTask{task_id:task.clone(),expected_selection:expected_selection.into(),account:account.clone()});
    let Some(pending)=pending else{
        domain.own(scope(domain)?,|owner|{authorize()?;owner.stage_command(actor,&command,Attached::None).map(drop)}).map_err(store_failure)?;
        return Ok(json!({"task_id":task,"status":"moved"}))
    };
    let execution=initial.lifecycle_of(task)?.execution().cloned().ok_or_else(||failure("The task has no execution to move"))?;
    let (backend,_)=domain.sdk().ok_or_else(||failure("The provider runtime is unavailable"))?;
    let selected=backend.for_provider(initial.tasks[task].provider,domain.home()).map_err(failure)?;
    let host_directory=selected.state_root.join(execution.instance().as_str());
    domain.own(scope(domain)?,|owner|{
        authorize()?;
        if owner.state().lifecycle_of(task)?.execution()!=Some(&execution){return Err(DomainError::new("stale_execution","The task's execution changed; refresh before moving it").into())}
        let source=owner.selection_source(task,host_directory)?;
        owner.stage_command(actor,&command,Attached::None)?;
        owner.request_provider_transition(&Plan{id:id.clone(),kind:Kind::Move,task:task.clone(),expected_selection:expected_selection.into(),source,target_account:account,target_model:pending.model,intent,reason:pending.reason,conversation_file:None,role:None})
    }).map_err(store_failure)?;
    Ok(json!({"task_id":task,"transition":id,"status":"pending"}))
}

/// Whether a move holds the task: no node but the one it moves to may start for it.
pub fn moving(domain:&dyn Runtime,task:&TaskId)->bool{
    if domain.read().state().tasks.get(task).is_some_and(owes_role_change){return true}
    (domain.read().state().moving_to(task).is_some() && matches!(current(domain,task),Ok(Some(change)) if change.plan.kind==Kind::Move))
        || starts_elsewhere(domain,task)
}
/// Whether the task moved before a node started on its new account and none has started there
/// yet: its last execution is still the one it moved from.
fn starts_elsewhere(domain:&dyn Runtime,task:&TaskId)->bool{
    let held={let store=domain.store().lock_or_exit("the domain store");store.deferred_move(task).and_then(|deferred|match deferred{Some(change)=>Ok(Some(change)),None=>store.refused_move(task)})};
    let Ok(Some(change))=held else{return false};
    let read=domain.read();let state=read.state();
    let open=state.lifecycle_of(task).is_ok_and(|lifecycle|!matches!(lifecycle.phase(),alondra_domain::TaskPhase::Finishing|alondra_domain::TaskPhase::Completed|alondra_domain::TaskPhase::Cancelled));
    let last=state.lifecycle_journal.get(task).and_then(|journal|journal.iter().rev().find_map(|record|match record{LifecycleRecord::Admitted{execution:Some(execution),..}=>Some(execution.instance.as_str().to_owned()),_=>None}));
    open && last.as_deref()==Some(change.plan.source.instance.as_str())
}

pub(crate) fn holds_source_inputs(domain:&dyn Runtime,task:&TaskId,instance:&str)->bool{
    if domain.read().state().tasks.get(task).is_some_and(owes_role_change){return true}
    if domain.read().state().tasks.get(task).is_none_or(|task|task.pending_selection.is_none()){return false}
    match current(domain,task){Ok(Some(change))=>change.plan.source.instance==instance,Ok(None)=>false,Err(_)=>true}
}
/// Whether the task owes a change of role its current execution does not play: nothing reaches
/// that execution, and nothing takes it up again, until the new one starts.
pub(crate) fn owes_role_change(task:&alondra_domain_rules::Task)->bool{
    task.stages.handover.as_ref().is_some_and(|handover|task.stages.role()!=Some(handover.to))
}
pub(crate) fn holds_execution(domain:&dyn Runtime,task:&TaskId)->bool{
    let read=domain.read();let instance=read.state().lifecycle_of(task).ok().and_then(|lifecycle|lifecycle.execution().map(|stamp|stamp.instance().as_str().to_owned()));
    instance.is_some_and(|instance|holds_source_inputs(domain,task,&instance))
}
fn phase(domain:&dyn Runtime,change:&Transition,next:Phase,facts:Value)->Result<(),DomainError>{domain.own(scope(domain)?,|owner|owner.provider_transition_phase(&change.plan.id,change.phase,next,facts)).map_err(store_failure).map(drop)}
fn applied(plan:&Plan)->TaskSelection{let mut selected=plan.intent.clone();selected.effective=Some(plan.target_model.clone());selected}
fn apply_selection(domain:&dyn Runtime,plan:&Plan,from:Phase,to:Phase,facts:Value)->Result<(),DomainError>{
    domain.own(scope(domain)?,|owner|{
        owner.validate_selection_source(plan)?;
        owner.stage_command(&Actor::System,&envelope(domain,&format!("{}-effective",plan.id),Command::RecordTaskSelection{task_id:plan.task.clone(),selection:applied(plan),pending:None}),Attached::None)?;
        owner.provider_transition_phase(&plan.id,from,to,facts)
    }).map_err(store_failure).map(drop)
}
fn abort_unapplied_model_change(domain: &dyn Runtime, change: &Transition) -> Result<bool, DomainError> {
    let plan = &change.plan;
    domain.own(scope(domain)?, |owner| {
        owner.validate_selection_binding(plan)?;
        let status = owner.transition_model_control_status(plan)?;
        let code = if status.as_deref() == Some("rejected") {
            "native_model_control_rejected"
        } else { return Ok(false) };
        let task = owner.state().tasks.get(&plan.task).ok_or_else(|| failure("The task is missing"))?;
        let expected_pending = PendingTaskSelection { account: plan.target_account.clone(), model: plan.target_model.clone(), reason: plan.reason.clone() };
        if task.selection.as_ref() != Some(&plan.intent) || task.pending_selection.as_ref() != Some(&expected_pending) {
            return Err(DomainError::new("stale_selection", "The requested selection changed; the earlier model control cannot clear it").into())
        }
        let selection = plan.intent.clone();
        let notice_id = format!("native-model-change-{}", plan.id);
        owner.stage_command(&Actor::System, &envelope(domain, &format!("{}-not-applied", plan.id), Command::RecordTaskSelection {
            task_id: plan.task.clone(), selection, pending: None,
        }), Attached::None)?;
        owner.stage_command(&Actor::System, &envelope(domain, &format!("{}-notice", plan.id), Command::PublishInformation {
            interaction_id: notice_id.as_str().into(), scope: Scope::Task { task_id: plan.task.clone() }, recipient: Recipient::User,
            body: code.into(), attachments: vec![], card: CardSpec::new(CardKind::Know, "The model change was not applied"),
        }), Attached::None)?;
        owner.provider_transition_phase(&plan.id, change.phase, Phase::Aborted, json!({"reason":code,"model_applied":false}))?;
        Ok(true)
    }).map_err(store_failure).map(|(aborted, _)| aborted)
}
fn classify(plan:&Plan)->SelectionTransition{
    if matches!(plan.kind,Kind::Move|Kind::Refresh|Kind::Stage){return SelectionTransition::CreateWithHandoff}
    if plan.target_account!=plan.source.account{return SelectionTransition::Reject("An active node keeps its account and provider".into())}
    let Some(model)=plan.source.selection.as_ref().and_then(|selection|selection.effective.clone()) else{return SelectionTransition::CreateWithHandoff};
    let current=ActiveSelection{account:plan.source.account.clone(),model,configuration:plan.source.configuration.clone()};
    let target=ActiveSelection{account:plan.target_account.clone(),model:plan.target_model.clone(),configuration:if plan.target_account==plan.source.account{plan.source.configuration.clone()}else{plan.target_account.wire_id()}};
    let capabilities=TransitionCapabilities{turn_scoped_model:plan.source.turn_model,turn_scoped_reasoning:false,native_resume:false,pinned_account:plan.intent.account.clone(),requested_model:plan.intent.requested.clone(),requested_reasoning_effort:plan.intent.requested_reasoning_effort.clone(),allow_model_substitution:plan.intent.allow_model_substitution,..Default::default()};
    classify_transition(&current,&target,&capabilities,None)
}
/// The context the target of the change `id` starts from, and the file of the conversation before
/// it that a refresh lets it read.
pub(crate) fn retained_handoff(domain:&dyn Runtime,task:&TaskId,id:&str)->Result<(PreparedHandoff,Option<std::path::PathBuf>),DomainError>{
    let change=match current(domain,task)?{
        Some(change)=>change,
        None=>domain.store().lock_or_exit("the domain store").deferred_move(task).map_err(failure)?.ok_or_else(||failure("The transfer context is missing"))?,
    };
    if change.plan.id!=id || !matches!(change.phase,Phase::ContextReady|Phase::Done){return Err(failure("The transfer is not ready for its target"))}
    let file=change.plan.conversation_file.clone();
    Ok((serde_json::from_value(prepared_facts(change)).map_err(failure)?,file))
}
fn drained(domain:&dyn Runtime,plan:&Plan)->Result<bool,DomainError>{
    let state=domain.state();let lifecycle=state.lifecycle_of(&plan.task)?;
    let unoffered=domain.own(scope(domain)?,|owner|owner.source_inputs_unoffered(&plan.task)).map_err(store_failure)?.0;
    Ok(unoffered && lifecycle.execution().is_some_and(|stamp|stamp.instance().as_str()==plan.source.instance) && lifecycle.turn().is_none()
        && !matches!(lifecycle.activity(),ActivityView::Working|ActivityView::OwnWork)
        && lifecycle.cards().iter().all(|(card,_)|lifecycle.card_callback(card).is_none()) && lifecycle.doubts().is_empty())
}
/// The task's canonical context, or why it cannot move: Alondra no longer has part of it.
pub(crate) fn critical_context(state:&DomainState,task:&alondra_domain_rules::Task)->Result<String,DomainError>{
    let canonical=alondra_engine_kernel::launches::canonical_task_context(state,task);
    if !canonical.missing_sources.is_empty(){return Err(failure(format!("Critical task context is missing: {}",canonical.missing_sources.join(", "))))}
    Ok(canonical.text)
}
fn handoff_refusal(error:alondra_store::HandoffError)->String{
    match error{
        alondra_store::HandoffError::CriticalContextTooLarge{needed,available}=>format!("The task's context does not fit the destination model: it needs about {needed} tokens and {available} are available"),
        alondra_store::HandoffError::NewInputTooLarge=>"The message that continues the task does not fit the destination model".into(),
        alondra_store::HandoffError::IncompleteHistory(reason)=>format!("The task's public conversation is incomplete: {reason}"),
    }
}
fn branch_of(directory:&str)->Option<String>{
    let output=std::process::Command::new("git").arg("-C").arg(directory).args(["rev-parse","--abbrev-ref","HEAD"]).output().ok()?;
    let name=String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !name.is_empty()).then_some(name)
}
/// What a refresh starts from: the worktree, the plan and the assistant's last message, with the
/// task's complete records and the plan written beside the conversation file to read on demand.
fn refresh_handoff(state:&DomainState,plan:&Plan,canonical:&str,entries:&[ContextEntry],window:Option<u64>)->Result<PreparedHandoff,StoreError>{
    let task=state.tasks.get(&plan.task).ok_or_else(||failure("Task missing"))?;
    let conversation=plan.conversation_file.as_ref().ok_or_else(||failure("A refresh needs the conversation file"))?;
    let folder=conversation.parent().ok_or_else(||failure("The conversation file has no folder"))?;
    let write=|name:&str,text:&str|->Result<String,StoreError>{
        let file=folder.join(name);
        alondra_foundation::save::atomic(&file,text.as_bytes()).map_err(|error|failure(format!("{name} could not be written: {error}")))?;
        Ok(file.display().to_string())
    };
    let records_file=write("task-records.md",canonical)?;
    let approved=alondra_engine_kernel::launches::approved_plan(state,task);
    let plan_file=if approved.is_empty(){None}else{Some(write("approved-plan.md",&approved)?)};
    let parts=alondra_store::RefreshParts{
        worktree:plan.source.directory.clone(),
        branch:branch_of(&plan.source.directory),
        pull_requests:state.task_prs(task).map(|pr|format!("https://{}/{}/pull/{}",pr.host,pr.repository,pr.number)).collect(),
        plan:Some(approved).filter(|text|!text.is_empty()),plan_file,
        last_assistant_message:entries.iter().rev().find(|entry|entry.item.source==alondra_store::ContextSource::Assistant).map(|entry|entry.item.content.clone()),
        records_file,conversation_file:conversation.display().to_string(),
    };
    let through=entries.last().map(|entry|entry.reference.clone());
    alondra_store::prepare_refresh_handoff(&parts,"Continue this Alondra task.",through,HandoffBudget{destination_context_tokens:window,reserved_tokens:alondra_store::HANDOFF_RESERVED_TOKENS}).map_err(|error|failure(handoff_refusal(error)).into())
}
/// The context the new execution of `plan` starts from: the task's canonical context and its
/// retained public records, within the window of the model it starts with.
fn handoff(owner:&mut dyn OwnerJournal,plan:&Plan)->Result<PreparedHandoff,StoreError>{
    let task=owner.state().tasks.get(&plan.task).ok_or_else(||failure("Task missing"))?;
    if plan.kind==Kind::Stage{return stage_handoff(owner.state(),plan)}
    let canonical=critical_context(owner.state(),task)?;
    let mut entries:Vec<ContextEntry>=vec![];let mut after=None;let mut coverage;
    loop{
        let page=owner.retained_context(&plan.task,after.as_ref(),128)?;coverage=page.coverage;
        let next=page.next;let last=page.entries.last().map(|entry|entry.reference.clone());entries.extend(page.entries);
        if entries.len()>8192{return Err(failure("Public context exceeds the supported transfer page bound").into())}
        if next.is_none(){break}
        if last.as_ref().is_none_or(|last|Some(last.sequence)!=next){return Err(failure("Public context pagination did not advance").into())}
        after=last;
    }
    if plan.source.selection.as_ref().and_then(|selection|selection.effective.as_ref()).is_none(){return Err(failure("This older execution has no complete retained public history; continue its current native session").into())}
    let window=owner.state().model_catalogs.iter().find(|catalog|catalog.account==plan.target_account).and_then(|catalog|catalog.models.iter().find(|model|model.model==plan.target_model.model.model)).and_then(|model|model.context_window);
    if plan.kind==Kind::Refresh{return refresh_handoff(owner.state(),plan,&canonical,&entries,window)}
    alondra_store::prepare_context_handoff(&canonical,"Continue this Alondra task from its public records. The user changed its provider or model. Preserve completed work and answer through Alondra.",&entries,&coverage,None,HandoffBudget{destination_context_tokens:window,reserved_tokens:alondra_store::HANDOFF_RESERVED_TOKENS}).map_err(|error|failure(handoff_refusal(error)).into())
}
/// What a stage's new execution starts from: the packet Alondra writes for its role, never the
/// conversation before it.
fn stage_handoff(state:&DomainState,plan:&Plan)->Result<PreparedHandoff,StoreError>{
    let task=state.tasks.get(&plan.task).ok_or_else(||failure("Task missing"))?;
    let role=plan.role.ok_or_else(||failure("A change of role names the role it hands over to"))?;
    let packet=alondra_engine_kernel::launches::stage_packet(state,task,role,&plan.source.directory,branch_of(&plan.source.directory).as_deref())
        .map_err(|missing|failure(format!("Critical task context is missing: {}",missing.join(", "))))?;
    let window=state.model_catalogs.iter().find(|catalog|catalog.account==plan.target_account).and_then(|catalog|catalog.models.iter().find(|model|model.model==plan.target_model.model.model)).and_then(|model|model.context_window);
    alondra_store::prepare_context_handoff(&packet,&stage_input(role),&[],&alondra_store::HistoryCoverage::default(),None,HandoffBudget{destination_context_tokens:window,reserved_tokens:alondra_store::HANDOFF_RESERVED_TOKENS})
        .map_err(|error|failure(handoff_refusal(error)).into())
}
/// Starts, when it is due, the change of role the staged task owes: the account and model of its
/// next role are chosen now, its current execution keeps every input from then on, and the change
/// waits for it to rest before it ends it. False when nothing is due or another change holds it.
pub(crate) fn request_stage(domain:&Arc<dyn Runtime>,task:&TaskId)->Result<bool,DomainError>{
    let initial=domain.state();
    let Some(held)=initial.tasks.get(task) else{return Ok(false)};
    let Some(handover)=held.stages.handover.clone().filter(StageHandover::due) else{return Ok(false)};
    if held.state.terminal() || held.ending.is_some() || held.pending_selection.is_some() || current(&**domain,task)?.is_some(){return Ok(false)}
    let Some(execution)=initial.lifecycle_of(task)?.execution().cloned() else{return Ok(false)};
    let intent=held.stages.selections.get(&handover.to).cloned().ok_or_else(||failure("The task has no selection for the role it hands over to"))?;
    let (backend,_)=domain.sdk().ok_or_else(||failure("The provider runtime is unavailable"))?;
    let selected=backend.for_provider(held.provider,domain.home()).map_err(failure)?;
    let host_directory=selected.state_root.join(execution.instance().as_str());
    let denied=alondra_daemon_engine_model::LaunchOptions::load(&host_directory).map(|options|options.disallowed_tools).map_err(failure)?;
    let decision=domain.engine().select_account(domain.home(),&intent,&denied,alondra_core::now_millis())?;
    let id=format!("stage-{}",crate::auth::random_hex(16));
    let reason=format!("The {} takes the task over. {}",handover.to.as_str(),decision.reason).chars().take(1900).collect::<String>();
    let pending=PendingTaskSelection{account:decision.account.clone(),model:decision.selection.clone(),reason:reason.clone()};
    let expected_selection=initial.task_selection_stamp(task).unwrap_or_default();
    let command=envelope(&**domain,&format!("{id}-begin"),Command::BeginStageHandover{task_id:task.clone(),expected_selection:expected_selection.clone(),transition:id.clone(),pending});
    domain.own(scope(&**domain)?,|owner|{
        if owner.state().lifecycle_of(task)?.execution()!=Some(&execution){return Err(DomainError::new("stale_execution","The task's execution changed; the change of role waits for the next look").into())}
        let source=owner.selection_source(task,host_directory)?;
        owner.stage_command(&Actor::System,&command,Attached::None)?;
        owner.request_provider_transition(&Plan{id:id.clone(),kind:Kind::Stage,task:task.clone(),expected_selection,source,target_account:decision.account,target_model:decision.selection,intent,reason,conversation_file:None,role:Some(handover.to)})
    }).map_err(store_failure)?;
    log!("{task} hands over to its {}: {id}",handover.to.as_str());
    Ok(true)
}
/// What the measures keep of a stage's new execution: its model, provider and why, by the agent
/// that runs it, whose role the task's record says.
fn record_stage_launch(domain:&dyn Runtime,plan:&Plan){
    let read=domain.read();
    let Some(task)=read.state().tasks.get(&plan.task) else{return};
    let decision=crate::domain_store::model_metrics::Decision{
        task_id:task.id.0.clone(),request_id:task.request_id.0.clone(),agent_id:task.agent_id.as_ref().map(|agent|agent.0.clone()),at:alondra_core::now_millis(),
        kind:crate::domain_store::model_metrics::DecisionKind::Launch,model:Some(plan.target_model.model.wire_id()),effort:plan.target_model.reasoning_effort.clone(),
        approach:None,chosen_by:alondra_domain_rules::ModelChooser::Orchestrator,reason:Some(plan.reason.clone()),title:task.title.clone(),provider:Some(plan.target_account.provider),tags:None,
    };
    drop(read);
    if let Err(error)=domain.record_model_decision(&decision){log!("could not record the model of {}'s new stage: {error:#}",plan.task);}
}
/// Whether the execution a stage leaves rests with nothing pending: no turn, no work of its own,
/// no decision or question it waits on, no input already sent to it.
fn stage_drained(domain:&dyn Runtime,plan:&Plan)->Result<bool,DomainError>{
    let state=domain.state();
    Ok(drained(domain,plan)? && state.lifecycle_of(&plan.task)?.cards().is_empty() && !state.asks_open(&plan.task,&plan.source.agent))
}
fn prepare_context(domain:&dyn Runtime,plan:&Plan)->Result<PreparedHandoff,DomainError>{
    domain.own(scope(domain)?,|owner|{
        let prepared=handoff(owner,plan)?;
        if let Some(through)=&prepared.coverage.through{owner.pin_context(&plan.id,through)?;}
        owner.provider_transition_phase(&plan.id,Phase::SourceFenced,Phase::ContextReady,serde_json::to_value(&prepared).map_err(failure)?)?;
        Ok(prepared)
    }).map_err(store_failure).map(|(value,_)|value)
}
/// Starts the task's execution in `host_directory` again as a new session of its model and
/// account, in the owner's transaction that decided it: the new session carries on from the task's
/// records and reads the conversation before it from `conversation_file` when it needs to. The
/// task keeps a pending selection equal to its own until the new session took its input, so every
/// hold of a selection change holds it meanwhile. Nothing is requested when the handoff could not
/// be prepared now.
pub(crate) fn request_refresh(owner:&mut dyn OwnerJournal,task:&TaskId,host_directory:std::path::PathBuf,conversation_file:std::path::PathBuf)->Result<String,StoreError>{
    let source=owner.selection_source(task,host_directory)?;
    let intent=source.selection.clone().ok_or_else(||failure("The task has no model selection"))?;
    let target_model=intent.effective.clone().ok_or_else(||failure("The execution's model is unknown"))?;
    let plan=Plan{id:format!("refresh-{}",crate::auth::random_hex(16)),kind:Kind::Refresh,task:task.clone(),expected_selection:owner.state().task_selection_stamp(task).unwrap_or_default(),
        target_account:source.account.clone(),target_model,intent,reason:"The prompt cache of the conversation expired".into(),conversation_file:Some(conversation_file),role:None,source};
    handoff(owner,&plan)?;
    owner.validate_selection_source(&plan)?;
    let pending=PendingTaskSelection{account:plan.target_account.clone(),model:plan.target_model.clone(),reason:plan.reason.clone()};
    let command=CommandEnvelope{protocol_version:PROTOCOL_VERSION,data_epoch:owner.state().data_epoch.clone(),command_id:format!("{}-pending",plan.id).as_str().into(),expected_revision:0,
        command:Command::RecordTaskSelection{task_id:task.clone(),selection:plan.intent.clone(),pending:Some(pending)}};
    owner.stage_command(&Actor::System,&command,Attached::None)?;
    owner.request_provider_transition(&plan)?;
    Ok(plan.id)
}
/// Ends the execution a task moves from and seals it: its host stops, what it did is committed
/// with its end, and the ended host is the proof that nothing of it writes any more.
fn end_source(domain:&Arc<dyn Runtime>,plan:&Plan)->Result<(),DomainError>{
    domain.own(scope(&**domain)?,|owner|owner.validate_selection_binding(plan)).map_err(store_failure)?;
    let alive=!matches!(alondra_engine_ports::agent_host::of(domain.ports()).and_then(|hosts|hosts.claim(&plan.source.host_directory)),Ok(Retained::Ended(_)));
    if alive || !host_ended(&**domain,&plan.task)? || crate::sdk_owner::claims_left(&**domain,&plan.task).map_err(failure)?{end_and_fence(domain,plan)?;}
    ensure_fence(&**domain,plan)?;
    let fences=domain.selection_changes().fences.lock_or_recover("the provider transition fences");
    domain.own(scope(&**domain)?,|owner|owner.source_fenced(plan,fences.get(&plan.id).unwrap())).map_err(store_failure).map(drop)
}
/// Whether the end of the task's current execution is already committed. A host that resumed the
/// same execution after it may still run: only the host's lock says whether one does.
fn host_ended(domain:&dyn Runtime,task:&TaskId)->Result<bool,DomainError>{
    let read=domain.read();
    let journal=read.state().lifecycle_journal.get(task).ok_or_else(||failure("The task has no lifecycle"))?;
    let admitted=journal.iter().rposition(|record|matches!(record,LifecycleRecord::Admitted{..})).unwrap_or(0);
    Ok(journal.iter().skip(admitted).any(|record|matches!(record,LifecycleRecord::HostEnded{..}|LifecycleRecord::HostStopped{..}|LifecycleRecord::ExecutionReleased{..})))
}
/// The move ends now, before its target starts: the task leaves its execution and keeps the
/// selection it moved with, and its next node starts on the new account from the prepared context
/// when something resumes it. A paused task stays paused.
fn defer(domain:&dyn Runtime,change:&Transition)->Result<(),DomainError>{
    let plan=&change.plan;
    domain.own(scope(domain)?,|owner|{
        owner.stage_command(&Actor::System,&envelope(domain,&format!("{}-deferred",plan.id),Command::DeferTaskMove{task_id:plan.task.clone(),account:plan.target_account.clone()}),Attached::None)?;
        owner.provider_transition_phase(&plan.id,Phase::ContextReady,Phase::Done,json!({"deferred_start":true,"context":change.facts}))
    }).map_err(store_failure).map(drop)
}
fn start_target(domain:&Arc<dyn Runtime>,plan:&Plan,prepared:PreparedHandoff)->Result<(),DomainError>{
    let task=&plan.task;
    let (backend,_)=domain.sdk().ok_or_else(||failure("Provider runtime missing"))?;
    let plugin=domain.ports().plugins().node(domain.home()).filter(|plugin|plugin.is_dir()).or_else(||Some(domain.home().join(crate::plugins::NODE)).filter(|plugin|plugin.is_dir())).ok_or_else(||failure("Alondra's task plugin is unavailable; run alondra doctor --fix"))?;
    let launch=crate::sdk_owner::Launch{task:task.clone(),name:domain.read().state().tasks[task].title.clone(),directory:plan.source.directory.clone(),prompt:prepared.new_input,plugin,options:alondra_daemon_engine_model::LaunchOptions{provider:plan.target_account.provider,selection:Some(applied(plan)),model:Some(plan.target_model.model.model.clone()),effort:plan.target_model.reasoning_effort.clone(),disallowed_tools:plan.source.disallowed_tools.clone()},model:None,account:plan.target_account.account.clone()};
    let driver=crate::sdk_owner::continue_session(Arc::clone(domain),backend,launch,crate::sdk_owner::Continuation::Transition{id:plan.id.clone()})
        .map_err(|failed|failed.refusal.unwrap_or_else(||failure(failed.why)))?;
    domain.add_sdk_owner(crate::sdk_owner::Owner::spawn(driver));
    Ok(())
}
/// Starts on its new account the task a move left waiting, once it has something to go on with.
pub(crate) async fn start_moved(domain:&Arc<dyn Runtime>,task:&TaskId)->Result<bool,DomainError>{
    let _advance=domain.selection_changes().advancing.lock().await;
    if !starts_elsewhere(&**domain,task) || !ready_to_start(&**domain,task)? || !admitted(&**domain,task){return Ok(false)}
    let change=domain.store().lock_or_exit("the domain store").deferred_move(task).map_err(failure)?.ok_or_else(||failure("The move is missing"))?;
    let prepared=serde_json::from_value(prepared_facts(change.clone())).map_err(failure)?;
    let started=current_target_model(&**domain,&change.plan).and_then(|target_model|start_target(domain,&Plan{target_model,..change.plan.clone()},prepared));
    match started{
        Err(error) if REFUSED_STARTS.contains(&error.code.as_str())=>{refuse_start(&**domain,&change,&error.message)?;Ok(false)}
        started=>started.map(|()|true),
    }
}
/// The model a moved task starts with, read again from its account's catalog: the catalog may have
/// been refreshed since the move resolved it, and a selection of an older catalog is refused.
fn current_target_model(domain:&dyn Runtime,plan:&Plan)->Result<ModelSelection,DomainError>{
    let read=domain.read();
    let catalog=read.state().model_catalogs.iter().find(|catalog|catalog.account==plan.target_account)
        .ok_or_else(||DomainError::new("model_catalog_unavailable","Refresh the models of the account the task moved to"))?;
    catalog.resolve(&alondra_core::model_selection::ModelCandidate{model:plan.target_model.model.model.clone(),reasoning_effort:plan.target_model.reasoning_effort.clone()})
        .map_err(|reason|DomainError::new("model_unavailable",reason))
}
/// The refusals a moved task's start gets again however often it is tried: its context does not
/// fit the destination, its model is no longer offered there, or the account cannot take it until
/// the user acts.
const REFUSED_STARTS:[&str;4]=["handoff_budget","model_unavailable","account_unavailable","paid_authorization_required"];
/// Ends the start of a moved task that is refused for good: the move waits at `StartRefused`, the
/// task says why it did not move, and a card asks the user to try again or cancel the task.
fn refuse_start(domain:&dyn Runtime,change:&Transition,reason:&str)->Result<(),DomainError>{
    let plan=&change.plan;
    let reason:String=reason.chars().take(1900).collect();
    log!("{} cannot start on {}; it waits for the user: {reason}",plan.task,plan.target_account.wire_id());
    domain.own(scope(domain)?,|owner|{
        let selection=owner.state().tasks.get(&plan.task).ok_or_else(||failure("Task missing"))?.selection.clone();
        let refused=MoveRefused{account:plan.target_account.clone(),reason:reason.clone()};
        owner.stage_command(&Actor::System,&envelope(domain,&format!("{}-start-refused-{}",plan.id,alondra_core::now_millis()),Command::AbortTaskSelection{task_id:plan.task.clone(),selection,refused:Some(refused)}),Attached::None)?;
        owner.stage_command(&Actor::System,&envelope(domain,&format!("{}-start-problem-{}",plan.id,alondra_core::now_millis()),Command::RaiseStartProblem{task_id:plan.task.clone(),problem:StartProblem::MoveStartRefused{account:plan.target_account.clone(),error:reason.clone()}}),Attached::None)?;
        owner.provider_transition_phase(&plan.id,Phase::Done,Phase::StartRefused,json!({"reason":reason,"context":change.facts["context"]}))
    }).map_err(store_failure).map(drop)
}
/// The user chose to try again the refused start of a moved task: its move goes back to waiting
/// for its target to start, and the next pass tries it once more.
fn retry_refused(domain:&dyn Runtime,change:&Transition)->Result<bool,DomainError>{
    let retry={
        let read=domain.read();let state=read.state();
        state.open_start_problem(&change.plan.task).is_none() && state.last_start_problem(&change.plan.task).is_some_and(|card|!card.withdrawn && matches!(&card.payload,
            InteractionPayload::Question{answer:Some(answer),..} if answer.option_id==START_RETRY)
            && matches!(alondra_domain_rules::start_problem::from_body(&card.body),Some(StartProblem::MoveStartRefused{..})))
    };
    if !retry{return Ok(false)}
    log!("{} tries again to start on {}, as the user asked",change.plan.task,change.plan.target_account.wire_id());
    phase(domain,change,Phase::Done,json!({"deferred_start":true,"context":change.facts["context"]}))?;
    Ok(true)
}
/// Whether the gate lets the moved task's next node start, as it lets a queued task: a free slot,
/// the disk's floor and the Mac's memory hold it until there is room.
fn admitted(domain:&dyn Runtime,task:&TaskId)->bool{
    let Some(priority)=domain.read().state().tasks.get(task).map(|task|task.priority) else{return false};
    let now=alondra_core::now_millis();
    domain.gate().ask(alondra_engine_ports::admission::Waiting{key:alondra_engine_ports::admission::Key::Task(task.clone()),priority,resuming:false,since:now},now).is_ok()
}
/// Whether the task has something to go on with and nobody else to wait for: the moment its next
/// node starts, as the node it moved from would have resumed then. A delivery goes on once the
/// user sent it back.
fn ready_to_start(domain:&dyn Runtime,task:&TaskId)->Result<bool,DomainError>{
    ready_in(domain.read().state(),task)
}
/// `ready_to_start` over a state: the decision the daemon and the whole-machine model share.
pub(crate) fn ready_in(state:&DomainState,task:&TaskId)->Result<bool,DomainError>{
    use alondra_domain::{Capacity,LiveState,Owed,TaskPhase};
    let lifecycle=state.lifecycle_of(task)?;
    let goes_on=match lifecycle.phase(){
        TaskPhase::InProgress=>matches!(lifecycle.live(),Some(LiveState::Working(_)|LiveState::WaitingCapacity(Capacity::Gate))),
        TaskPhase::Delivered=>lifecycle.obligation().is_some_and(|obligation|obligation.owed==Owed::Start),
        _=>false,
    };
    Ok(!lifecycle.pending_inputs().is_empty() && goes_on)
}
/// How a move whose source has ended goes on: its target starts now, or the move ends at once and
/// the target starts when something resumes the task. A move never waits for the task to be ready.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum MoveStart{Start,Defer}
pub(crate) fn move_start(state:&DomainState,task:&TaskId)->Result<MoveStart,DomainError>{
    Ok(if ready_in(state,task)?{MoveStart::Start}else{MoveStart::Defer})
}
/// Stops the source's host and commits its end, keeping as the fence the folder the end was read
/// from, still locked: a lock released and taken again can be held meanwhile by a child this
/// daemon forked that has not exec'd yet, which would read as a host that still runs.
fn end_and_fence(domain:&Arc<dyn Runtime>,plan:&Plan)->Result<(),DomainError>{
    let (backend,_)=domain.sdk().ok_or_else(||failure("The provider runtime is unavailable"))?;
    domain.stop_sdk_host(&plan.source.agent).map_err(failure)?;
    let claimed=crate::sdk_owner::end_and_claim_unowned_host(Arc::clone(domain),backend,plan.task.clone()).map_err(failure)?;
    if let Some(proof)=claimed.filter(|proof|proof.state_dir()==plan.source.host_directory){
        domain.selection_changes().fences.lock_or_recover("the provider transition fences").insert(plan.id.clone(),proof);
    }
    ensure_fence(&**domain,plan)
}
fn ensure_fence(domain:&dyn Runtime,plan:&Plan)->Result<(),DomainError>{
    if domain.selection_changes().fences.lock_or_recover("the provider transition fences").contains_key(&plan.id){return Ok(())}
    let retained=alondra_engine_ports::agent_host::of(domain.ports()).and_then(|hosts|hosts.claim(&plan.source.host_directory)).map_err(failure)?;
    let proof=match retained{Retained::Ended(proof)=>proof,Retained::Held=>return Err(failure("The source host still holds its native execution")),Retained::Absent=>return Err(failure("The source host fence is missing; its absence is not continuation proof"))};
    domain.selection_changes().fences.lock_or_recover("the provider transition fences").insert(plan.id.clone(),proof);
    Ok(())
}

pub async fn advance(domain:&Arc<dyn Runtime>,task:&TaskId)->Result<Phase,DomainError>{
    let _advance=domain.selection_changes().advancing.lock().await;
    loop{
        let change=current(&**domain,task)?.ok_or_else(||failure("No selection change is pending"))?;
        if *domain.selection_changes().crash_at.lock_or_recover("the transition's crash point")==Some(change.phase){return Err(failure("the daemon died"))}
        let plan=&change.plan;
        if plan.kind==Kind::Model && plan.target_account!=plan.source.account{return Err(failure("The account and provider of this native session cannot change; create a new node"))}
        if plan.kind==Kind::Model && matches!(change.phase,Phase::SourceFenced|Phase::ContextReady|Phase::TargetAdmitted|Phase::Consumed){return Err(failure("Session transfer is disabled; the node keeps its original account and native history"))}

        if matches!(change.phase,Phase::Requested|Phase::Draining) && matches!(domain.read().state().lifecycle_of(task)?.phase(),alondra_domain::TaskPhase::Finishing|alondra_domain::TaskPhase::Completed|alondra_domain::TaskPhase::Cancelled){
            phase(&**domain,&change,Phase::Aborted,json!({"task_ended":true}))?;
            return Ok(Phase::Aborted)
        }
        match change.phase{
            Phase::Requested=>match classify(plan){
                SelectionTransition::Reject(reason)=>return Err(failure(reason)),
                SelectionTransition::Keep=>{apply_selection(&**domain,plan,Phase::Requested,Phase::Done,json!({"unchanged_execution":true}))?;return Ok(Phase::Done)},
                SelectionTransition::ApplyNextTurn=>phase(&**domain,&change,Phase::Draining,json!({}))?,
                SelectionTransition::CreateWithHandoff if matches!(plan.kind,Kind::Move|Kind::Refresh|Kind::Stage)=>phase(&**domain,&change,Phase::Draining,json!({}))?,
                _=>return Err(failure("This change requires a new native session; create a new node instead")),
            },
            Phase::Draining if plan.kind==Kind::Stage=>{
                if crate::sdk_owner::being_retaken(&**domain,task){return Ok(Phase::Draining)}
                if !domain.owns_sdk_host(&plan.source.agent){
                    let (backend,_)=domain.sdk().ok_or_else(||failure("The provider runtime is unavailable"))?;
                    let plugin=domain.home().join(crate::plugins::NODE);
                    if crate::sdk_owner::adopt_for_transition(Arc::clone(domain),backend,task.clone(),Some(&plugin)).map_err(failure)?{return Ok(Phase::Draining)}
                }
                let ended=matches!(alondra_engine_ports::agent_host::of(domain.ports()).and_then(|hosts|hosts.claim(&plan.source.host_directory)),Ok(Retained::Ended(_)));
                if !ended && !stage_drained(&**domain,plan)?{return Ok(Phase::Draining)}
                end_source(domain,plan)?
            }
            Phase::Draining if matches!(plan.kind,Kind::Move|Kind::Refresh)=>{
                if crate::sdk_owner::being_retaken(&**domain,task){return Ok(Phase::Draining)}
                end_source(domain,plan)?
            }
            Phase::Draining=>{
                if matches!(classify(plan), SelectionTransition::ApplyNextTurn) && abort_unapplied_model_change(&**domain, &change)? { return Ok(Phase::Aborted) }
                domain.own(scope(&**domain)?,|owner|owner.validate_selection_source(plan)).map_err(store_failure)?;
                if !drained(&**domain,plan)? {
                    if !domain.owns_sdk_host(&plan.source.agent){
                        let (backend,_)=domain.sdk().ok_or_else(||failure("Provider runtime missing"))?;
                        let plugin=domain.home().join(crate::plugins::NODE);
                        if crate::sdk_owner::adopt_for_transition(Arc::clone(domain),backend,task.clone(),Some(&plugin)).map_err(failure)?{return Ok(Phase::Draining)}
                        if !drained(&**domain,plan)?{return Err(failure("The source ended with unresolved work or delivery; its native input cannot be repeated"))}
                    }else{return Ok(Phase::Draining)}
                }
                if !domain.owns_sdk_host(&plan.source.agent){
                    let (backend,_)=domain.sdk().ok_or_else(||failure("The native runtime is unavailable"))?;
                    let plugin=domain.ports().plugins().node(domain.home()).ok_or_else(||failure("The node plugin is unavailable; run alondra doctor --fix"))?;
                    if !crate::sdk_owner::adopt_for_transition(Arc::clone(domain),backend,task.clone(),Some(&plugin)).map_err(failure)?{return Err(failure("The existing native session cannot be adopted; create a new node instead of transferring it"))}
                    return Ok(Phase::Draining)
                }
                let available=domain.read().state().lifecycle_of(task)?.resource()==Some(alondra_domain::ResourceEvidence::Available);
                if available && matches!(classify(plan),SelectionTransition::ApplyNextTurn){
                    let handed=match domain.own(scope(&**domain)?,|owner|owner.configure_transition_model(plan)).map_err(store_failure) {
                        Ok((handed, _)) => handed,
                        Err(error) if error.code == "native_model_control_rejected" => {
                            if abort_unapplied_model_change(&**domain, &change)? { return Ok(Phase::Aborted) }
                            return Err(error)
                        }
                        Err(error) => return Err(error),
                    };
                    if !handed{return Ok(Phase::Draining)}
                    crate::sdk_owner::retain_selection_options(&plan.source.host_directory,applied(plan)).map_err(failure)?;
                    apply_selection(&**domain,plan,Phase::Draining,Phase::TargetReady,json!({"same_host":true}))?;
                    let now=current(&**domain,task)?.unwrap();phase(&**domain,&now,Phase::Done,json!({"same_host":true}))?;return Ok(Phase::Done)
                }
                if !available || !matches!(classify(plan),SelectionTransition::ApplyNextTurn){return Err(failure("The existing native session cannot apply this model change; create a new node"))}
                end_and_fence(domain,plan)?;
                let fences=domain.selection_changes().fences.lock_or_recover("the provider transition fences");
                domain.own(scope(&**domain)?,|owner|owner.source_fenced(plan,fences.get(&plan.id).unwrap())).map_err(store_failure)?;
            }
            Phase::SourceFenced=>{ensure_fence(&**domain,plan)?;prepare_context(&**domain,plan)?;}
            Phase::ContextReady=>{
                ensure_fence(&**domain,plan)?;
                match if plan.kind==Kind::Move{move_start(domain.read().state(),task)?}else{MoveStart::Start}{
                    MoveStart::Defer=>{defer(&**domain,&change)?;return Ok(Phase::Done)}
                    MoveStart::Start=>{
                        if plan.kind==Kind::Stage && !domain.gate().may_take_turn(task,alondra_core::now_millis()){return Ok(Phase::ContextReady)}
                        start_target(domain,plan,serde_json::from_value(change.facts.clone()).map_err(failure)?)?;
                        if plan.kind==Kind::Stage{record_stage_launch(&**domain,plan);}
                        return Ok(Phase::TargetAdmitted)
                    }
                }
            }
            Phase::TargetReady if change.facts["same_host"].as_bool()==Some(true)=>{phase(&**domain,&change,Phase::Done,change.facts.clone())?;return Ok(Phase::Done)}
            Phase::TargetAdmitted|Phase::TargetReady=>{
                ensure_fence(&**domain,plan)?;
                if let Some(agent)=domain.read().state().tasks.get(task).and_then(|task|task.agent_id.clone()) && !domain.owns_sdk_host(&agent){crate::sdk_owner::retake(&**domain,task.clone());}
                return Ok(change.phase)
            }
            Phase::Consumed=>{phase(&**domain,&change,Phase::Done,change.facts.clone())?;}
            Phase::Done|Phase::Aborted|Phase::StartRefused=>{domain.selection_changes().fences.lock_or_recover("the provider transition fences").remove(&plan.id);return Ok(change.phase)}
            Phase::Uncertain=>{
                abort(&**domain,task,change.facts["reason"].as_str().unwrap_or("The move was left without an outcome"))?;
                return Ok(Phase::Aborted)
            }
        }
    }
}

pub async fn run(domain:Arc<dyn Runtime>){
    let mut changed=domain.subscribe();
    loop{
        pass(&domain).await;
        tokio::select!{result=changed.changed()=>if result.is_err(){return},_=clock::sleep(std::time::Duration::from_secs(10))=>{}}
    }
}

/// One look at every selection change: each advances as far as it can, ends aborted when a step
/// fails, and a task a move left waiting starts on its new account once it can.
pub async fn pass(domain:&Arc<dyn Runtime>){
    release_finished_fences(&**domain);
    let owed:Vec<TaskId>=domain.read().state().tasks.values().filter(|task|task.stages.handover.as_ref().is_some_and(StageHandover::due)).map(|task|task.id.clone()).collect();
    for task in owed{
        if let Err(error)=request_stage(domain,&task){log!("the change of role of {task} could not start: {}",error.message);}
    }
    let pending=domain.store().lock_or_exit("the domain store").provider_transitions();
    for change in pending.unwrap_or_default(){
        if let Err(error)=advance(domain,&change.plan.task).await{
            log!("selection change of {} could not finish: {}",change.plan.task,error.message);
            if let Err(failed)=abort(&**domain,&change.plan.task,&error.message){log!("selection change of {} could not be aborted: {}",change.plan.task,failed.message);}
        }
    }
    let refused=domain.store().lock_or_exit("the domain store").refused_moves();
    for change in refused.unwrap_or_default(){
        if let Err(error)=retry_refused(&**domain,&change){log!("the refused start of {} could not be tried again: {}",change.plan.task,error.message);}
    }
    let moved=domain.store().lock_or_exit("the domain store").deferred_moves();
    for change in moved.unwrap_or_default(){
        if let Err(error)=start_moved(domain,&change.plan.task).await{log!("{} could not start on {}: {}",change.plan.task,change.plan.target_account.wire_id(),error.message);}
    }
}

pub(crate) fn release_finished_fences(domain:&dyn Runtime){
    let ids:Vec<String>=domain.selection_changes().fences.lock_or_recover("the provider transition fences").keys().cloned().collect();
    for id in ids{
        let ended=domain.store().lock_or_exit("the domain store").provider_transition_finished(&id).unwrap_or(false);
        if ended{domain.selection_changes().fences.lock_or_recover("the provider transition fences").remove(&id);}
    }
}
/// Ends a selection change that cannot finish: the task keeps the selection it had before, nothing
/// stays pending, and a move says on the task why it did not happen.
pub(crate) fn abort(domain:&dyn Runtime,task:&TaskId,reason:&str)->Result<(),DomainError>{
    let Some(change)=current(domain,task)? else{return Ok(())};
    let plan=&change.plan;
    let reason:String=reason.chars().take(1900).collect();
    domain.own(scope(domain)?,|owner|{
        if let Some(held)=owner.state().tasks.get(task).filter(|held|held.pending_selection.is_some()){
            let selection=if held.account_key()==plan.source.account{plan.source.selection.clone()}else{held.selection.clone()};
            let refused=(plan.kind==Kind::Move).then(||MoveRefused{account:plan.target_account.clone(),reason:reason.clone()});
            owner.stage_command(&Actor::System,&envelope(domain,&format!("{}-aborted",plan.id),Command::AbortTaskSelection{task_id:task.clone(),selection,refused}),Attached::None)?;
        }
        owner.release_context(&plan.id,task)?;
        if plan.kind==Kind::Stage{
            owner.stage_command(&Actor::System,&envelope(domain,&format!("{}-failed",plan.id),Command::StageHandoverFailed{task_id:task.clone(),transition:plan.id.clone(),refused:false}),Attached::None)?;
            if let (Some(role),true)=(plan.role,owner.state().tasks.get(task).and_then(|held|held.stages.handover.as_ref()).is_some_and(|handover|handover.refused)){
                owner.stage_command(&Actor::System,&envelope(domain,&format!("{}-start-problem",plan.id),Command::RaiseStartProblem{task_id:task.clone(),problem:StartProblem::StageStartRefused{role,error:reason.clone()}}),Attached::None)?;
            }
        }
        owner.provider_transition_phase(&plan.id,change.phase,Phase::Aborted,json!({"previous_phase":change.phase,"reason":reason}))
    }).map_err(store_failure)?;
    domain.selection_changes().fences.lock_or_recover("the provider transition fences").remove(&plan.id);
    Ok(())
}

alondra_engine_kernel::extension!(Transitions);

pub trait TransitionsOf {
    fn selection_changes(&self) -> &Transitions;
}

impl<R: alondra_engine_ports::Runtime + ?Sized> TransitionsOf for R {
    fn selection_changes(&self) -> &Transitions {
        alondra_engine_ports::extension(self.as_runtime())
    }
}
