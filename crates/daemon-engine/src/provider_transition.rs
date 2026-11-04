//! A requested selection stays durable while its current execution drains, is fenced and hands off.
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::{Attached,StoreError};
use crate::domain_store::provider_transition::{Kind,Plan,Phase,Transition};
use alondra_agent_runtime::owned_host::{EndedHost,Retained};
use alondra_core::accounts::AccountKey;
use alondra_core::domain::*;
use alondra_core::model_selection::{ModelSelection,PendingTaskSelection,TaskSelection};
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
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) crash_at:Mutex<Option<Phase>>,
}
#[cfg(any(test, feature = "test-hooks"))]
impl Transitions {
    /// What a daemon that dies lets go of: the ended hosts it held as proof.
    pub(crate) fn die(&self){*self.crash_at.lock().unwrap()=None;self.fences.lock().unwrap().clear();}
}
fn failure(error:impl std::fmt::Display)->DomainError{DomainError::new("selection_transition",error.to_string())}
fn store_failure(error:StoreError)->DomainError{match error{StoreError::Domain(error)=>error,StoreError::Storage(error)=>failure(error)}}
fn scope(domain:&DomainRuntime)->Result<AuthorityScope,DomainError>{AuthorityScope::new(domain.read().state().data_epoch.clone()).map_err(failure)}
fn envelope(domain:&DomainRuntime,id:&str,command:Command)->CommandEnvelope{CommandEnvelope{protocol_version:PROTOCOL_VERSION,data_epoch:domain.read().state().data_epoch.clone(),command_id:id.into(),expected_revision:0,command}}
fn current(domain:&DomainRuntime,task:&TaskId)->Result<Option<Transition>,DomainError>{domain.store.lock_or_exit("the domain store").provider_transition(task).map_err(failure)}

pub(crate) fn denied_tools(domain:&DomainRuntime,task:&TaskId)->Result<Vec<String>,DomainError>{
    let state=domain.state().map_err(failure)?;
    let task_data=state.tasks.get(task).ok_or_else(||DomainError::new("not_found","Task missing"))?;
    let execution=state.lifecycle_of(task)?.execution().cloned().ok_or_else(||failure("The task has no managed execution"))?;
    let (backend,_)=domain.sdk().ok_or_else(||failure("The provider runtime is unavailable"))?;
    let selected=backend.for_provider(task_data.provider,domain.home()).map_err(failure)?;
    crate::sdk_owner::LaunchOptions::load(&selected.state_root.join(execution.instance().as_str())).map(|options|options.disallowed_tools).map_err(failure)
}

pub fn request(domain:&DomainRuntime,actor:&Actor,task:&TaskId,target_account:AccountKey,target_model:ModelSelection,intent:TaskSelection,reason:&str,expected_selection:&str,authorize:impl FnOnce()->Result<(),DomainError>)->Result<Value,DomainError>{
    let initial=domain.state().map_err(failure)?;
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
        if target_account.provider==alondra_core::accounts::Provider::Codex && !source.disallowed_tools.is_empty(){return Err(DomainError::new("unsupported_tool_policy","Codex cannot enforce this task's tool deny list; keep the current provider or explicitly revise the task policy").into())}
        owner.stage_command(actor,&command,Attached::None)?;
        owner.request_provider_transition(&Plan{id:id.clone(),kind:Kind::Model,task:task.clone(),expected_selection:expected_selection.into(),source,target_account,target_model,intent,reason:reason.into()})
    }).map_err(store_failure)?;
    Ok(json!({"task_id":task,"transition":id,"status":"pending"}))
}

/// Moves the task to `account`. A task that already ran keeps the move open while its execution
/// ends, and a new node on `account` takes it over from its record; one that never ran just starts
/// there. The command and the move's first step commit together, or neither does.
pub fn request_move(domain:&DomainRuntime,actor:&Actor,task:&TaskId,account:AccountKey,expected_selection:&str,authorize:impl FnOnce()->Result<(),DomainError>)->Result<Value,DomainError>{
    let initial=domain.state().map_err(failure)?;
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
        owner.request_provider_transition(&Plan{id:id.clone(),kind:Kind::Move,task:task.clone(),expected_selection:expected_selection.into(),source,target_account:account,target_model:pending.model,intent,reason:pending.reason})
    }).map_err(store_failure)?;
    Ok(json!({"task_id":task,"transition":id,"status":"pending"}))
}

