//! Append-only selection changes share the task's owner transaction and native execution fence.
use super::*;
use alondra_core::accounts::AccountKey;
use alondra_core::model_selection::{ModelSelection, TaskSelection};
use alondra_agent_runtime::owned_host::EndedHost;

pub(super) const SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS provider_transition_steps(
 sequence INTEGER PRIMARY KEY, transition_id TEXT NOT NULL, task_id TEXT NOT NULL REFERENCES tasks(id),
 phase TEXT NOT NULL, at INTEGER NOT NULL, facts TEXT NOT NULL CHECK(json_valid(facts)));
 CREATE INDEX IF NOT EXISTS provider_transition_task ON provider_transition_steps(task_id,sequence);
 CREATE TRIGGER IF NOT EXISTS provider_transition_immutable BEFORE UPDATE ON provider_transition_steps BEGIN SELECT RAISE(ABORT,'provider transition evidence is immutable'); END;
 CREATE TRIGGER IF NOT EXISTS provider_transition_retained BEFORE DELETE ON provider_transition_steps BEGIN SELECT RAISE(ABORT,'provider transition evidence is retained'); END;
 CREATE TRIGGER IF NOT EXISTS provider_transition_one_open BEFORE INSERT ON provider_transition_steps
 WHEN NEW.phase='requested' AND EXISTS(SELECT 1 FROM provider_transition_steps p WHERE p.task_id=NEW.task_id AND p.phase='requested'
 AND NOT EXISTS(SELECT 1 FROM provider_transition_steps e WHERE e.transition_id=p.transition_id AND e.phase IN('done','aborted')))
 BEGIN SELECT RAISE(ABORT,'a task has one open provider transition'); END;
 CREATE TRIGGER IF NOT EXISTS account_move_no_provider_transition BEFORE INSERT ON account_move_steps
 WHEN NEW.step='planned' AND EXISTS(SELECT 1 FROM provider_transition_steps p WHERE p.task_id=NEW.task_id AND p.phase='requested'
 AND NOT EXISTS(SELECT 1 FROM provider_transition_steps e WHERE e.transition_id=p.transition_id AND e.phase IN('done','aborted')))
 BEGIN SELECT RAISE(ABORT,'a provider transition already holds the task'); END;"#;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Source {
    pub agent: AgentId,
    pub instance: String,
    pub conversation: String,
    pub generation: u64,
    pub account: AccountKey,
    pub configuration: String,
    pub binding: String,
    pub driver: String,
    pub turn_model: bool,
    pub directory: String,
    pub host_directory: std::path::PathBuf,
    pub selection: Option<TaskSelection>,
    pub disallowed_tools: Vec<String>,
}
/// What a selection change does: change the model of the running session, or move the task to
/// another account, where a new node takes it over from its record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub(crate) enum Kind { #[default] Model, Move }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub id: String,
    #[serde(default)]
    pub kind: Kind,
    pub task: TaskId,
    pub expected_selection: String,
    pub source: Source,
    pub target_account: AccountKey,
    pub target_model: ModelSelection,
    pub intent: TaskSelection,
    pub reason: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub(crate) enum Phase { Requested, Draining, SourceFenced, ContextReady, TargetAdmitted, TargetReady, Consumed, Done, Aborted, Uncertain }
impl Phase {
    pub fn key(self) -> &'static str { match self {
        Self::Requested=>"requested",Self::Draining=>"draining",Self::SourceFenced=>"source_fenced",Self::ContextReady=>"context_ready",Self::TargetAdmitted=>"target_admitted",Self::TargetReady=>"target_ready",Self::Consumed=>"consumed",Self::Done=>"done",Self::Aborted=>"aborted",Self::Uncertain=>"uncertain",
    }}
    fn parse(value:&str)->Result<Self>{Ok(match value {
        "requested"=>Self::Requested,"draining"=>Self::Draining,"source_fenced"=>Self::SourceFenced,"context_ready"=>Self::ContextReady,"target_admitted"=>Self::TargetAdmitted,"target_ready"=>Self::TargetReady,"consumed"=>Self::Consumed,"done"=>Self::Done,"aborted"=>Self::Aborted,"uncertain"=>Self::Uncertain,_=>anyhow::bail!("unknown provider transition phase"),
    })}
}
#[derive(Clone, Debug)]
pub(crate) struct Transition { pub plan:Plan,pub phase:Phase,pub facts:serde_json::Value }

