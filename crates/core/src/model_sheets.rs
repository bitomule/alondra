//! The model sheets Alondra ships (`model_sheets.toml`): what is publicly known of each model
//! before Alondra has measured it. A model no sheet matches has none.
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::accounts::Provider;
use crate::task_tags::TaskKind;

const SHEETS: &str = include_str!("model_sheets.toml");

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Cost {
    Low,
    Medium,
    High,
}

impl Cost {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelSheet {
    pub provider: Provider,
    #[serde(rename = "match")]
    pub prefix: String,
    pub cost: Cost,
    #[serde(default)]
    pub benchmark: Option<f64>,
    /// USD per million input, output, cache-read and cache-write tokens.
    pub usd_per_mtok: [f64; 4],
    pub good_for: Vec<TaskKind>,
    pub cautions: String,
    pub evidence: String,
}

impl ModelSheet {
    pub fn suits(&self, kind: TaskKind) -> bool {
        self.good_for.contains(&kind)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    sheet: Vec<ModelSheet>,
}

fn parse(text: &str) -> Result<Vec<ModelSheet>, String> {
    let file: File = toml::from_str(text).map_err(|error| error.to_string())?;
    if let Some(blank) = file.sheet.iter().find(|sheet| sheet.prefix.trim().is_empty() || sheet.good_for.is_empty()) {
        return Err(format!("a {} sheet has no match or no kinds", blank.provider.as_str()));
    }
    for sheet in &file.sheet {
        if sheet.usd_per_mtok.iter().any(|p| !p.is_finite() || *p <= 0.0)
            || sheet.benchmark.is_some_and(|b| !b.is_finite() || !(0.0..=100.0).contains(&b)) {
            return Err(format!("invalid price or benchmark for {}", sheet.prefix));
        }
    }
    Ok(file.sheet)
}

pub fn sheets() -> &'static [ModelSheet] {
    static PARSED: OnceLock<Vec<ModelSheet>> = OnceLock::new();
    PARSED.get_or_init(|| parse(SHEETS).expect("model_sheets.toml parses"))
}

/// The id a sheet's `match` is compared with: lowercase, without a context suffix (`[1m]`) and,
/// for Claude, without the `claude-` of a full id.
fn comparable(provider: Provider, model: &str) -> String {
    let base = model.split('[').next().unwrap_or(model).trim().to_ascii_lowercase();
    match provider {
        Provider::Claude => base.strip_prefix("claude-").map(str::to_owned).unwrap_or(base),
        Provider::Codex => base,
    }
}

pub fn sheet_for(provider: Provider, model: &str) -> Option<&'static ModelSheet> {
    let id = comparable(provider, model);
    sheets().iter().find(|sheet| sheet.provider == provider && id.starts_with(&sheet.prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_sheet_parses_with_its_evidence() {
        let all = sheets();
        assert!(all.len() >= 7);
        for sheet in all {
            assert!(!sheet.evidence.trim().is_empty() && !sheet.cautions.trim().is_empty(), "{}", sheet.prefix);
        }
        assert!(parse("[[sheet]]\nprovider = \"claude\"\nmatch = \"opus\"\ncost = \"cheap\"\ngood_for = [\"docs\"]\ncautions = \"\"\nevidence = \"\"").is_err());
        assert!(parse("[[sheet]]\nprovider = \"claude\"\nmatch = \"\"\ncost = \"low\"\ngood_for = [\"docs\"]\ncautions = \"\"\nevidence = \"\"").is_err());
    }

    #[test]
    fn a_model_takes_the_first_sheet_its_id_starts_with() {
        let prefix = |provider, model| sheet_for(provider, model).map(|sheet| sheet.prefix.as_str());
        assert_eq!(prefix(Provider::Claude, "opus"), Some("opus"));
        assert_eq!(prefix(Provider::Claude, "claude-opus-5-5"), Some("opus"));
        assert_eq!(prefix(Provider::Claude, "sonnet[1m]"), Some("sonnet"));
        assert_eq!(prefix(Provider::Claude, "Claude-Haiku-5"), Some("haiku"));
        assert_eq!(prefix(Provider::Codex, "gpt-6.1-sol"), Some("gpt-6.1-sol"));
        assert_eq!(prefix(Provider::Codex, "gpt-6-astra-mini"), Some("gpt-6-astra"));
        assert_eq!(prefix(Provider::Codex, "gpt-7"), Some("gpt"), "an unknown GPT gets the generic Codex sheet");
        assert!(sheet_for(Provider::Codex, "sonnet").is_none(), "a sheet belongs to its provider");
        assert!(sheet_for(Provider::Claude, "gpt-6.1-sol").is_none());
    }

    #[test]
    fn an_unknown_model_has_no_sheet() {
        assert!(sheet_for(Provider::Claude, "default").is_none());
        assert!(sheet_for(Provider::Codex, "o9-preview").is_none());
        assert!(sheet_for(Provider::Claude, "").is_none());
    }

    #[test]
    fn the_sheets_say_what_the_public_evidence_says() {
        let opus = sheet_for(Provider::Claude, "opus").unwrap();
        assert!(opus.evidence.contains("64.8%") && opus.cost == Cost::High && !opus.suits(TaskKind::Docs));
        let sonnet = sheet_for(Provider::Claude, "sonnet").unwrap();
        assert!(sonnet.evidence.contains("61.8%") && sonnet.cost < opus.cost && sonnet.suits(TaskKind::Docs));
        let sol = sheet_for(Provider::Codex, "gpt-6.1-sol").unwrap();
        assert!(sol.evidence.contains("$1.92/task") && sol.suits(TaskKind::Migration) && sol.suits(TaskKind::Tests));
    }
}