/// Whether a move holds the task: no node but the one it moves to may start for it.
pub(crate) fn moving(domain:&DomainRuntime,task:&TaskId)->bool{
    domain.read().state().moving_to(task).is_some() && matches!(current(domain,task),Ok(Some(change)) if change.plan.kind==Kind::Move)
}

pub(crate) fn holds_source_inputs(domain:&DomainRuntime,task:&TaskId,instance:&str)->bool{
    if domain.read().state().tasks.get(task).is_none_or(|task|task.pending_selection.is_none()){return false}
    match current(domain,task){Ok(Some(change))=>change.plan.source.instance==instance,Ok(None)=>false,Err(_)=>true}
}
pub(crate) fn holds_execution(domain:&DomainRuntime,task:&TaskId)->bool{
    let read=domain.read();let instance=read.state().lifecycle_of(task).ok().and_then(|lifecycle|lifecycle.execution().map(|stamp|stamp.instance().as_str().to_owned()));
    instance.is_some_and(|instance|holds_source_inputs(domain,task,&instance))
}
fn phase(domain:&DomainRuntime,change:&Transition,next:Phase,facts:Value)->Result<(),DomainError>{domain.own(scope(domain)?,|owner|owner.provider_transition_phase(&change.plan.id,change.phase,next,facts)).map_err(store_failure).map(drop)}
fn applied(plan:&Plan)->TaskSelection{let mut selected=plan.intent.clone();selected.effective=Some(plan.target_model.clone());selected}
fn apply_selection(domain:&DomainRuntime,plan:&Plan,from:Phase,to:Phase,facts:Value)->Result<(),DomainError>{
    domain.own(scope(domain)?,|owner|{
        owner.validate_selection_source(plan)?;
        owner.stage_command(&Actor::System,&envelope(domain,&format!("{}-effective",plan.id),Command::RecordTaskSelection{task_id:plan.task.clone(),selection:applied(plan),pending:None}),Attached::None)?;
        owner.provider_transition_phase(&plan.id,from,to,facts)
    }).map_err(store_failure).map(drop)
}
fn abort_unapplied_model_change(domain: &DomainRuntime, change: &Transition) -> Result<bool, DomainError> {
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
            body: code.into(), attachments: vec![], card: CardSpec::derived(CardKind::Know, "The model change was not applied"),
        }), Attached::None)?;
        owner.provider_transition_phase(&plan.id, change.phase, Phase::Aborted, json!({"reason":code,"model_applied":false}))?;
        Ok(true)
    }).map_err(store_failure).map(|(aborted, _)| aborted)
}
fn classify(plan:&Plan)->SelectionTransition{
    if plan.kind==Kind::Move{return SelectionTransition::CreateWithHandoff}
    if plan.target_account!=plan.source.account{return SelectionTransition::Reject("An active node keeps its account and provider".into())}
    let Some(model)=plan.source.selection.as_ref().and_then(|selection|selection.effective.clone()) else{return SelectionTransition::CreateWithHandoff};
    let current=ActiveSelection{account:plan.source.account.clone(),model,configuration:plan.source.configuration.clone()};
    let target=ActiveSelection{account:plan.target_account.clone(),model:plan.target_model.clone(),configuration:if plan.target_account==plan.source.account{plan.source.configuration.clone()}else{plan.target_account.wire_id()}};
    let capabilities=TransitionCapabilities{turn_scoped_model:plan.source.turn_model,turn_scoped_reasoning:false,native_resume:false,pinned_account:plan.intent.account.clone(),requested_model:plan.intent.requested.clone(),requested_reasoning_effort:plan.intent.requested_reasoning_effort.clone(),allow_model_substitution:plan.intent.allow_model_substitution,..Default::default()};
    classify_transition(&current,&target,&capabilities,None)
}
pub(crate) fn retained_handoff(domain:&DomainRuntime,task:&TaskId,id:&str)->Result<PreparedHandoff,DomainError>{
    let change=current(domain,task)?.ok_or_else(||failure("The transfer context is missing"))?;
    if change.plan.id!=id || change.phase!=Phase::ContextReady{return Err(failure("The transfer is not ready for its target"))}
    serde_json::from_value(change.facts).map_err(failure)
}
fn drained(domain:&DomainRuntime,plan:&Plan)->Result<bool,DomainError>{
    let state=domain.state().map_err(failure)?;let lifecycle=state.lifecycle_of(&plan.task)?;
    let unoffered=domain.own(scope(domain)?,|owner|owner.source_inputs_unoffered(&plan.task)).map_err(store_failure)?.0;
    Ok(unoffered && lifecycle.execution().is_some_and(|stamp|stamp.instance().as_str()==plan.source.instance) && lifecycle.turn().is_none()
        && !matches!(lifecycle.activity(),ActivityView::Working|ActivityView::OwnWork)
        && lifecycle.cards().iter().all(|(card,_)|lifecycle.card_callback(card).is_none()) && lifecycle.doubts().is_empty())
}
/// The task's canonical context, or why it cannot move: something of it did not fit or was cut.
pub(crate) fn critical_context(state:&DomainState,task:&alondra_core::domain::Task)->Result<String,DomainError>{
    let canonical=crate::domain_orchestrator_tools::canonical_task_context(state,task);
    if !canonical.complete{return Err(failure(format!("Critical task context is incomplete: {}",canonical.omitted_sources.join(", "))))}
    Ok(canonical.text)
}
fn prepare_context(domain:&DomainRuntime,plan:&Plan)->Result<PreparedHandoff,DomainError>{
    domain.own(scope(domain)?,|owner|{
        let task=owner.state().tasks.get(&plan.task).ok_or_else(||failure("Task missing"))?;
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
        let prepared=alondra_store::prepare_context_handoff(&canonical,"Continue this Alondra task from its public records. The user changed its provider or model. Preserve completed work and answer through Alondra.",&entries,&coverage,None,HandoffBudget{destination_context_tokens:window,reserved_tokens:4096}).map_err(|error|failure(format!("The task context cannot be transferred: {error:?}")))?;
        if let Some(through)=&prepared.coverage.through{owner.pin_context(&plan.id,through)?;}
        owner.provider_transition_phase(&plan.id,Phase::SourceFenced,Phase::ContextReady,serde_json::to_value(&prepared).map_err(failure)?)?;
        Ok(prepared)
    }).map_err(store_failure).map(|(value,_)|value)
}
/// Ends the execution a task moves from and seals it: its host stops, what it did is committed
/// with its end, and the ended host is the proof that nothing of it writes any more.
fn end_source(domain:&Arc<DomainRuntime>,plan:&Plan)->Result<(),DomainError>{
    domain.own(scope(domain)?,|owner|owner.validate_selection_binding(plan)).map_err(store_failure)?;
    let alive=!matches!(EndedHost::claim(&plan.source.host_directory),Ok(Retained::Ended(_)));
    if alive || !host_ended(domain,&plan.task)?{
        let (backend,_)=domain.sdk().ok_or_else(||failure("The provider runtime is unavailable"))?;
        domain.stop_sdk_host(&plan.source.agent).map_err(failure)?;
        crate::sdk_owner::end_unowned_host(Arc::clone(domain),backend,plan.task.clone()).map_err(failure)?;
    }
    ensure_fence(domain,plan)?;
    let fences=domain.selection_changes.fences.lock_or_recover("the provider transition fences");
    domain.own(scope(domain)?,|owner|owner.source_fenced(plan,fences.get(&plan.id).unwrap())).map_err(store_failure).map(drop)
}
/// Whether the end of the task's current execution is already committed. A host that resumed the
/// same execution after it may still run: only the host's lock says whether one does.
fn host_ended(domain:&DomainRuntime,task:&TaskId)->Result<bool,DomainError>{
    let read=domain.read();
    let journal=read.state().lifecycle_journal.get(task).ok_or_else(||failure("The task has no lifecycle"))?;
    let admitted=journal.iter().rposition(|record|matches!(record,LifecycleRecord::Admitted{..})).unwrap_or(0);
    Ok(journal.iter().skip(admitted).any(|record|matches!(record,LifecycleRecord::HostEnded{..}|LifecycleRecord::ExecutionReleased{..})))
}
/// Whether the task has something to go on with and nobody else to wait for: the moment its next
/// node starts, as the node it moved from would have resumed then.
fn ready_to_start(domain:&DomainRuntime,task:&TaskId)->Result<bool,DomainError>{
    let read=domain.read();let lifecycle=read.state().lifecycle_of(task)?;
    Ok(!lifecycle.pending_inputs().is_empty() && matches!(lifecycle.live(),Some(alondra_domain::LiveState::Working(_)|alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::Gate))))
}
fn ensure_fence(domain:&DomainRuntime,plan:&Plan)->Result<(),DomainError>{
    if domain.selection_changes.fences.lock_or_recover("the provider transition fences").contains_key(&plan.id){return Ok(())}
    let retained=EndedHost::claim(&plan.source.host_directory).map_err(failure)?;
    let proof=match retained{Retained::Ended(proof)=>proof,Retained::Held=>return Err(failure("The source host still holds its native execution")),Retained::Absent=>return Err(failure("The source host fence is missing; its absence is not continuation proof"))};
    domain.selection_changes.fences.lock_or_recover("the provider transition fences").insert(plan.id.clone(),proof);
    Ok(())
}

