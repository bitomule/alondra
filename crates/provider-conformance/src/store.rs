//! A real application store, in memory, holding one task whose execution an adapter's normalizer
//! folds observations for, through the same staging the daemon uses.
use alondra_agent_application::{ObservationNormalizer, ObservedFact, Skipping, stage_observation};
use alondra_domain::Event;
use alondra_foundation::{ConversationId, ExecutionStamp, HostId, InputId, InstanceId, TaskId};
use alondra_provider_contract::{Capability, OrderedObservation, Placement, ProviderBinding};
use alondra_store::{AdapterInputMetadata, AuthorityScope, InputPayload, LifecycleTransaction, OwnerGrant};
use rusqlite::{Connection, TransactionBehavior};
use std::collections::BTreeSet;

const WORKSPACE: &str = "/Users/ana/work";

fn failed(step: &str) -> impl Fn(alondra_store::Error) -> String + '_ {
    move |error| format!("{step}: {error:?}")
}
fn sql(step: &str) -> impl Fn(rusqlite::Error) -> String + '_ {
    move |error| format!("{step}: {error}")
}

/// What the store took of one observation: the facts the domain is to record, or the reason the
/// staging refused it, after which the observation is discarded the way the daemon discards one.
pub enum Observed {
    Facts(Vec<ObservedFact>),
    Refused(String),
}

pub struct Store {
    connection: Connection,
    scope: AuthorityScope,
    binding: ProviderBinding,
    owner: OwnerGrant,
    task: TaskId,
}

impl Store {
    /// A task admitted with `initial` and launched on a binding with `capabilities`, its owner
    /// granted: what a fresh host of the execution starts from.
    pub fn open(capabilities: BTreeSet<Capability>, initial: &InputId, native: Vec<u8>) -> Result<Self, String> {
        let mut connection = Connection::open_in_memory().map_err(sql("open"))?;
        connection.execute_batch("PRAGMA foreign_keys=ON").map_err(sql("foreign keys"))?;
        connection
            .execute_batch(include_str!("../../domain-store-schema/schema.sql"))
            .map_err(sql("domain schema"))?;
        connection
            .execute_batch(
                "CREATE TABLE task_worktrees(task_id TEXT PRIMARY KEY,dir TEXT NOT NULL,at INTEGER NOT NULL);
                 INSERT INTO domain_meta VALUES(1,'epoch',0,0);
                 INSERT INTO requests(id,body,closed) VALUES('request','Work',0);
                 INSERT INTO agents(id,name) VALUES('node','Worker');
                 INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('task','request','Task','not_started',0,0,0,0)",
            )
            .map_err(sql("task rows"))?;
        connection.execute("INSERT INTO task_worktrees VALUES('task',?1,0)", [WORKSPACE]).map_err(sql("worktree"))?;
        let mut install = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql("install"))?;
        LifecycleTransaction::install(&mut install).map_err(failed("install"))?;
        install.commit().map_err(sql("install"))?;
        let scope = AuthorityScope::new("epoch".into()).map_err(failed("scope"))?;
        let binding = ProviderBinding::new("binding".into(), "conformance".into(), "instance".into(), "config".into(), 1, 1, capabilities, "credential".into(), 1)
            .map_err(|_| "binding".to_owned())?;
        let task = TaskId::from("task");
        let raw = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql("launch"))?;
        let mut transaction = LifecycleTransaction::attach(raw, scope.clone()).map_err(failed("attach"))?;
        transaction.create(&task, InputPayload::new(initial.clone(), "First input").map_err(failed("initial input"))?).map_err(failed("create"))?;
        transaction
            .apply(&task, 0, "admit", Event::Admit {
                node: "node".into(),
                conversation: ConversationId::new("conversation").map_err(|error| error.to_string())?,
                instance: InstanceId::new("instance").map_err(|error| error.to_string())?,
                host: HostId::new("host").map_err(|error| error.to_string())?,
            })
            .map_err(failed("admit"))?;
        transaction.bind_provider(&task, &binding).map_err(failed("bind"))?;
        let stamp = transaction.load(&task).map_err(failed("load"))?.execution().cloned().ok_or("no execution after admission")?;
        transaction
            .put_adapter_input(&stamp, initial, AdapterInputMetadata::new(native).map_err(failed("input record"))?)
            .map_err(failed("initial record"))?;
        transaction.stage_launch(&task, &Placement::LocalDirectory(WORKSPACE.into()), &binding).map_err(failed("stage launch"))?;
        transaction.reserve_owner(&stamp, 0).map_err(failed("owner"))?;
        let owner = transaction.commit().map_err(failed("launch"))?.owners.pop().ok_or("no owner granted")?;
        Ok(Self { connection, scope, binding, owner, task })
    }

    pub fn owner(&self) -> &OwnerGrant {
        &self.owner
    }

    fn execution(&self) -> &ExecutionStamp {
        self.owner.execution()
    }

    /// Alondra hands the provider another input, whose native identity is `native`.
    pub fn hand(&mut self, input: &InputId, native: Vec<u8>) -> Result<(), String> {
        let raw = self.connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql("hand"))?;
        let mut transaction = LifecycleTransaction::attach(raw, self.scope.clone()).map_err(failed("attach"))?;
        transaction.put_input(&self.task, InputPayload::new(input.clone(), "Another input").map_err(failed("input"))?).map_err(failed("put input"))?;
        let stamp = self.owner.execution().clone();
        transaction
            .put_adapter_input(&stamp, input, AdapterInputMetadata::new(native).map_err(failed("input record"))?)
            .map_err(failed("input record"))?;
        transaction.commit().map_err(failed("hand"))?;
        Ok(())
    }

    pub fn batch(&self, cursor: u64, evidence: Vec<u8>) -> OrderedObservation {
        OrderedObservation { owner_generation: self.owner.generation(), execution: self.execution().clone(), cursor, evidence }
    }

    /// Stages `batch` as the daemon does and settles it without the domain recording anything:
    /// what is compared is what the adapter said, not what the domain made of it.
    pub fn observe<N: ObservationNormalizer>(&mut self, normalizer: &N, batch: &OrderedObservation) -> Result<Observed, String> {
        let generation = self.owner.generation();
        let raw = self.connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql("observe"))?;
        let mut transaction = LifecycleTransaction::attach(raw, self.scope.clone()).map_err(failed("attach"))?;
        let refusal = match stage_observation(&mut transaction, &self.binding, generation, batch, normalizer) {
            Ok(pending) => {
                let facts = pending.facts().to_vec();
                pending.settle_recorded(&mut transaction, 0).map_err(|error| format!("settle: {error:?}"))?;
                transaction.commit().map_err(failed("commit"))?;
                return Ok(Observed::Facts(facts));
            }
            Err(error) => format!("{error:?}"),
        };
        drop(transaction);
        let raw = self.connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql("discard"))?;
        let mut transaction = LifecycleTransaction::attach(raw, self.scope.clone()).map_err(failed("attach"))?;
        let pending = stage_observation(&mut transaction, &self.binding, generation, batch, &Skipping(normalizer))
            .map_err(|error| format!("discarding a refused observation: {error:?}"))?;
        pending.settle_recorded(&mut transaction, 0).map_err(|error| format!("settle: {error:?}"))?;
        transaction.commit().map_err(failed("discard"))?;
        Ok(Observed::Refused(refusal))
    }
}
