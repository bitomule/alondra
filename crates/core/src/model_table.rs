//! The "Available models" table the orchestrator reads in `machine`: each model an enabled
//! account advertises, the accounts that serve it with their margin, its sheet and how it did in
//! Alondra's own ended tasks, overall and by kind of task.
use std::collections::BTreeMap;

use crate::accounts::{AccountKey, Provider};
use crate::metrics::report::{TaskRow, provider_of};
use crate::model_selection::AccountModelCatalog;
use crate::model_sheets::sheet_for;

/// How many tasks of a kind a model needs before its results for that kind are shown.
pub const KIND_SAMPLE: i64 = 3;
const KINDS_SHOWN: usize = 2;

/// An enabled account and what is left of it, from 0 to 1, when known.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountRoom {
    pub account: AccountKey,
    pub name: String,
    pub margin: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub tasks: i64,
    pub clean: i64,
    pub cost_micros: i64,
    pub costed: i64,
}

impl Tally {
    fn add(&mut self, task: &TaskRow) {
        self.tasks += 1;
        self.clean += i64::from(task.clean);
        if let Some(cost) = task.spend.and_then(|spend| spend.cost_micros) {
            self.cost_micros += cost;
            self.costed += 1;
        }
    }

    fn text(&self) -> String {
        let first_time = (self.clean * 100 + self.tasks / 2) / self.tasks.max(1);
        match self.costed {
            0 => format!("{first_time}%"),
            costed => format!("{first_time}% ${:.2}", self.cost_micros as f64 / costed as f64 / 1_000_000.0),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Results {
    pub overall: Tally,
    pub by_kind: BTreeMap<String, Tally>,
}

/// The name a model's results are kept under: a Claude model by its family (`opus`), whether it
/// was recorded as an alias, a full id or provider-qualified; any other by its own id.
fn measured_key(provider: Provider, model: &str) -> String {
    let bare = model.split_once('/').filter(|(qualifier, _)| Provider::parse(qualifier).is_ok()).map_or(model, |(_, rest)| rest);
    match provider {
        Provider::Claude => crate::model_label::model_family(bare),
        Provider::Codex => bare.to_ascii_lowercase(),
    }
}

/// What each model's ended tasks came to, by provider and model.
pub fn results(tasks: &[TaskRow]) -> BTreeMap<(Provider, String), Results> {
    let mut all: BTreeMap<(Provider, String), Results> = BTreeMap::new();
    for task in tasks {
        let (Some(model), Some(provider)) = (task.model.as_deref(), provider_of(task.provider.as_deref(), task.model.as_deref())) else { continue };
        let results = all.entry((provider, measured_key(provider, model))).or_default();
        results.overall.add(task);
        if let Some(kind) = &task.kind {
            results.by_kind.entry(kind.clone()).or_default().add(task);
        }
    }
    all
}

struct Row<'a> {
    provider: Provider,
    model: &'a str,
    efforts: Vec<&'a str>,
    accounts: Vec<&'a AccountRoom>,
}

fn margin(room: &AccountRoom) -> String {
    match room.margin {
        Some(margin) => format!("{} {:.0}%", room.name, margin * 100.0),
        None => format!("{} ?", room.name),
    }
}

/// The table, or nothing when no enabled account advertises a model.
pub fn available_models(catalogs: &[AccountModelCatalog], accounts: &[AccountRoom], tasks: &[TaskRow]) -> Option<String> {
    let mut rows: Vec<Row<'_>> = vec![];
    for catalog in catalogs {
        let Some(room) = accounts.iter().find(|room| room.account == catalog.account) else { continue };
        for entry in catalog.models.iter().filter(|entry| !entry.hidden) {
            let provider = catalog.account.provider;
            let at = match rows.iter().position(|row| row.provider == provider && row.model == entry.model) {
                Some(at) => at,
                None => {
                    rows.push(Row { provider, model: &entry.model, efforts: vec![], accounts: vec![] });
                    rows.len() - 1
                }
            };
            let row = &mut rows[at];
            for effort in &entry.reasoning_efforts {
                if !row.efforts.contains(&effort.as_str()) { row.efforts.push(effort); }
            }
            if !row.accounts.iter().any(|known| known.account == room.account) { row.accounts.push(room); }
        }
    }
    if rows.is_empty() {
        return None;
    }
    let measured = results(tasks);
    let mut out = String::from("Available models: model | efforts | account margin | sheet: cost, good for, caution | measured: tasks, % done first time, mean cost; kinds with 3+\n");
    for row in rows {
        let mut parts = vec![format!("{}/{}", row.provider.as_str(), row.model)];
        if !row.efforts.is_empty() { parts.push(row.efforts.join(",")); }
        parts.push(row.accounts.iter().map(|room| margin(room)).collect::<Vec<_>>().join(", "));
        parts.push(match sheet_for(row.provider, row.model) {
            Some(sheet) => format!("{} cost, {}, {}", sheet.cost.as_str(), sheet.good_for.iter().map(|kind| kind.as_str()).collect::<Vec<_>>().join("/"), sheet.cautions),
            None => "no sheet".into(),
        });
        parts.push(match measured.get(&(row.provider, measured_key(row.provider, row.model))) {
            Some(results) => {
                let mut kinds: Vec<_> = results.by_kind.iter().filter(|(_, tally)| tally.tasks >= KIND_SAMPLE).collect();
                kinds.sort_by(|a, b| b.1.tasks.cmp(&a.1.tasks).then_with(|| a.0.cmp(b.0)));
                let mut text = format!("{}: {}", results.overall.tasks, results.overall.text());
                for (kind, tally) in kinds.into_iter().take(KINDS_SHOWN) {
                    text.push_str(&format!("; {kind} {}: {}", tally.tasks, tally.text()));
                }
                text
            }
            None => "not measured".into(),
        });
        out.push_str(&parts.join(" | "));
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountId;
    use crate::metrics::fold::Outcome;
    use crate::metrics::report::{Spend, Tokens};
    use crate::model_selection::CatalogModel;

    fn model(id: &str, efforts: &[&str], hidden: bool) -> CatalogModel {
        CatalogModel { model: id.into(), display_name: id.into(), reasoning_efforts: efforts.iter().map(|e| (*e).into()).collect(),
            default_reasoning_effort: None, context_window: None, input_modalities: vec![], is_default: false, hidden }
    }

    fn catalog(account: &AccountKey, models: Vec<CatalogModel>) -> AccountModelCatalog {
        AccountModelCatalog { account: account.clone(), revision: "r".into(), models, native_default: None, measured_at: 0 }
    }

    fn task(model: &str, provider: Option<&str>, kind: Option<&str>, clean: bool, cost: Option<i64>) -> TaskRow {
        TaskRow {
            project: None, model: Some(model.into()), effort: None, approach: None, provider: provider.map(Into::into), kind: kind.map(Into::into),
            outcome: Outcome::Done, started_at: None, finished_at: 0, clean, change_rounds: i64::from(!clean), reviewer_rounds: 0, interventions: 0,
            reopened: false, relaunched: false, work_ms: 0, waiting_user_ms: 0, queued_ms: 0, blocked_ms: 0, sessions: 1, steps: 0, subagent_steps: 0,
            looks: 0, polls: 0, pr_merged: false, pr_lines: None, spend: Some(Spend { cost_micros: cost, tokens: Tokens::default() }),
        }
    }

    fn work() -> AccountKey {
        AccountKey::new(Provider::Claude, AccountId::parse("work").unwrap())
    }

    fn rooms() -> Vec<AccountRoom> {
        vec![
            AccountRoom { account: AccountKey::native(Provider::Claude), name: "Personal".into(), margin: Some(0.62) },
            AccountRoom { account: work(), name: "Work".into(), margin: None },
            AccountRoom { account: AccountKey::native(Provider::Codex), name: "Codex".into(), margin: Some(0.9) },
        ]
    }

    #[test]
    fn each_model_of_an_enabled_account_gets_one_row_with_its_accounts_sheet_and_results() {
        let personal = catalog(&AccountKey::native(Provider::Claude), vec![model("opus", &["high", "medium"], false), model("sonnet", &["medium"], false), model("secret", &[], true)]);
        let shared = catalog(&work(), vec![model("sonnet", &["low", "medium"], false)]);
        let codex = catalog(&AccountKey::native(Provider::Codex), vec![model("gpt-6.1-sol", &["high"], false), model("o9", &[], false)]);
        let disabled = catalog(&AccountKey::new(Provider::Codex, AccountId::parse("off").unwrap()), vec![model("gpt-6-astra", &[], false)]);
        let mut tasks = vec![
            task("sonnet", None, Some("docs"), true, Some(1_000_000)), task("claude/sonnet", Some("claude"), Some("docs"), true, Some(2_000_000)),
            task("claude-sonnet-5-5", None, Some("docs"), false, Some(3_000_000)), task("sonnet", None, Some("fix"), true, None),
            task("codex/gpt-6.1-sol", Some("codex"), Some("migration"), true, Some(1_920_000)),
        ];
        tasks.push(TaskRow { model: None, ..task("opus", None, None, true, None) });
        let table = available_models(&[personal, shared, codex, disabled], &rooms(), &tasks).unwrap();
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 5, "{table}");
        assert!(lines[0].starts_with("Available models"));
        assert_eq!(lines[1], "claude/opus | high,medium | Personal 62% | high cost, feature/refactor/design/review/research/migration, Priciest Claude tier; wasted on mechanical work | not measured");
        assert_eq!(lines[2], "claude/sonnet | medium,low | Personal 62%, Work ? | medium cost, feature/fix/refactor/tests/docs, Below Opus on TB4 (61.8% vs 64.8%) | 4: 75% $2.00; docs 3: 67% $2.00");
        assert_eq!(lines[3], "codex/gpt-6.1-sol | high | Codex 90% | medium cost, fix/tests/migration/refactor/review/docs, Below the Claude Code runs on TB4 (58.2%) | 1: 100% $1.92");
        assert_eq!(lines[4], "codex/o9 | Codex 90% | no sheet | not measured");
        assert!(!table.contains("secret") && !table.contains("astra"), "hidden models and disabled accounts stay out");
    }

    #[test]
    fn no_enabled_account_means_no_table() {
        let personal = catalog(&AccountKey::native(Provider::Claude), vec![model("opus", &[], false)]);
        assert_eq!(available_models(&[personal], &[], &[]), None);
    }

    #[test]
    fn six_measured_models_fit_in_a_kilobyte_and_a_half() {
        let ids = ["opus", "sonnet", "haiku", "fable"];
        let claude = catalog(&AccountKey::native(Provider::Claude), ids.iter().map(|id| model(id, &["low", "medium", "high"], false)).collect());
        let codex = catalog(&AccountKey::native(Provider::Codex), ["gpt-6.1-sol", "gpt-6-astra"].iter().map(|id| model(id, &["low", "medium", "high"], false)).collect());
        let mut tasks = vec![];
        for id in ids.iter().map(|id| format!("claude/{id}")).chain(["codex/gpt-6.1-sol".to_owned(), "codex/gpt-6-astra".to_owned()]) {
            for kind in ["feature", "fix", "docs", "tests"] {
                for n in 0..4 { tasks.push(task(&id, None, Some(kind), n % 2 == 0, Some(1_234_567))); }
            }
        }
        let table = available_models(&[claude, codex], &rooms(), &tasks).unwrap();
        assert!(table.len() < 1536, "{} bytes:\n{table}", table.len());
        assert!(table.contains("| 16: 50% $1.23; docs 4: 50% $1.23; feature 4: 50% $1.23\n"), "{table}");
    }
}