pub(crate) async fn advance(domain:&Arc<DomainRuntime>,task:&TaskId)->Result<Phase,DomainError>{
    let _advance=domain.selection_changes.advancing.lock().await;
    loop{
        let change=current(domain,task)?.ok_or_else(||failure("No selection change is pending"))?;
        #[cfg(any(test, feature = "test-hooks"))]
        if *domain.selection_changes.crash_at.lock().unwrap()==Some(change.phase){return Err(failure("the daemon died"))}
        let plan=&change.plan;
        if plan.kind==Kind::Model && plan.target_account!=plan.source.account{return Err(failure("The account and provider of this native session cannot change; create a new node"))}
        if plan.kind==Kind::Model && matches!(change.phase,Phase::SourceFenced|Phase::ContextReady|Phase::TargetAdmitted|Phase::Consumed){return Err(failure("Session transfer is disabled; the node keeps its original account and native history"))}

        if matches!(change.phase,Phase::Requested|Phase::Draining) && matches!(domain.read().state().lifecycle_of(task)?.phase(),alondra_domain::TaskPhase::Finishing|alondra_domain::TaskPhase::Completed|alondra_domain::TaskPhase::Cancelled){
            phase(domain,&change,Phase::Aborted,json!({"task_ended":true}))?;
            return Ok(Phase::Aborted)
        }
        match change.phase{
            Phase::Requested=>match classify(plan){
                SelectionTransition::Reject(reason)=>return Err(failure(reason)),
                SelectionTransition::Keep=>{apply_selection(domain,plan,Phase::Requested,Phase::Done,json!({"unchanged_execution":true}))?;return Ok(Phase::Done)},
                SelectionTransition::ApplyNextTurn=>phase(domain,&change,Phase::Draining,json!({}))?,
                SelectionTransition::CreateWithHandoff if plan.kind==Kind::Move=>phase(domain,&change,Phase::Draining,json!({}))?,
                _=>return Err(failure("This change requires a new native session; create a new node instead")),
            },
            Phase::Draining if plan.kind==Kind::Move=>end_source(domain,plan)?,
            Phase::Draining=>{
                if matches!(classify(plan), SelectionTransition::ApplyNextTurn) && abort_unapplied_model_change(domain, &change)? { return Ok(Phase::Aborted) }
                domain.own(scope(domain)?,|owner|owner.validate_selection_source(plan)).map_err(store_failure)?;
                if !drained(domain,plan)? {
                    if !domain.owns_sdk_host(&plan.source.agent){
                        let (backend,_)=domain.sdk().ok_or_else(||failure("Provider runtime missing"))?;
                        let plugin=domain.home().join(crate::plugins::NODE);
                        match crate::sdk_owner::adopt_for_transition(Arc::clone(domain),backend,task.clone(),Some(&plugin)).map_err(failure)? {
                            Some(driver)=>{domain.add_sdk_owner(crate::sdk_owner::Owner::spawn(driver));return Ok(Phase::Draining)},
                            None=>if !drained(domain,plan)?{return Err(failure("The source ended with unresolved work or delivery; its native input cannot be repeated"))},
                        }
                    }else{return Ok(Phase::Draining)}
                }
                if !domain.owns_sdk_host(&plan.source.agent){
                    let (backend,_)=domain.sdk().ok_or_else(||failure("The native runtime is unavailable"))?;
                    let plugin=crate::plugins::node_in(domain.home()).ok_or_else(||failure("The node plugin is unavailable; run alondra doctor --fix"))?;
                    let driver=crate::sdk_owner::adopt_for_transition(Arc::clone(domain),backend,task.clone(),Some(&plugin)).map_err(failure)?.ok_or_else(||failure("The existing native session cannot be adopted; create a new node instead of transferring it"))?;
                    domain.add_sdk_owner(crate::sdk_owner::Owner::spawn(driver));return Ok(Phase::Draining)
                }
                let available=domain.read().state().lifecycle_of(task)?.resource()==Some(alondra_domain::ResourceEvidence::Available);
                if available && matches!(classify(plan),SelectionTransition::ApplyNextTurn){
                    let handed=match domain.own(scope(domain)?,|owner|owner.configure_transition_model(plan)).map_err(store_failure) {
                        Ok((handed, _)) => handed,
                        Err(error) if error.code == "native_model_control_rejected" => {
                            if abort_unapplied_model_change(domain, &change)? { return Ok(Phase::Aborted) }
                            return Err(error)
                        }
                        Err(error) => return Err(error),
                    };
                    if !handed{return Ok(Phase::Draining)}
                    crate::sdk_owner::retain_selection_options(&plan.source.host_directory,applied(plan)).map_err(failure)?;
                    apply_selection(domain,plan,Phase::Draining,Phase::TargetReady,json!({"same_host":true}))?;
                    let now=current(domain,task)?.unwrap();phase(domain,&now,Phase::Done,json!({"same_host":true}))?;return Ok(Phase::Done)
                }
                if !available || !matches!(classify(plan),SelectionTransition::ApplyNextTurn){return Err(failure("The existing native session cannot apply this model change; create a new node"))}
                let (backend,_)=domain.sdk().ok_or_else(||failure("Provider runtime missing"))?;
                domain.stop_sdk_host(&plan.source.agent).map_err(failure)?;
                crate::sdk_owner::end_unowned_host(Arc::clone(domain),backend,task.clone()).map_err(failure)?;
                ensure_fence(domain,plan)?;
                let fences=domain.selection_changes.fences.lock_or_recover("the provider transition fences");
                domain.own(scope(domain)?,|owner|owner.source_fenced(plan,fences.get(&plan.id).unwrap())).map_err(store_failure)?;
            }
            Phase::SourceFenced=>{ensure_fence(domain,plan)?;prepare_context(domain,plan)?;}
            Phase::ContextReady=>{
                ensure_fence(domain,plan)?;
                if plan.kind==Kind::Move && !ready_to_start(domain,task)?{return Ok(Phase::ContextReady)}
                let prepared:PreparedHandoff=serde_json::from_value(change.facts.clone()).map_err(failure)?;
                let (backend,_)=domain.sdk().ok_or_else(||failure("Provider runtime missing"))?;
                let plugin=crate::plugins::node_in(domain.home()).filter(|plugin|plugin.is_dir()).or_else(||Some(domain.home().join(crate::plugins::NODE)).filter(|plugin|plugin.is_dir())).ok_or_else(||failure("Alondra's task plugin is unavailable; run alondra doctor --fix"))?;
                let launch=crate::sdk_owner::Launch{task:task.clone(),name:domain.read().state().tasks[task].title.clone(),directory:plan.source.directory.clone(),prompt:prepared.new_input,plugin,options:crate::sdk_owner::LaunchOptions{provider:plan.target_account.provider,selection:Some(applied(plan)),model:Some(plan.target_model.model.model.clone()),effort:plan.target_model.reasoning_effort.clone(),disallowed_tools:plan.source.disallowed_tools.clone()},model:None,account:plan.target_account.account.clone()};
                let driver=crate::sdk_owner::continue_session(Arc::clone(domain),backend,launch,crate::sdk_owner::Continuation::Transition{id:plan.id.clone()}).map_err(failure)?;
                domain.add_sdk_owner(crate::sdk_owner::Owner::spawn(driver));return Ok(Phase::TargetAdmitted)
            }
            Phase::TargetReady if change.facts["same_host"].as_bool()==Some(true)=>{phase(domain,&change,Phase::Done,change.facts.clone())?;return Ok(Phase::Done)}
            Phase::TargetAdmitted|Phase::TargetReady=>{
                ensure_fence(domain,plan)?;
                if let Some(agent)=domain.read().state().tasks.get(task).and_then(|task|task.agent_id.clone()) && !domain.owns_sdk_host(&agent){crate::sdk_owner::retake(domain,task.clone());}
                return Ok(change.phase)
            }
            Phase::Consumed=>{phase(domain,&change,Phase::Done,change.facts.clone())?;}
            Phase::Done|Phase::Aborted=>{domain.selection_changes.fences.lock_or_recover("the provider transition fences").remove(&plan.id);return Ok(change.phase)}
            Phase::Uncertain=>return Ok(Phase::Uncertain),
        }
    }
}

