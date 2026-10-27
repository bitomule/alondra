use alondra_core::accounts::Provider;
use alondra_core::domain::{DomainState, TaskId};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use super::repository::exec;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS task_providers(task_id TEXT PRIMARY KEY REFERENCES tasks(id), provider TEXT NOT NULL CHECK(provider IN('claude','codex')));\
CREATE TABLE IF NOT EXISTS account_configuration(singleton INTEGER PRIMARY KEY CHECK(singleton=1), account_order TEXT NOT NULL CHECK(json_valid(account_order)), config_version TEXT NOT NULL);\
CREATE TABLE IF NOT EXISTS account_model_catalogs(provider TEXT NOT NULL, account TEXT NOT NULL, catalog TEXT NOT NULL CHECK(json_valid(catalog)), PRIMARY KEY(provider,account));\
CREATE TABLE IF NOT EXISTS task_selections(task_id TEXT PRIMARY KEY REFERENCES tasks(id), selection TEXT NOT NULL CHECK(json_valid(selection)), pending TEXT CHECK(pending IS NULL OR json_valid(pending)));\
CREATE TABLE IF NOT EXISTS model_profiles(singleton INTEGER PRIMARY KEY CHECK(singleton=1), profiles TEXT NOT NULL CHECK(json_valid(profiles)), config_version TEXT NOT NULL);\
CREATE TABLE IF NOT EXISTS schedule_paid_accounts(schedule_id TEXT PRIMARY KEY REFERENCES schedules(id), account TEXT NOT NULL CHECK(json_valid(account)));";

pub(super) fn save(c: &Connection, before: &DomainState, after: &DomainState) -> Result<()> {
    for schedule in after.schedule.schedules.values().filter(|schedule| before.schedule.schedules.get(&schedule.id).and_then(|prior| prior.paid_account_grant.as_ref()) != schedule.paid_account_grant.as_ref()) {
        match &schedule.paid_account_grant {
            Some(account) => { exec(c, "INSERT INTO schedule_paid_accounts(schedule_id,account) VALUES(?1,?2) ON CONFLICT(schedule_id) DO UPDATE SET account=excluded.account", params![schedule.id.0, serde_json::to_string(account)?])?; }
            None => { exec(c, "DELETE FROM schedule_paid_accounts WHERE schedule_id=?1", [&schedule.id.0])?; }
        }
    }
    for task in after.tasks.values().filter(|task| before.tasks.get(&task.id).map_or(task.provider != Provider::Claude, |prior| prior.provider != task.provider)) {
        if task.provider != Provider::Claude {
            exec(c, "INSERT INTO task_providers(task_id,provider) VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET provider=excluded.provider", params![task.id.0, task.provider.as_str()])?;
        } else {
            exec(c, "DELETE FROM task_providers WHERE task_id=?1", params![task.id.0])?;
        }
    }
    for task in after.tasks.values().filter(|task| before.tasks.get(&task.id).map_or(task.selection.is_some(), |prior| prior.selection != task.selection || prior.pending_selection != task.pending_selection)) {
        if let Some(selection) = &task.selection {
            exec(c, "INSERT INTO task_selections(task_id,selection,pending) VALUES(?1,?2,?3) ON CONFLICT(task_id) DO UPDATE SET selection=excluded.selection,pending=excluded.pending",
                params![task.id.0, serde_json::to_string(selection)?, task.pending_selection.as_ref().map(serde_json::to_string).transpose()?])?;
        } else {
            exec(c, "DELETE FROM task_selections WHERE task_id=?1", params![task.id.0])?;
        }
    }
    if before.account_order != after.account_order || before.account_config_version != after.account_config_version {
        exec(c, "INSERT INTO account_configuration(singleton,account_order,config_version) VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET account_order=excluded.account_order,config_version=excluded.config_version",
            params![serde_json::to_string(&after.account_order)?, after.account_config_version])?;
    }
    if before.model_profiles != after.model_profiles || before.model_profiles_version != after.model_profiles_version {
        exec(c, "INSERT INTO model_profiles(singleton,profiles,config_version) VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET profiles=excluded.profiles,config_version=excluded.config_version",
            params![serde_json::to_string(&after.model_profiles)?, after.model_profiles_version])?;
    }
    for gone in before.model_catalogs.iter().filter(|old| !after.model_catalogs.iter().any(|new| old.account == new.account)) {
        exec(c, "DELETE FROM account_model_catalogs WHERE provider=?1 AND account=?2", params![gone.account.provider.as_str(), gone.account.account.as_str()])?;
    }
    for catalog in after.model_catalogs.iter().filter(|new| !before.model_catalogs.contains(new)) {
        exec(c, "INSERT INTO account_model_catalogs(provider,account,catalog) VALUES(?1,?2,?3) ON CONFLICT(provider,account) DO UPDATE SET catalog=excluded.catalog",
            params![catalog.account.provider.as_str(), catalog.account.account.as_str(), serde_json::to_string(catalog)?])?;
    }
    Ok(())
}

