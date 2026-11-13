//! The journal of each move of a task to another account when its own reaches its usage limit:
//! one row per step it reached, never changed or removed, each step only after the one before it.
//! A move is planned, its old host stopped, the execution on the other account admitted, then
//! done; until its admission it can instead be undone, and the task stays on its account's limit.
//! The daemon goes on from the last step a move reached, so a crash repeats no effect of a step
//! already recorded.
use super::*;

/// Created on every open like the SDK handover's journal: a database from before it gains it.
pub(super) const ACCOUNT_MOVE_STEPS: &str = concat!(
    "CREATE TABLE IF NOT EXISTS account_move_steps(",
    "task_id TEXT NOT NULL REFERENCES tasks(id), ",
    "number INTEGER NOT NULL CHECK(number>0), ",
    "step TEXT NOT NULL CHECK(step IN('planned','host_stopped','admitted','done','undone')), ",
    "at INTEGER NOT NULL, ",
    "facts TEXT NOT NULL CHECK(json_valid(facts)), ",
    "PRIMARY KEY(task_id,number,step)); ",
    "CREATE TRIGGER IF NOT EXISTS account_move_steps_kept BEFORE UPDATE ON account_move_steps ",
    "BEGIN SELECT RAISE(ABORT,'an account move step is never changed'); END; ",
    "CREATE TRIGGER IF NOT EXISTS account_move_steps_retained BEFORE DELETE ON account_move_steps ",
    "BEGIN SELECT RAISE(ABORT,'an account move step is never removed'); END; ",
    "CREATE TRIGGER IF NOT EXISTS account_move_steps_one_open BEFORE INSERT ON account_move_steps ",
    "WHEN NEW.step='planned' AND EXISTS(SELECT 1 FROM account_move_steps AS planned WHERE planned.task_id=NEW.task_id AND planned.step='planned' ",
    "AND NOT EXISTS(SELECT 1 FROM account_move_steps AS ended WHERE ended.task_id=planned.task_id AND ended.number=planned.number AND ended.step IN('done','undone'))) ",
    "BEGIN SELECT RAISE(ABORT,'a task has one open account move at a time'); END; ",
    "CREATE TRIGGER IF NOT EXISTS account_move_steps_in_order BEFORE INSERT ON account_move_steps ",
    "WHEN NEW.step<>'planned' AND (NOT EXISTS(SELECT 1 FROM account_move_steps WHERE task_id=NEW.task_id AND number=NEW.number ",
    "AND step=CASE NEW.step WHEN 'host_stopped' THEN 'planned' WHEN 'admitted' THEN 'host_stopped' WHEN 'done' THEN 'admitted' ELSE 'planned' END) ",
    "OR EXISTS(SELECT 1 FROM account_move_steps WHERE task_id=NEW.task_id AND number=NEW.number AND step IN('done','undone')) ",
    "OR (NEW.step='undone' AND EXISTS(SELECT 1 FROM account_move_steps WHERE task_id=NEW.task_id AND number=NEW.number AND step='admitted'))) ",
    "BEGIN SELECT RAISE(ABORT,'an account move step comes only after the one before it'); END;",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Planned,
    HostStopped,
    Admitted,
    Done,
    Undone,
}

impl Step {
    fn key(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::HostStopped => "host_stopped",
            Self::Admitted => "admitted",
            Self::Done => "done",
            Self::Undone => "undone",
        }
    }
    fn parse(key: &str) -> Result<Self> {
        Ok(match key {
            "planned" => Self::Planned,
            "host_stopped" => Self::HostStopped,
            "admitted" => Self::Admitted,
            "done" => Self::Done,
            "undone" => Self::Undone,
            other => anyhow::bail!("unknown account move step {other}"),
        })
    }
    /// The move is over, done or undone: nothing of it is left to do.
    pub fn ended(self) -> bool {
        matches!(self, Self::Done | Self::Undone)
    }
}