pub async fn run(domain:Arc<DomainRuntime>){
    let mut changed=domain.subscribe();
    loop{
        release_finished_fences(&domain);
        let pending=domain.store.lock_or_exit("the domain store").provider_transitions();
        if let Ok(pending)=pending{for change in pending{if let Err(error)=advance(&domain,&change.plan.task).await{log!("selection change of {} needs attention: {}",change.plan.task,error.message);let _=mark_uncertain(&domain,&change.plan.task,&error.message);}}}
        tokio::select!{result=changed.changed()=>if result.is_err(){return},_=tokio::time::sleep(std::time::Duration::from_secs(10))=>{}}
    }
}

pub(crate) fn release_finished_fences(domain:&DomainRuntime){
    let ids:Vec<String>=domain.selection_changes.fences.lock_or_recover("the provider transition fences").keys().cloned().collect();
    for id in ids{
        let ended=domain.store.lock_or_exit("the domain store").provider_transition_finished(&id).unwrap_or(false);
        if ended{domain.selection_changes.fences.lock_or_recover("the provider transition fences").remove(&id);}
    }
}
fn mark_uncertain(domain:&DomainRuntime,task:&TaskId,reason:&str)->Result<(),DomainError>{
    let Some(change)=current(domain,task)? else{return Ok(())};if change.phase==Phase::Uncertain{return Ok(())}
    let reason: String=format!("Selection change needs attention: {reason}").chars().take(1900).collect();
    domain.own(scope(domain)?,|owner|{
        let selection=owner.state().tasks.get(task).and_then(|task|task.selection.clone()).unwrap_or_else(||change.plan.intent.clone());
        owner.stage_command(&Actor::System,&envelope(domain,&format!("{}-uncertain",change.plan.id),Command::RecordTaskSelection{task_id:task.clone(),selection,pending:Some(PendingTaskSelection{account:change.plan.target_account.clone(),model:change.plan.target_model.clone(),reason:reason.clone()})}),Attached::None)?;
        owner.provider_transition_phase(&change.plan.id,change.phase,Phase::Uncertain,json!({"previous_phase":change.phase,"reason":reason,"evidence":change.facts}))
    }).map_err(store_failure).map(drop)
}