pub(super) fn load(c: &Connection, state: &mut DomainState) -> Result<()> {
    let exists = |table: &str| c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)", [table], |row| row.get::<_, bool>(0));
    if exists("schedule_paid_accounts")? {
        let mut statement = c.prepare("SELECT schedule_id,account FROM schedule_paid_accounts")?;
        for row in statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
            let (id, account) = row?;
            state.schedule.schedules.get_mut(&alondra_core::domain::ScheduleId(id)).context("paid grant schedule missing")?.paid_account_grant = Some(serde_json::from_str(&account)?);
        }
    }
    if exists("task_providers")? {
        let mut statement = c.prepare("SELECT task_id,provider FROM task_providers")?;
        for row in statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
            let (id, provider) = row?;
            state.tasks.get_mut(&TaskId::from(id)).context("provider task missing")?.provider = Provider::parse(&provider).map_err(anyhow::Error::msg)?;
        }
    }
    if exists("account_configuration")?
        && let Some((order, version)) = c.query_row("SELECT account_order,config_version FROM account_configuration WHERE singleton=1", [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).optional()?
    {
        state.account_order = serde_json::from_str(&order)?;
        state.account_config_version = version;
    }
    if exists("task_selections")? {
        let mut statement = c.prepare("SELECT task_id,selection,pending FROM task_selections")?;
        for row in statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)))? {
            let (id, selection, pending) = row?;
            let task = state.tasks.get_mut(&TaskId::from(id)).context("selection task missing")?;
            task.selection = Some(serde_json::from_str(&selection)?);
            task.pending_selection = pending.as_deref().map(serde_json::from_str).transpose()?;
        }
    }
    if exists("account_model_catalogs")? {
        let mut statement = c.prepare("SELECT catalog FROM account_model_catalogs ORDER BY provider,account")?;
        for row in statement.query_map([], |row| row.get::<_, String>(0))? {
            state.model_catalogs.push(serde_json::from_str(&row?)?);
        }
    }
    if exists("model_profiles")?
        && let Some((profiles, version)) = c.query_row("SELECT profiles,config_version FROM model_profiles WHERE singleton=1", [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).optional()?
    {
        state.model_profiles = serde_json::from_str(&profiles)?;
        state.model_profiles_version = version;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::accounts::{AccountId, AccountKey};
    use alondra_core::model_selection::AccountModelCatalog;

    #[test]
    fn pre_provider_stores_have_no_phantom_accounts_or_catalogs() {
        let c = Connection::open_in_memory().unwrap();
        let mut state = DomainState::new("fixture");
        load(&c, &mut state).unwrap();
        assert!(state.accounts.is_empty());
        assert!(state.account_order.is_empty());
        assert!(state.model_catalogs.is_empty());
    }

    #[test]
    fn global_order_and_colliding_catalog_slugs_survive_restarts_and_removal() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        let before = DomainState::new("fixture");
        let mut after = before.clone();
        let named = AccountId::parse("work").unwrap();
        let keys = [AccountKey::new(Provider::Codex, named.clone()), AccountKey::new(Provider::Claude, named)];
        after.account_order = keys.to_vec();
        after.account_config_version = "config-hash".into();
        after.model_catalogs = keys.iter().map(|key| AccountModelCatalog {
            account: key.clone(), revision: key.provider.as_str().into(), models: vec![], native_default: None, measured_at: 1,
        }).collect();
        save(&c, &before, &after).unwrap();
        let mut loaded = before.clone();
        load(&c, &mut loaded).unwrap();
        assert_eq!(loaded.account_order, keys);
        assert_eq!(loaded.account_config_version, "config-hash");
        assert_eq!(loaded.model_catalogs.len(), 2);
        after.model_catalogs.retain(|catalog| catalog.account.provider == Provider::Codex);
        save(&c, &loaded, &after).unwrap();
        loaded.model_catalogs.clear();
        load(&c, &mut loaded).unwrap();
        assert_eq!(loaded.model_catalogs[0].account, keys[0]);
        assert_eq!(loaded.model_catalogs.len(), 1);
    }
}
