use super::*;

impl DomainStore {
    pub fn public_task_context(&self, task: &TaskId, after: Option<&alondra_store::ContextRef>, limit: usize) -> Result<alondra_store::ContextPage> {
        let scope = alondra_store::AuthorityScope::new(epoch_of(&self.conn)?)?;
        let transaction = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Deferred)?;
        let journal = alondra_store::LifecycleTransaction::attach(transaction, scope)?;
        Ok(journal.context_page(task, after, limit)?)
    }
}