fn load(c:&Connection,id:&str)->Result<Option<Transition>> {
    let plan:Option<String>=c.query_row("SELECT facts FROM provider_transition_steps WHERE transition_id=?1 AND phase='requested'",[id],|r|r.get(0)).optional()?;
    let Some(plan)=plan else{return Ok(None)};
    let (phase,facts):(String,String)=c.query_row("SELECT phase,facts FROM provider_transition_steps WHERE transition_id=?1 ORDER BY sequence DESC LIMIT 1",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    Ok(Some(Transition{plan:serde_json::from_str(&plan)?,phase:Phase::parse(&phase)?,facts:serde_json::from_str(&facts)?}))
}
fn open(c:&Connection,task:&TaskId)->Result<Option<Transition>> {
    let id:Option<String>=c.query_row("SELECT p.transition_id FROM provider_transition_steps p WHERE p.task_id=?1 AND p.phase='requested' AND NOT EXISTS(SELECT 1 FROM provider_transition_steps e WHERE e.transition_id=p.transition_id AND e.phase IN('done','aborted')) ORDER BY p.sequence DESC LIMIT 1",[&task.0],|r|r.get(0)).optional()?;
    id.map(|id|load(c,&id)).transpose().map(Option::flatten)
}
fn insert(c:&Connection,plan:&Plan)->Result<()> {
    anyhow::ensure!(!plan.id.is_empty() && plan.reason.len()<=2000,"invalid provider transition");
    let legacy:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM account_move_steps p WHERE p.task_id=?1 AND p.step='planned' AND NOT EXISTS(SELECT 1 FROM account_move_steps e WHERE e.task_id=p.task_id AND e.number=p.number AND e.step IN('done','undone')))",[&plan.task.0],|r|r.get(0))?;
    anyhow::ensure!(!legacy,"a legacy account move already holds the task");
    c.execute("INSERT INTO provider_transition_steps(transition_id,task_id,phase,at,facts) VALUES(?1,?2,'requested',?3,?4)",params![plan.id,plan.task.0,alondra_core::now_millis(),serde_json::to_string(plan)?])?;
    Ok(())
}
fn append(c:&Connection,id:&str,expected:Phase,next:Phase,facts:&serde_json::Value)->Result<()> {
    let current=load(c,id)?.context("provider transition is missing")?;
    anyhow::ensure!(current.phase==expected,"provider transition changed before its effect was recorded");
    let allowed=match (expected,next) {
        (Phase::Requested,Phase::Draining|Phase::Done|Phase::Aborted)=>true,
        (Phase::Draining,Phase::Draining|Phase::SourceFenced|Phase::TargetReady|Phase::Aborted)=>true,
        (Phase::SourceFenced,Phase::ContextReady)=>true,
        (Phase::ContextReady,Phase::TargetAdmitted)=>true,
        (Phase::TargetAdmitted,Phase::TargetReady)=>true,
        (Phase::TargetReady,Phase::Consumed|Phase::Done)=>true,
        (Phase::Consumed,Phase::Done)=>true,
        (_,Phase::Uncertain) if !matches!(expected,Phase::Done|Phase::Aborted)=>true,
        _=>false,
    };
    anyhow::ensure!(allowed,"provider transition step is out of order");
    let encoded=serde_json::to_string(facts)?;
    anyhow::ensure!(encoded.len()<=2*1024*1024,"provider transition context exceeds its evidence limit");
    c.execute("INSERT INTO provider_transition_steps(transition_id,task_id,phase,at,facts) VALUES(?1,?2,?3,?4,?5)",params![id,current.plan.task.0,next.key(),alondra_core::now_millis(),encoded])?;
    Ok(())
}

