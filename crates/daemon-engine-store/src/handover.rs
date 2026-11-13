//! The journal of each `claude --bg` node's move to the Agent SDK: one row per step it reached,
//! never changed or removed, each step only after the one before it. The daemon goes on from the
//! last step a node reached, so a crash repeats no effect of a step already recorded.
use super::*;

/// Created on every open like task_relations: a database from before the SDK handover gains it.
pub(super) const SDK_HANDOVER_STEPS: &str = concat!(
    "CREATE TABLE IF NOT EXISTS sdk_handover_steps(",
    "node TEXT NOT NULL CHECK(length(node)>0), ",
    "step TEXT NOT NULL CHECK(step IN('planned','bg_stopped','sdk_admitted','done')), ",
    "task_id TEXT NOT NULL REFERENCES tasks(id), ",
    "at INTEGER NOT NULL, ",
    "facts TEXT NOT NULL CHECK(json_valid(facts)), ",
    "PRIMARY KEY(node,step)); ",
    "CREATE TRIGGER IF NOT EXISTS sdk_handover_steps_kept BEFORE UPDATE ON sdk_handover_steps ",
    "BEGIN SELECT RAISE(ABORT,'an SDK handover step is never changed'); END; ",
    "CREATE TRIGGER IF NOT EXISTS sdk_handover_steps_retained BEFORE DELETE ON sdk_handover_steps ",
    "BEGIN SELECT RAISE(ABORT,'an SDK handover step is never removed'); END; ",
    "CREATE TRIGGER IF NOT EXISTS sdk_handover_steps_in_order BEFORE INSERT ON sdk_handover_steps ",
    "WHEN NEW.step<>'planned' AND NOT EXISTS(SELECT 1 FROM sdk_handover_steps WHERE node=NEW.node AND task_id=NEW.task_id ",
    "AND step=CASE NEW.step WHEN 'bg_stopped' THEN 'planned' WHEN 'sdk_admitted' THEN 'bg_stopped' ELSE 'sdk_admitted' END) ",
    "BEGIN SELECT RAISE(ABORT,'an SDK handover step comes only after the one before it'); END;",
);

const COPY_SUFFIX: &str = "before-sdk-handover";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Planned,
    BgStopped,
    SdkAdmitted,
    Done,
}

impl Step {
    fn key(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::BgStopped => "bg_stopped",
            Self::SdkAdmitted => "sdk_admitted",
            Self::Done => "done",
        }
    }
    fn parse(key: &str) -> Result<Self> {
        Ok(match key {
            "planned" => Self::Planned,
            "bg_stopped" => Self::BgStopped,
            "sdk_admitted" => Self::SdkAdmitted,
            "done" => Self::Done,
            other => anyhow::bail!("unknown SDK handover step {other}"),
        })
    }
}

/// A process named by its PID and the moment it started: a PID alone can be reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    pub pid: i32,
    pub started: (u64, u64),
}

/// What the plan of a node's move fixes: the session it resumes, where, the `claude --bg` process
/// that ran it, and the input that opens it through the SDK.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub session: String,
    pub cwd: String,
    pub name: String,
    pub process: Option<Process>,
    pub input: String,
    /// The outbox message the input is, handed with the move.
    pub outbox: Option<i64>,
    /// The node was brought back (lost, stalled or paused) rather than found at rest.
    pub returning: bool,
}

/// One node's move as far as it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handover {
    pub node: AgentId,
    pub task: TaskId,
    pub plan: Plan,
    pub step: Step,
    /// The `claude --bg` process seen exiting before the SDK took the session.
    pub exited: Option<Process>,
    /// The SDK node that took the task.
    pub agent: Option<AgentId>,
}

/// Where the copy of the database kept before the first move is.
pub fn copy_path(database: &Path) -> std::path::PathBuf {
    let mut name = database.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{COPY_SUFFIX}"));
    database.with_file_name(name)
}

impl DomainStore {
    /// Every node's move, as far as each went.
    pub fn handovers(&self) -> Result<Vec<Handover>> {
        handovers(&self.conn)
    }

    /// Records the plan of `node`'s move. The first plan this database records keeps a private
    /// copy of it first, as it was. False when the node already has a move.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn plan_handover(&mut self, node: &AgentId, task: &TaskId, plan: &Plan) -> Result<bool> {
        let first: bool = self.conn.query_row("SELECT NOT EXISTS(SELECT 1 FROM sdk_handover_steps)", [], |r| r.get(0))?;
        if first {
            let database = self.conn.path().map(std::path::PathBuf::from).context("the domain database has a path")?;
            lifecycle_migration::keep_copy_as(&self.conn, &database, &copy_path(&database))?;
        }
        let inserted = self.conn.execute(
            "INSERT INTO sdk_handover_steps(node,step,task_id,at,facts) VALUES(?1,'planned',?2,?3,?4) ON CONFLICT DO NOTHING",
            params![node.0, task.0, alondra_core::now_millis(), serde_json::to_string(plan)?],
        )?;
        Ok(inserted == 1)
    }

    /// The `claude --bg` process of `node`'s move exited, seen by its identity.
    pub fn record_bg_stopped(&mut self, node: &AgentId, task: &TaskId, exited: Option<Process>) -> Result<()> {
        insert_step(&self.conn, node, task, Step::BgStopped, &json!({"exited": exited}))
    }

    /// The move of `node` is over: the SDK node holds its task and the old node is closed.
    pub fn record_handover_done(&mut self, node: &AgentId, task: &TaskId) -> Result<()> {
        insert_step(&self.conn, node, task, Step::Done, &json!({}))
    }

    /// Hands the outbox message the move carried as its first input, once: it went with the move.
    pub fn hand_carried_message(&mut self, id: i64) -> Result<()> {
        if !self.pending_messages()?.iter().any(|message| message.id == id) {
            return Ok(());
        }
        if let Some(claim) = self.claim_message(id)? {
            ensure!(self.finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Handed, None)?,
                "domain delivery claim changed while finishing");
        }
        Ok(())
    }
}