/// What the plan of a move fixes: the account the task leaves and the one it goes on with, the
/// card of the limit it leaves, the execution that hit it and its Claude session, which the
/// execution on the other account resumes in the same folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub from: alondra_core::accounts::AccountId,
    pub to: alondra_core::accounts::AccountId,
    pub card: InteractionId,
    pub node: AgentId,
    pub instance: String,
    pub session: String,
    pub cwd: String,
    /// Until when the task moves no more: the reset of the limit it left.
    pub until: i64,
}

/// One move as far as it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountMove {
    pub task: TaskId,
    pub number: i64,
    pub plan: Plan,
    pub step: Step,
    /// The node of the execution admitted on the other account.
    pub agent: Option<AgentId>,
    /// Why a move was undone.
    pub undone: Option<String>,
}

impl DomainStore {
    /// Every account move, as far as each went, oldest first.
    pub fn account_moves(&self) -> Result<Vec<AccountMove>> {
        account_moves(&self.conn)
    }

    /// Records the plan of the task's next move; refused while another of its moves is open.
    pub fn plan_account_move(&mut self, _task: &TaskId, _plan: &Plan) -> Result<i64> {
        anyhow::bail!("Accounts and providers are chosen when creating a node. An existing execution cannot move to another account")
    }

    /// No host runs the session the move takes any more.
    pub fn record_move_host_stopped(&mut self, task: &TaskId, number: i64) -> Result<()> {
        insert_step(&self.conn, task, number, Step::HostStopped, &json!({}))
    }

    /// The move is over: the execution on the other account runs the task.
    pub fn record_move_done(&mut self, task: &TaskId, number: i64) -> Result<()> {
        insert_step(&self.conn, task, number, Step::Done, &json!({}))
    }

    /// What the provider keeps of the task's conversation `conversation`: for Claude, its session.
    pub fn conversation_metadata(&self, task: &TaskId, conversation: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.conn.query_row(
            "SELECT metadata FROM provider_conversation_metadata WHERE task_id=?1 AND conversation_id=?2 ORDER BY rowid DESC LIMIT 1",
            params![task.0, conversation],
            |r| r.get(0),
        ).optional()?)
    }

    /// The launch placement of this admitted conversation, independent of later worktrees.
    pub fn conversation_directory(&self, task: &TaskId, conversation: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row(
            "SELECT d.placement FROM lifecycle_dispatches d JOIN lifecycle_executions e ON e.task_id=d.task_id AND e.generation=d.generation WHERE e.task_id=?1 AND e.conversation_id=?2 AND d.kind='launch' AND d.placement_kind='local' ORDER BY d.id DESC LIMIT 1",
            params![task.0, conversation], |row| row.get(0),
        ).optional()?)
    }

    /// The move did not happen: the task stays on its account's limit, for `why`.
    pub fn record_move_undone(&mut self, task: &TaskId, number: i64, why: &str) -> Result<()> {
        insert_step(&self.conn, task, number, Step::Undone, &json!({"why": why}))
    }
}

impl OwnerTransaction<'_> {
    /// The execution on the other account was admitted as `agent`: recorded in the transaction
    /// that admits it.
    pub fn record_move_admitted(&mut self, task: &TaskId, number: i64, agent: &AgentId) -> std::result::Result<(), StoreError> {
        self.sql(|conn| insert_step(conn, task, number, Step::Admitted, &json!({"agent": agent})))
    }
}

fn insert_step(conn: &Connection, task: &TaskId, number: i64, step: Step, facts: &Value) -> Result<()> {
    conn.execute(
        "INSERT INTO account_move_steps(task_id,number,step,at,facts) VALUES(?1,?2,?3,?4,?5)",
        params![task.0, number, step.key(), alondra_core::now_millis(), facts.to_string()],
    )?;
    Ok(())
}