impl DomainStore {
    pub(crate) fn provider_transition_finished(&self,id:&str)->Result<bool>{Ok(self.conn.query_row("SELECT EXISTS(SELECT 1 FROM provider_transition_steps WHERE transition_id=?1 AND phase IN('done','aborted'))",[id],|row|row.get(0))?)}
    pub(crate) fn provider_transition(&self,task:&TaskId)->Result<Option<Transition>> {open(&self.conn,task)}
    pub(crate) fn provider_transitions(&self)->Result<Vec<Transition>> {
        let mut statement=self.conn.prepare("SELECT DISTINCT task_id FROM provider_transition_steps WHERE phase='requested' ORDER BY task_id")?;
        let ids=statement.query_map([],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().map(|id|open(&self.conn,&TaskId(id))).collect::<Result<Vec<_>>>().map(|rows|rows.into_iter().flatten().collect())
    }
}
impl OwnerTransaction<'_> {
    pub(crate) fn validate_selection_binding(&mut self,plan:&Plan)->std::result::Result<(),StoreError>{
        let lifecycle=self.lifecycle(&plan.task)?;
        let stamp=lifecycle.execution().ok_or_else(||DomainError::new("native_session_required","The source native session is unavailable"))?;
        let same_account=plan.kind==Kind::Move || plan.target_account==plan.source.account;
        if stamp.instance().as_str()!=plan.source.instance || stamp.conversation().as_str()!=plan.source.conversation || stamp.generation()!=plan.source.generation || self.state().tasks[&plan.task].account_key()!=plan.source.account || !same_account{return Err(DomainError::new("immutable_session","The active native session changed; refresh instead of transferring it").into())}
        if plan.kind==Kind::Move{return Ok(())}
        let binding=self.execution_binding(stamp)?;
        if binding.id()!=plan.source.binding || binding.driver()!=plan.source.driver || binding.configuration()!=plan.source.configuration{return Err(DomainError::new("stale_native_binding","The native credential or runtime binding changed; this session cannot be transferred").into())}
        let identity=crate::sdk_owner::NativeAccountIdentity::load(&plan.source.host_directory).map_err(|error|StoreError::Storage(anyhow::anyhow!("{error}")))?;
        self.require_native_identity(&binding,identity.as_ref())?;
        Ok(())
    }
    pub(crate) fn validate_selection_source(&mut self,plan:&Plan)->std::result::Result<(),StoreError>{
        self.validate_selection_binding(plan)?;
        let catalog=self.state().model_catalogs.iter().find(|catalog|catalog.account==plan.target_account).ok_or_else(||DomainError::new("model_catalog_unavailable","Refresh this account's model catalog"))?;
        let candidate=alondra_core::model_selection::ModelCandidate{model:plan.target_model.model.model.clone(),reasoning_effort:plan.target_model.reasoning_effort.clone()};
        if catalog.resolve(&candidate).as_ref()!=Ok(&plan.target_model){return Err(DomainError::new("stale_model_catalog","The native model catalog changed; refresh the requested model").into())}
        if plan.kind==Kind::Move{return Ok(())}
        let effective=plan.source.selection.as_ref().and_then(|selection|selection.effective.as_ref()).ok_or_else(||DomainError::new("native_session_required","The source model identity is unavailable"))?;
        if effective.reasoning_effort!=plan.target_model.reasoning_effort{return Err(DomainError::new("unsupported_live_effort","The verified model control cannot change reasoning effort").into())}
        Ok(())
    }
    pub(crate) fn transition_launch_text(&mut self,task:&TaskId,fallback:&str)->std::result::Result<String,StoreError>{
        let first=self.lifecycle(task)?.pending_inputs().first().map(|input|input.as_str().to_owned()).context("transition input missing")?;
        let text=self.sql(|c|c.query_row("SELECT body FROM lifecycle_inputs WHERE task_id=?1 AND input_id=?2",params![task.0,first],|r|r.get::<_,Option<String>>(0)).map_err(Into::into))?.unwrap_or_else(||fallback.to_owned());
        let change=self.sql(|c|open(c,task))?.context("transfer context missing")?;
        let prepared:alondra_store::PreparedHandoff=serde_json::from_value(change.facts).map_err(|error|StoreError::Storage(error.into()))?;
        let window=self.state().model_catalogs.iter().find(|catalog|catalog.account==change.plan.target_account).and_then(|catalog|catalog.models.iter().find(|model|model.model==change.plan.target_model.model.model)).and_then(|model|model.context_window);
        let available=window.and_then(|window|window.checked_sub(4096)).unwrap_or(alondra_store::HANDOFF_TOKEN_CAP);
        if prepared.estimated_context_tokens.saturating_add(alondra_store::estimated_tokens(&text))>available{return Err(DomainError::new("handoff_budget","The exact queued user input and retained context exceed the target's transfer budget").into())}
        Ok(text)
    }
    pub(crate) fn source_inputs_unoffered(&mut self,task:&TaskId)->std::result::Result<bool,StoreError>{
        let lifecycle=self.lifecycle(task)?;
        let generation=lifecycle.execution().context("source execution missing")?.generation();
        let inputs:Vec<_>=lifecycle.pending_inputs().iter().map(|input|input.as_str().to_owned()).collect();
        self.sql(|c|{
            for input in inputs {
                let sent:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM lifecycle_dispatches WHERE task_id=?1 AND generation=?2 AND input_id=?3 AND status IN('claimed','handed','uncertain'))",params![task.0,i64::try_from(generation)?,input],|r|r.get(0))?;
                if sent{return Ok(false)}
            }
            Ok(true)
        })
    }
    pub(crate) fn request_provider_transition(&mut self,plan:&Plan)->std::result::Result<(),StoreError>{ self.sql(|c|insert(c,plan)) }
    pub(crate) fn provider_transition_phase(&mut self,id:&str,expected:Phase,next:Phase,facts:serde_json::Value)->std::result::Result<(),StoreError>{self.sql(|c|append(c,id,expected,next,&facts))}
    pub(crate) fn source_fenced(&mut self,plan:&Plan,proof:&EndedHost)->std::result::Result<(),StoreError>{
        self.sql(|c|{
            anyhow::ensure!(proof.state_dir()==plan.source.host_directory,"source fence belongs to another host");
            let bootstrap:serde_json::Value=serde_json::from_slice(proof.bootstrap())?;
            anyhow::ensure!(bootstrap["execution"]["task_id"].as_str()==Some(&plan.task.0) && bootstrap["execution"]["process_instance_id"].as_str()==Some(plan.source.instance.as_str()) && bootstrap["execution"]["generation"].as_u64()==Some(plan.source.generation),"source fence belongs to another execution");
            anyhow::ensure!(bootstrap["execution"]["agent_id"].as_str()==Some(plan.source.agent.0.as_str()) && bootstrap["execution"]["conversation_id"].as_str()==Some(plan.source.conversation.as_str()) && bootstrap["fence"]["profile_id"].as_str()==Some("alondra") && bootstrap["fence"]["account_id"].as_str()==Some(plan.source.account.account.as_str()),"source bootstrap does not match its recorded binding");
            use sha2::Digest;
            append(c,&plan.id,Phase::Draining,Phase::SourceFenced,&serde_json::json!({"bootstrap_digest":sha2::Sha256::digest(proof.bootstrap()).iter().map(|byte|format!("{byte:02x}")).collect::<String>(),"generation":plan.source.generation}))
        })
    }
    pub(crate) fn selection_source(&mut self,task:&TaskId,host_directory:std::path::PathBuf)->std::result::Result<Source,StoreError>{
        let current=self.state().tasks.get(task).ok_or_else(||DomainError::new("not_found","Task missing"))?;
        let selection=current.selection.clone();let account=current.account_key();
        let stamp=self.lifecycle(task)?.execution().cloned().ok_or_else(||DomainError::new("conflict","Task has no managed execution"))?;
        let disallowed_tools=crate::sdk_owner::LaunchOptions::load(&host_directory).map_err(|error|DomainError::new("selection_transition",error.to_string()))?.disallowed_tools;
        let turn_model=self.execution_binding(&stamp)?.permits(alondra_provider_contract::Capability::SetModel);
        self.sql(|c|{
            let (binding,driver,configuration):(String,String,String)=c.query_row("SELECT b.id,b.driver,b.configuration FROM lifecycle_execution_bindings e JOIN provider_bindings b ON b.id=e.binding_id WHERE e.task_id=?1 AND e.generation=?2",params![task.0,i64::try_from(stamp.generation())?],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            let directory:String=c.query_row("SELECT dir FROM task_worktrees WHERE task_id=?1",[&task.0],|r|r.get(0))?;
            Ok(Source{agent:stamp.node().clone(),instance:stamp.instance().as_str().into(),conversation:stamp.conversation().as_str().into(),generation:stamp.generation(),account,configuration,binding,driver,turn_model,directory,host_directory,selection,disallowed_tools})
        })
    }
}