impl OwnerTransaction<'_> {
    /// The SDK node `agent` took `node`'s task: recorded in the transaction that admits it.
    pub fn record_sdk_admitted(&mut self, node: &AgentId, task: &TaskId, agent: &AgentId) -> std::result::Result<(), StoreError> {
        self.sql(|conn| insert_step(conn, node, task, Step::SdkAdmitted, &json!({"agent": agent})))
    }
}

fn insert_step(conn: &Connection, node: &AgentId, task: &TaskId, step: Step, facts: &Value) -> Result<()> {
    conn.execute(
        "INSERT INTO sdk_handover_steps(node,step,task_id,at,facts) VALUES(?1,?2,?3,?4,?5)",
        params![node.0, step.key(), task.0, alondra_core::now_millis(), facts.to_string()],
    )?;
    Ok(())
}

fn handovers(conn: &Connection) -> Result<Vec<Handover>> {
    let mut statement = conn.prepare("SELECT node,step,task_id,facts FROM sdk_handover_steps ORDER BY node,at,rowid")?;
    let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?;
    let mut found: Vec<Handover> = vec![];
    for row in rows {
        let (node, step, task, facts) = row?;
        let (step, facts): (Step, Value) = (Step::parse(&step)?, serde_json::from_str(&facts)?);
        match step {
            Step::Planned => found.push(Handover {
                node: AgentId(node),
                task: TaskId::from(task.as_str()),
                plan: serde_json::from_value(facts)?,
                step,
                exited: None,
                agent: None,
            }),
            _ => {
                let handover = found.iter_mut().rev().find(|handover| handover.node.0 == node).context("an SDK handover step without its plan")?;
                handover.step = handover.step.max(step);
                match step {
                    Step::BgStopped => handover.exited = serde_json::from_value(facts["exited"].clone())?,
                    Step::SdkAdmitted => handover.agent = Some(serde_json::from_value(facts["agent"].clone())?),
                    _ => {}
                }
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (crate::test_support::Directory, DomainStore) {
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir(&dir).unwrap();
        let store = DomainStore::open(&dir.join("domain.db")).unwrap();
        (dir, store)
    }

    fn task(store: &mut DomainStore) -> TaskId {
        store.conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        TaskId::from("t-moved")
    }

    fn plan() -> Plan {
        Plan { session: "11111111-1111-4111-8111-111111111111".into(), cwd: "/work".into(), name: "node".into(), process: Some(Process { pid: 7, started: (1, 2) }), input: "go on".into(), outbox: None, returning: false }
    }

    #[test]
    fn a_move_goes_only_forward_and_its_steps_are_never_changed_or_removed() {
        let (dir, mut store) = store();
        let task = task(&mut store);
        let node = AgentId("11111111-1111-4111-8111-111111111111".into());
        assert!(store.plan_handover(&node, &task, &plan()).unwrap());
        assert!(!store.plan_handover(&node, &task, &plan()).unwrap(), "a node is moved once");
        let skipped = insert_step(&store.conn, &node, &task, Step::SdkAdmitted, &json!({"agent": "sdk-1"})).unwrap_err();
        assert!(format!("{skipped:#}").contains("only after the one before it"), "{skipped:#}");
        store.record_bg_stopped(&node, &task, Some(Process { pid: 7, started: (1, 2) })).unwrap();
        for (statement, refusal) in [("UPDATE sdk_handover_steps SET at=0", "never changed"), ("DELETE FROM sdk_handover_steps", "never removed")] {
            let refused = store.conn.execute(statement, []).unwrap_err().to_string();
            assert!(refused.contains(refusal), "{refused}");
        }
        let moved = store.handovers().unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!((moved[0].step, moved[0].exited, moved[0].plan.clone()), (Step::BgStopped, Some(Process { pid: 7, started: (1, 2) }), plan()));
        drop(store);
        let reopened = Connection::open(dir.join("domain.db")).unwrap();
        assert_eq!(handovers(&reopened).unwrap()[0].step, Step::BgStopped, "a daemon that comes back reads the step recorded");
    }

    #[test]
    fn the_first_move_keeps_a_copy_of_the_database_as_it_was() {
        let (dir, mut store) = store();
        let task = task(&mut store);
        let copy = copy_path(&dir.join("domain.db").canonicalize().unwrap());
        assert!(!copy.exists());
        store.plan_handover(&AgentId("a".into()), &task, &plan()).unwrap();
        let kept = Connection::open(&copy).unwrap();
        let steps: i64 = kept.query_row("SELECT count(*) FROM sdk_handover_steps", [], |r| r.get(0)).unwrap();
        assert_eq!(steps, 0, "the copy is the database before its first move");
        drop(kept);
        let first = std::fs::metadata(&copy).unwrap().modified().unwrap();
        store.plan_handover(&AgentId("b".into()), &task, &plan()).unwrap();
        assert_eq!(std::fs::metadata(&copy).unwrap().modified().unwrap(), first, "only the first move copies it");
    }
}