fn account_moves(conn: &Connection) -> Result<Vec<AccountMove>> {
    let mut statement = conn.prepare("SELECT task_id,number,step,facts FROM account_move_steps ORDER BY rowid")?;
    let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?;
    let mut found: Vec<AccountMove> = vec![];
    for row in rows {
        let (task, number, step, facts) = row?;
        let (step, facts): (Step, Value) = (Step::parse(&step)?, serde_json::from_str(&facts)?);
        if step == Step::Planned {
            found.push(AccountMove { task: TaskId::from(task.as_str()), number, plan: serde_json::from_value(facts)?, step, agent: None, undone: None });
            continue;
        }
        let moving = found.iter_mut().find(|moving| moving.task.0 == task && moving.number == number).context("an account move step without its plan")?;
        moving.step = step;
        match step {
            Step::Admitted => moving.agent = Some(serde_json::from_value(facts["agent"].clone())?),
            Step::Undone => moving.undone = facts["why"].as_str().map(str::to_owned),
            _ => {}
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
        store.conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        (dir, store)
    }

    fn plan() -> Plan {
        Plan {
            from: alondra_core::accounts::AccountId::parse("work").unwrap(), to: alondra_core::accounts::AccountId::default_account(),
            card: "claude-usage-limit-until-4102444800-t-7".into(), node: AgentId("sdk-1".into()), instance: "sdk-1".into(),
            session: "11111111-2222-4333-8444-555555555555".into(), cwd: "/work".into(), until: 4_102_444_800_000,
        }
    }

    #[test]
    fn a_move_goes_only_forward_one_at_a_time_and_its_steps_are_never_changed_or_removed() {
        let (dir, mut store) = store();
        let task = TaskId::from("t-moved");
        assert!(store.plan_account_move(&task,&plan()).unwrap_err().to_string().contains("creating a node"));
        let first = 1;
        insert_step(&store.conn,&task,first,Step::Planned,&serde_json::to_value(plan()).unwrap()).unwrap();
        assert!(insert_step(&store.conn,&task,first+1,Step::Planned,&serde_json::to_value(plan()).unwrap()).is_err(), "historical one-open-move invariant is retained");
        let skipped = store.record_move_done(&task, first).unwrap_err();
        assert!(format!("{skipped:#}").contains("only after the one before it"), "{skipped:#}");
        store.record_move_host_stopped(&task, first).unwrap();
        for (statement, refusal) in [("UPDATE account_move_steps SET at=0", "never changed"), ("DELETE FROM account_move_steps", "never removed")] {
            let refused = store.conn.execute(statement, []).unwrap_err().to_string();
            assert!(refused.contains(refusal), "{refused}");
        }
        store.record_move_undone(&task, first, "the other account ran out too").unwrap();
        assert!(store.record_move_host_stopped(&task, first).is_err(), "an undone move takes no further step");
        let second=first+1;
        insert_step(&store.conn,&task,second,Step::Planned,&serde_json::to_value(plan()).unwrap()).unwrap();
        assert_eq!(second, first + 1, "an ended move lets the next one start");
        store.record_move_host_stopped(&task, second).unwrap();
        store.conn.execute(
            "INSERT INTO account_move_steps(task_id,number,step,at,facts) VALUES(?1,?2,'admitted',0,?3)",
            params![task.0, second, json!({"agent": "sdk-2"}).to_string()],
        ).unwrap();
        assert!(store.record_move_undone(&task, second, "too late").is_err(), "an admitted move is never undone");
        store.record_move_done(&task, second).unwrap();
        drop(store);
        let reopened = Connection::open(dir.join("domain.db")).unwrap();
        let moves = account_moves(&reopened).unwrap();
        assert_eq!(moves.iter().map(|moving| (moving.number, moving.step)).collect::<Vec<_>>(), [(first, Step::Undone), (second, Step::Done)]);
        assert_eq!(moves[0].undone.as_deref(), Some("the other account ran out too"));
        assert_eq!((moves[1].agent.clone(), moves[1].plan.clone()), (Some(AgentId("sdk-2".into())), plan()), "a daemon that comes back reads what was recorded");
    }
}
