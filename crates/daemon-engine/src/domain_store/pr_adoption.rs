use alondra_core::domain::{DomainState, OpenedPr};
use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction, params};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS pr_adoptions(host TEXT NOT NULL,repository TEXT NOT NULL,number INTEGER NOT NULL CHECK(number>0),PRIMARY KEY(host,repository,number));
CREATE TABLE IF NOT EXISTS pull_request_tasks(task_id TEXT NOT NULL REFERENCES tasks(id),pr_id TEXT NOT NULL REFERENCES pull_requests(id),PRIMARY KEY(task_id,pr_id));
CREATE INDEX IF NOT EXISTS pull_request_tasks_pr ON pull_request_tasks(pr_id,task_id);";

const OWNERSHIP: &str = "CREATE TRIGGER immutable_pr_ownership BEFORE UPDATE ON pull_requests
WHEN NEW.id<>OLD.id OR NEW.host<>OLD.host OR NEW.repository<>OLD.repository OR NEW.number<>OLD.number
OR (NEW.task_id<>OLD.task_id AND (SELECT state FROM tasks WHERE id=OLD.task_id) NOT IN('done','cancelled'))
BEGIN SELECT RAISE(ABORT,'PR identity and live ownership are immutable'); END;";

pub(super) fn upgrade(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(SCHEMA)?;
    tx.execute_batch("INSERT OR IGNORE INTO pull_request_tasks(task_id,pr_id) SELECT task_id,id FROM pull_requests;")?;
    // Historical deliveries and stacks keep the task that owned the PR when they were written.
    // Current ownership remains in pull_requests, guarded against taking a live task's PR.
    for name in ["tasks", "interaction_subjects", "delivery_targets", "pr_decisions", "merge_authorizations", "task_stacks", "stack_readiness"] {
        let table: String = tx.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name=?1", [name], |r| r.get(0))?;
        const OLD: &str = "REFERENCES pull_requests(task_id,id)";
        if !table.contains(OLD) { continue; }
        let attached = tx.prepare("SELECT type,sql FROM sqlite_master WHERE tbl_name=?1 AND type IN('trigger','index') AND sql IS NOT NULL")?
            .query_map([name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        tx.execute_batch(&format!("PRAGMA defer_foreign_keys=ON; CREATE TABLE pr_adoption_copy AS SELECT * FROM {name}; DROP TABLE {name};"))?;
        tx.execute_batch(&table.replace(OLD, "REFERENCES pull_request_tasks(task_id,pr_id)"))?;
        for (_, sql) in attached.iter().filter(|(kind, _)| kind == "index") { tx.execute_batch(sql)?; }
        tx.execute_batch(&format!("INSERT INTO {name} SELECT * FROM pr_adoption_copy; DROP TABLE pr_adoption_copy;"))?;
        for (_, sql) in attached.iter().filter(|(kind, _)| kind == "trigger") { tx.execute_batch(sql)?; }
    }
    tx.execute_batch("DROP TRIGGER IF EXISTS immutable_pr_ownership;")?;
    tx.execute_batch(OWNERSHIP)
}

pub(super) fn save(c: &Connection, before: &DomainState, after: &DomainState) -> Result<()> {
    ensure!(before.pr_adoptions.is_subset(&after.pr_adoptions), "PR adoption requests are retained");
    for pr in after.pr_adoptions.difference(&before.pr_adoptions) {
        super::repository::exec(c, "INSERT INTO pr_adoptions(host,repository,number) VALUES(?1,?2,?3)", params![pr.host, pr.repository, i64::try_from(pr.number)?])?;
    }
    for (pr, tasks) in &after.pr_task_history {
        ensure!(before.pr_task_history.get(pr).is_none_or(|old| old.is_subset(tasks)), "PR ownership history is retained");
        for task in tasks.iter().filter(|task| before.pr_task_history.get(pr).is_none_or(|old| !old.contains(*task))) {
            super::repository::exec(c, "INSERT OR IGNORE INTO pull_request_tasks(task_id,pr_id) VALUES(?1,?2)", params![task.0, pr.0])?;
        }
    }
    ensure!(before.pr_task_history.keys().all(|pr| after.pr_task_history.contains_key(pr)), "PR ownership history is retained");
    Ok(())
}

pub(super) fn load(c: &Connection, state: &mut DomainState) -> Result<()> {
    state.pr_task_history.clear();
    let mut previous = c.prepare("SELECT p.pr_id,p.task_id FROM pull_request_tasks p JOIN pull_requests r ON r.id=p.pr_id WHERE EXISTS(SELECT 1 FROM pull_request_tasks h WHERE h.pr_id=p.pr_id AND h.task_id<>r.task_id)")?;
    let pairs = previous.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for pair in pairs {
        let (pr, task) = pair?;
        state.pr_task_history.entry(pr.into()).or_default().insert(task.into());
    }
    state.pr_adoptions = c.prepare("SELECT host,repository,number FROM pr_adoptions")?.query_map([], |r|
        Ok(OpenedPr { host: r.get(0)?, repository: r.get(1)?, number: r.get::<_, i64>(2)? as u64 }))?.collect::<rusqlite::Result<_>>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_pr_ownership_migrates_without_losing_a_historical_delivery() {
        let mut c = Connection::open_in_memory().unwrap();
        let schema = include_str!("../../../alondrad/src/domain_store/schema.sql")
            .replace("REFERENCES pull_request_tasks(task_id,pr_id)", "REFERENCES pull_requests(task_id,id)");
        c.execute_batch(&schema).unwrap();
        c.execute_batch("DROP TABLE pull_request_tasks;
            INSERT INTO requests(id,body,closed) VALUES('r','Work',0);
            INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('old','r','Old','cancelled',1,0,0,0),('new','r','New','running',1,0,0,0);
            INSERT INTO pull_requests VALUES('pr','old','github.com','owner/repo',7,'head','main','open',NULL,'[\"squash\"]');
            UPDATE tasks SET pr_id='pr' WHERE id='old';
            INSERT INTO interactions(id,scope_kind,task_id,author_kind,recipient_kind,body,blocking,kind,revision,ordering,is_final,card)
                VALUES('delivery','task','old','system','user','Ready',0,'delivery',1,1,1,'{\"kind\":\"review\",\"need\":\"Review\"}');
            INSERT INTO delivery_targets(interaction_id,ordinal,task_id,kind,pr_id,head) VALUES('delivery',0,'old','pull_request','pr','head');").unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        let tx = c.transaction().unwrap();
        upgrade(&tx).unwrap();
        tx.execute_batch("UPDATE tasks SET pr_id=NULL WHERE id='old';
            UPDATE pull_requests SET task_id='new' WHERE id='pr';
            INSERT INTO pull_request_tasks VALUES('new','pr');
            UPDATE tasks SET pr_id='pr' WHERE id='new';").unwrap();
        tx.commit().unwrap();
        assert_eq!(c.query_row("SELECT task_id FROM delivery_targets WHERE interaction_id='delivery'", [], |r| r.get::<_, String>(0)).unwrap(), "old");
        assert!(c.execute("UPDATE pull_requests SET task_id='old' WHERE id='pr'", []).is_err(), "a live task keeps its PR");
        let tx = c.transaction().unwrap();
        upgrade(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(c.query_row("SELECT count(*) FROM pull_request_tasks", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert!(!c.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
    }
}