impl OwnerTransaction<'_> {
    pub(crate) fn transition_model_control_status(&mut self, plan: &Plan) -> std::result::Result<Option<String>, StoreError> {
        let change = self.sql(|connection| load(connection, &plan.id))?.context("provider transition missing")?;
        let Some(control) = change.facts["control_id"].as_i64() else { return Ok(None) };
        self.sql(|connection| {
            let row: Option<(String, String)> = connection.query_row(
                "SELECT status,argument FROM lifecycle_controls WHERE id=?1 AND task_id=?2 AND generation=?3 AND kind='set_model'",
                params![control, plan.task.0, i64::try_from(plan.source.generation)?], |row| Ok((row.get(0)?, row.get(1)?))
            ).optional()?;
            match row {
                Some((status, model)) if model == plan.target_model.model.model => Ok(Some(status)),
                _ => anyhow::bail!("the native model control identity changed"),
            }
        })
    }
    pub(crate) fn configure_transition_model(&mut self,plan:&Plan)->std::result::Result<bool,StoreError>{
        self.validate_selection_source(plan)?;
        if let Some(status) = self.transition_model_control_status(plan)? {
            if status == "rejected" { return Err(DomainError::new("native_model_control_rejected", "The native runtime refused the model change before applying it").into()) }
            if status == "uncertain" { return Err(StoreError::Storage(anyhow::anyhow!("the native model configuration is uncertain; do not repeat it"))) }
            return Ok(status == "handed");
        }
        let command=CommandEnvelope{protocol_version:PROTOCOL_VERSION,data_epoch:self.state().data_epoch.clone(),command_id:format!("{}-set-model",plan.id).as_str().into(),expected_revision:0,command:Command::SetModel{task_id:plan.task.clone(),model:plan.target_model.model.model.clone()}};
        self.stage_command(&Actor::System,&command,Attached::None)?;
        self.sql(|c|{
            let control:i64=c.query_row("SELECT id FROM lifecycle_controls WHERE task_id=?1 AND generation=?2 AND kind='set_model' AND argument=?3 ORDER BY id DESC LIMIT 1",params![plan.task.0,i64::try_from(plan.source.generation)?,plan.target_model.model.model],|r|r.get(0))?;
            append(c,&plan.id,Phase::Draining,Phase::Draining,&serde_json::json!({"control_id":control}))?;Ok(false)
        })
    }
    pub(crate) fn pending_transition_selection(&mut self,id:&str)->std::result::Result<alondra_core::model_selection::PendingTaskSelection,StoreError>{
        let change=self.sql(|c|load(c,id))?.context("provider transition missing")?;
        Ok(alondra_core::model_selection::PendingTaskSelection{account:change.plan.target_account,model:change.plan.target_model,reason:change.plan.reason})
    }
    pub(crate) fn admit_provider_transition(&mut self,id:&str,stamp:&alondra_foundation::ExecutionStamp)->std::result::Result<(),StoreError>{
        let input=self.lifecycle(stamp.task())?.pending_inputs().first().map(|input|input.as_str().to_owned()).context("target admission has no first input")?;
        self.sql(|c|{
            let change=load(c,id)?.context("provider transition missing")?;
            anyhow::ensure!(change.plan.task==*stamp.task() && change.plan.source.instance!=stamp.instance().as_str(),"target admission does not belong to the selection change");
            append(c,id,Phase::ContextReady,Phase::TargetAdmitted,&serde_json::json!({"agent":stamp.node(),"instance":stamp.instance().as_str(),"generation":stamp.generation(),"input":input,"context":change.facts}))
        })
    }
    pub(crate) fn observe_provider_transition(&mut self,stamp:&alondra_foundation::ExecutionStamp,cursor:u64)->std::result::Result<(),StoreError>{
        if self.state().tasks.get(stamp.task()).is_none_or(|task|task.pending_selection.is_none()){return Ok(())}
        let Some(mut change)=self.sql(|c|open(c,stamp.task()))? else{return Ok(())};
        if !matches!(change.phase,Phase::TargetAdmitted|Phase::TargetReady) || change.facts["instance"].as_str()!=Some(stamp.instance().as_str()) || change.facts["generation"].as_u64()!=Some(stamp.generation()){return Ok(())}
        if change.phase==Phase::TargetAdmitted && self.lifecycle(stamp.task())?.resource()==Some(alondra_domain::ResourceEvidence::Available){
            self.provider_transition_phase(&change.plan.id,Phase::TargetAdmitted,Phase::TargetReady,change.facts.clone())?;change.phase=Phase::TargetReady;
        }
        if change.phase!=Phase::TargetReady{return Ok(())}
        let input=change.facts["input"].as_str().context("target input identity missing")?.to_owned();
        let consumed=self.state().lifecycle_journal.get(stamp.task()).is_some_and(|journal|journal.iter().any(|record|matches!(record,LifecycleRecord::TurnStarted{input:seen,..}|LifecycleRecord::InputConsumed{input:seen} if seen==&input)));
        if !consumed{return Ok(())}
        let mut selection=change.plan.intent.clone();selection.effective=Some(change.plan.target_model.clone());
        let envelope=CommandEnvelope{protocol_version:PROTOCOL_VERSION,data_epoch:self.state().data_epoch.clone(),command_id:format!("{}-consumed",change.plan.id).as_str().into(),expected_revision:0,command:Command::RecordTaskSelection{task_id:stamp.task().clone(),selection,pending:None}};
        self.stage_command(&Actor::System,&envelope,Attached::None)?;
        let proof=serde_json::json!({"target":change.facts,"consumed_input":input,"cursor":cursor,"generation":stamp.generation()});
        self.provider_transition_phase(&change.plan.id,Phase::TargetReady,Phase::Consumed,proof.clone())?;
        self.release_context(&change.plan.id,stamp.task())?;
        self.provider_transition_phase(&change.plan.id,Phase::Consumed,Phase::Done,proof)
    }
    pub(crate) fn provider_transition_blocks(&mut self,stamp:&alondra_foundation::ExecutionStamp)->std::result::Result<bool,StoreError>{
        if self.state().tasks.get(stamp.task()).is_none_or(|task|task.pending_selection.is_none()){return Ok(false)}
        Ok(self.sql(|c|open(c,stamp.task()))?.is_some_and(|change|change.plan.source.instance==stamp.instance().as_str()))
    }
}
