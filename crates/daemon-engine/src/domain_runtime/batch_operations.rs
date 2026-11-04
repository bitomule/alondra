use super::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

type Answer = std::result::Result<Value,DomainError>;

pub(crate) enum Admission {
    Started(Operation),
    Existing { run: String, call: String },
}

pub(crate) struct Operation {
    domain: Arc<DomainRuntime>,
    run: String,
    call: String,
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.domain.operations_live.lock_or_recover("the operation registry").remove(&(self.run.clone(),self.call.clone()));
        self.domain.operations_changed.send_modify(|version| *version = version.wrapping_add(1));
    }
}
impl Operation {
    fn finish(&self, answer: &Answer) -> std::result::Result<(),StoreError> {
        self.domain.store.lock_or_exit("the domain store").complete_tool_operation(&self.run,&self.call,answer)?;
        self.domain.orchestrator_progress();
        Ok(())
    }
}

impl DomainRuntime {
    fn begin_tool_operation(self: &Arc<Self>, bearer: &str, operation: Option<&str>, kind: &str, request: &Value) -> std::result::Result<Admission,StoreError> {
        let refused = || DomainError::new("forbidden","The current orchestrator execution is required");
        let mut authority = self.authority.lock_or_recover("the domain authority");
        let mut store = self.store.lock_or_exit("the domain store");
        let state = store.load()?;
        let principal = authority.resolve(&state.data_epoch.0,bearer).map_err(|_| refused())?;
        let Principal::Orchestrator { generation, execution_id } = &principal else { return Err(refused().into()) };
        if *generation != state.orchestrator_generation { return Err(refused().into()); }
        let identity = match operation {
            Some(identity) if !identity.is_empty() && identity.len() <= 1024 && identity.bytes().all(|byte| byte.is_ascii_hexdigit()) => identity.to_owned(),
            Some(_) => return Err(DomainError::new("invalid_input","Invalid native MCP operation identity").into()),
            None => format!("legacy-{}",crate::auth::random_hex(16)),
        };
        let call: String = Sha256::digest(identity.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
        if store.begin_tool_operation(execution_id,&call,kind,request)?.is_some() {
            return Ok(Admission::Existing { run: execution_id.clone(),call });
        }
        authority.record_change(&principal,kind);
        self.orchestrator_progress();
        self.operations_live.lock_or_recover("the operation registry").insert((execution_id.clone(),call.clone()));
        self.operations_changed.send_modify(|version| *version = version.wrapping_add(1));
        Ok(Admission::Started(Operation { domain: Arc::clone(self),run: execution_id.clone(),call }))
    }

    async fn operation_answer(&self, run: &str, call: &str) -> Answer {
        let mut changed = self.operations_changed.subscribe();
        loop {
            let row = self.store.lock_or_exit("the domain store").tool_operations(run)
                .map_err(|_| DomainError::new("storage_unavailable","The operation receipt could not be read"))?
                .into_iter().find(|row| row.0 == call).ok_or_else(|| DomainError::new("operation_uncertain","The operation receipt is missing"))?;
            match row.2.as_str() {
                "succeeded" => return row.3.as_deref().ok_or_else(|| DomainError::new("operation_uncertain","The completed operation has no result"))
                    .and_then(|result| serde_json::from_str(result).map_err(|_| DomainError::new("operation_uncertain","The operation result is unreadable"))),
                "uncertain" => return Err(row.4.as_deref().and_then(|error| serde_json::from_str(error).ok()).unwrap_or_else(|| DomainError::new("operation_uncertain","Reconcile the earlier operation before trying it again"))),
                _ => {}
            }
            if !self.operations_live.lock_or_recover("the operation registry").contains(&(run.into(),call.into())) {
                return Err(DomainError::new("operation_uncertain","The earlier operation lost its executor; inspect its committed effects before continuing"));
            }
            if changed.changed().await.is_err() { return Err(DomainError::new("operation_uncertain","The operation stopped before its result was recorded")); }
        }
    }

    pub(crate) async fn drain_batch_operations(&self, run: &str) -> std::result::Result<(),StoreError> {
        let mut changed = self.operations_changed.subscribe();
        loop {
            let rows = self.store.lock_or_exit("the domain store").tool_operations(run)?;
            let live = self.operations_live.lock_or_recover("the operation registry").iter().any(|(execution,_)| execution == run);
            if !live { return Ok(()); }
            drop(rows);
            if changed.changed().await.is_err() { return Err(StoreError::Storage(anyhow::anyhow!("The operation drain stopped before its writers finished"))); }
        }
    }

    /// The operations of a run whose result nobody knows, as `kind:state(code)`, for the log.
    pub(crate) fn unconfirmed_operations(&self, run: &str) -> Vec<String> {
        let operations = self.store.lock_or_exit("the domain store").tool_operations(run).unwrap_or_default();
        operations.iter().filter(|row| crate::domain_store::batch_effects::is_unconfirmed(row)).map(|row| {
            let code = row.4.as_deref().and_then(|error| serde_json::from_str::<DomainError>(error).ok()).map_or(String::new(), |error| format!("({})", error.code));
            format!("{}:{}{code}",row.1,row.2)
        }).collect()
    }

    pub(crate) fn confirmed_batch_effects(&self, run: &str, admitted: &[String]) -> std::result::Result<bool,StoreError> {
        let store = self.store.lock_or_exit("the domain store");
        let operations = store.tool_operations(run)?;
        if operations.iter().any(crate::domain_store::batch_effects::is_unconfirmed) { return Ok(false); }
        let mut confirmed = std::collections::BTreeMap::<String,usize>::new();
        for effect in store.batch_effects(run)? { if effect.changed { *confirmed.entry(effect.kind).or_default() += 1; } }
        for (_,kind,_,_,_) in operations { *confirmed.entry(kind).or_default() += 1; }
        let mut required = std::collections::BTreeMap::<&str,usize>::new();
        for kind in admitted { *required.entry(kind.as_str()).or_default() += 1; }
        Ok(required.into_iter().all(|(kind,count)| confirmed.get(kind).copied().unwrap_or(0) >= count))
    }
}

/// Dropping the HTTP consumer does not cancel an admitted physical operation. Its executor
/// records the actual outcome, and the batch waits for that executor before changing authority.
pub async fn execute(domain: Arc<DomainRuntime>, bearer: &str, identity: Option<&str>, kind: &'static str, request: Value, work: impl std::future::Future<Output=Answer> + Send + 'static) -> Answer {
    let admission = domain.begin_tool_operation(bearer,identity,kind,&request).map_err(|error| match error {
        StoreError::Domain(error) => error,
        _ => DomainError::new("storage_unavailable","The operation intent could not be recorded"),
    })?;
    match admission {
        Admission::Existing { run,call } => domain.operation_answer(&run,&call).await,
        Admission::Started(operation) => {
            let running = tokio::spawn(async move {
                let answer = work.await;
                if let Err(error) = operation.finish(&answer) {
                    log!("An orchestrator operation has an uncertain result: {error}");
                    return Err(DomainError::new("operation_uncertain","The operation may have completed, but its result was not committed; reconcile before continuing"));
                }
                answer
            });
            running.await.unwrap_or_else(|_| Err(DomainError::new("operation_uncertain","The operation executor stopped; reconcile its effects before continuing")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Directory;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize,Ordering};

    #[test]
    fn an_operation_that_ends_after_a_panic_holding_the_registry_leaves_it() {
        let home = Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let domain = Arc::new(DomainRuntime::open_at(&home,&home.join("domain.db")).unwrap());
        domain.operations_live.lock().unwrap().insert(("run".into(),"call".into()));
        crate::locks::poison(&domain.operations_live);
        drop(Operation { domain: Arc::clone(&domain),run: "run".into(),call: "call".into() });
        assert!(domain.operations_live.lock().unwrap().is_empty(), "a stale entry would make its callers wait for it forever");
    }

    #[tokio::test]
    async fn losing_the_http_consumer_does_not_cancel_or_repeat_a_durable_mutation() {
        let home = Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let domain = Arc::new(DomainRuntime::open_at(&home,&home.join("domain.db")).unwrap());
        let token = domain.bind_orchestrator("run").unwrap().expose_for_child_environment().to_owned();
        let effects = Arc::new(AtomicUsize::new(0));
        let (entered,started) = tokio::sync::oneshot::channel();
        let (release,barrier) = tokio::sync::oneshot::channel();
        let job_domain = Arc::clone(&domain); let job_token = token.clone(); let count = Arc::clone(&effects);
        let consumer = tokio::spawn(async move {
            execute(job_domain,&job_token,Some("ab12"),"launch",json!({"task":"task"}),async move {
                count.fetch_add(1,Ordering::SeqCst);
                entered.send(()).unwrap();
                barrier.await.unwrap();
                Ok(json!({"node":"the-actually-started-node"}))
            }).await
        });
        started.await.unwrap();
        consumer.abort();
        assert!(consumer.await.unwrap_err().is_cancelled());
        assert_eq!(effects.load(Ordering::SeqCst),1);
        assert!(!domain.operations_live.lock().unwrap().is_empty());
        release.send(()).unwrap();
        domain.drain_batch_operations("run").await.unwrap();
        let repeated = execute(Arc::clone(&domain),&token,Some("ab12"),"launch",json!({"task":"task"}),async { panic!("a durable tool call cannot run twice") }).await.unwrap();
        assert_eq!(repeated,json!({"node":"the-actually-started-node"}));
        assert!(domain.confirmed_batch_effects("run",&["launch".into()]).unwrap());
        assert_eq!(effects.load(Ordering::SeqCst),1);
    }

    #[tokio::test]
    async fn operation_receipt_failure_prevents_the_physical_effect() {
        let home = Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let domain = Arc::new(DomainRuntime::open_at(&home,&home.join("domain.db")).unwrap());
        let token = domain.bind_orchestrator("run").unwrap().expose_for_child_environment().to_owned();
        let db = rusqlite::Connection::open(home.join("domain.db")).unwrap();
        db.execute_batch("CREATE TRIGGER refuse_tool_intent BEFORE INSERT ON orchestrator_tool_operations BEGIN SELECT RAISE(ABORT,'injected intent refusal'); END;").unwrap();
        let effects = AtomicUsize::new(0);
        let count = Arc::new(effects);
        let observed = Arc::clone(&count);
        let answer = execute(Arc::clone(&domain),&token,Some("ab12"),"launch",json!({"task":"task"}),async move { observed.fetch_add(1,Ordering::SeqCst); Ok(json!({"node":"never"})) }).await;
        assert_eq!(answer.unwrap_err().code,"storage_unavailable");
        assert_eq!(count.load(Ordering::SeqCst),0);
        assert!(domain.operations_live.lock().unwrap().is_empty());
        drop(db);
    }

    #[tokio::test]
    async fn a_restarted_or_uncertain_operation_is_not_offered_again() {
        let home = Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let domain = Arc::new(DomainRuntime::open_at(&home,&home.join("domain.db")).unwrap());
        let token = domain.bind_orchestrator("run").unwrap().expose_for_child_environment().to_owned();
        let admission = domain.begin_tool_operation(&token,Some("ab12"),"launch",&json!({"task":"task"})).unwrap();
        drop(admission);
        let answer = execute(Arc::clone(&domain),&token,Some("ab12"),"launch",json!({"task":"task"}),async { panic!("an uncertain previous effect cannot be replayed") }).await;
        assert_eq!(answer.unwrap_err().code,"operation_uncertain");
        assert!(!domain.confirmed_batch_effects("run",&["launch".into()]).unwrap());
        let changed = execute(Arc::clone(&domain),&token,Some("ab12"),"launch",json!({"task":"different"}),async { panic!("a reused identity cannot change its command") }).await;
        assert!(changed.is_err());
    }

    #[tokio::test]
    async fn a_refusal_is_settled_and_a_lost_outcome_is_named_in_the_log() {
        let home = Directory::new();
        std::fs::create_dir_all(&*home).unwrap();
        let domain = Arc::new(DomainRuntime::open_at(&home,&home.join("domain.db")).unwrap());
        let token = domain.bind_orchestrator("run").unwrap().expose_for_child_environment().to_owned();
        let refused = execute(Arc::clone(&domain),&token,Some("ab12"),"set_launch_plan",json!({"task":"task"}),async { Err(DomainError::new("conflict","the task or its launch plan changed")) }).await;
        assert_eq!(refused.unwrap_err().code,"conflict");
        assert!(domain.confirmed_batch_effects("run",&["set_launch_plan".into()]).unwrap(), "a refusal changed nothing");
        assert!(domain.unconfirmed_operations("run").is_empty());
        let lost = execute(Arc::clone(&domain),&token,Some("cd34"),"send",json!({"node":"node"}),async { Err(DomainError::new("operation_uncertain","its executor stopped")) }).await;
        assert_eq!(lost.unwrap_err().code,"operation_uncertain");
        assert!(!domain.confirmed_batch_effects("run",&["set_launch_plan".into(),"send".into()]).unwrap());
        assert_eq!(domain.unconfirmed_operations("run"),["send:uncertain(operation_uncertain)"]);
        let outcomes = domain.store.lock().unwrap().batch_outcomes("run",&domain.state().unwrap().data_epoch,"",32).unwrap();
        let mut statuses: Vec<_> = outcomes["entries"].as_array().unwrap().iter().map(|entry| (entry["kind"].as_str().unwrap().to_owned(),entry["status"].as_str().unwrap().to_owned())).collect();
        statuses.sort();
        assert_eq!(statuses,[("send".to_owned(),"uncertain".to_owned()),("set_launch_plan".to_owned(),"refused".to_owned())]);
    }
}
