pub const SUPPORTED_CLI_VERSION: &str = "0.163.0";
pub fn supported_cli_version(output: &str) -> bool { output.split_whitespace().any(|part| part == SUPPORTED_CLI_VERSION) }

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::model_selection::{AccountModelCatalog, CatalogModel};
use crate::usage::{Share, UsageReading, WindowKind, WindowReading};

pub fn command(executable: &Path, environment: &BTreeMap<String, String>, user_home: &Path, path: &str, username: &str, profile: Option<&Path>) -> Command {
    let mut command = Command::new(executable);
    if profile.is_some() { command.args(["-c", "cli_auth_credentials_store=\"file\""]); }
    command.env_clear().env("HOME", user_home).env("PATH", path).env("USER", username).env("LOGNAME", username).current_dir(user_home);
    for key in ["LANG", "LC_ALL", "TMPDIR", "SSL_CERT_FILE", "SSL_CERT_DIR", "HTTPS_PROXY", "HTTP_PROXY", "NO_PROXY"] {
        if let Some(value) = environment.get(key) { command.env(key, value); }
    }
    if let Some(profile) = profile.or_else(|| environment.get("CODEX_HOME").map(Path::new)) { command.env("CODEX_HOME", profile); }
    command
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProbeReport {
    pub requires_auth: bool,
    pub auth_method: Option<String>,
    pub account_type: Option<String>,
    pub workspace_id: Option<String>,
    pub signed_in: bool,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub pay_per_use: bool,
    pub rate_limits: Option<Value>,
    pub catalog: AccountModelCatalog,
    pub catalog_error: Option<String>,
}

impl ProbeReport {
    pub fn supported_authentication(&self) -> bool {
        matches!((self.auth_method.as_deref(), self.account_type.as_deref()), (Some("chatgpt"), Some("chatgpt")) | (Some("apikey"), Some("apiKey")))
    }

    pub fn native_identity(&self) -> Option<String> {
        self.workspace_id.clone()
    }

    pub fn ordinary_usage_allowed(&self) -> Option<bool> {
        self.rate_limits.as_ref()?.get("ordinaryUsageAllowed")?.as_bool()
    }
}

pub fn parse_models(list: &Value) -> Result<Vec<CatalogModel>, String> {
    list["data"].as_array().ok_or("Codex model list has no data array")?.iter().map(|model| {
        let name = model["model"].as_str().or_else(|| model["id"].as_str()).filter(|model| !model.is_empty()).ok_or("Codex returned a model without an ID")?;
        Ok(CatalogModel { model: name.into(), display_name: model["displayName"].as_str().unwrap_or(name).into(),
            reasoning_efforts: model["supportedReasoningEfforts"].as_array().into_iter().flatten().filter_map(|entry| entry["reasoningEffort"].as_str().map(str::to_owned)).collect(),
            default_reasoning_effort: model["defaultReasoningEffort"].as_str().map(str::to_owned),
            context_window: model["contextWindow"].as_u64(), input_modalities: model["inputModalities"].as_array().into_iter().flatten().filter_map(|entry| entry.as_str().map(str::to_owned)).collect(),
            is_default: model["isDefault"].as_bool().unwrap_or(false), hidden: model["hidden"].as_bool().unwrap_or(false) })
    }).collect()
}

pub fn parse_rate_limits(value: &Value, plan: Option<String>) -> Result<UsageReading, String> {
    let mut windows = vec![];
    let mut append = |scope: &str, limits: &Value| -> Result<(), String> {
        for dimension in ["primary", "secondary"] {
            let window = &limits[dimension];
            let Some(percent) = window["usedPercent"].as_f64().filter(|used| used.is_finite() && (0.0..=100.0).contains(used)) else { continue };
            let duration = window["windowDurationMins"].as_u64().map_or_else(|| "unknown".into(), |minutes| minutes.to_string());
            let kind = WindowKind::Other(format!("codex:{scope}:{dimension}:{duration}"));
            windows.push(WindowReading { kind, used: Share::of(percent / 100.0),
                resets_at: window["resetsAt"].as_i64().and_then(|seconds| seconds.checked_mul(1000)), exhausted: percent >= 100.0 });
        }
        Ok(())
    };
    if let Some(common) = value.get("rateLimits").filter(|limits| !limits.is_null()) { append("account", common)?; }
    if let Some(buckets) = value.get("rateLimitsByLimitId").and_then(Value::as_object) {
        for (id, limits) in buckets {
            if id.contains(':') || id.chars().any(char::is_control) { return Err("Codex returned an invalid rate-limit ID".into()); }
            append(&format!("limit:{id}"), limits)?;
        }
    }
    let reading = UsageReading { windows, plan, complete: false };
    reading.check()?;
    Ok(reading)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountKey, Provider};
    use serde_json::json;


    #[test]
    fn rate_limit_windows_keep_native_durations_and_partial_reads_preserve_other_dimensions() {
        let reading = parse_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":100,"windowDurationMins":90,"resetsAt":1000},"secondary":{"usedPercent":20,"windowDurationMins":20160,"resetsAt":2000}}}), Some("plus".into())).unwrap();
        assert_eq!(reading.windows[0].kind.duration_minutes(), Some(90));
        assert_eq!(reading.windows[1].kind.duration_minutes(), Some(20160));
        assert!(reading.windows.iter().all(|window| matches!(window.kind, WindowKind::Other(_))));
        let key = AccountKey::native(Provider::Codex);
        let measured = crate::usage::record(None, key.provider, &key.account, &reading, 1, "2026-10-25");
        assert_eq!(measured.plan_exhausted_until(2), Some(1_000_000));
        let partial = parse_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":5,"windowDurationMins":60,"resetsAt":3000}}}), None).unwrap();
        let latest = crate::usage::record(Some(&measured), key.provider, &key.account, &partial, 2, "2026-10-25");
        assert_eq!(latest.windows.len(), 2);
        assert!(latest.windows.iter().any(|window| window.reading.kind.duration_minutes() == Some(60)));
        assert!(!latest.windows.iter().any(|window| window.reading.kind.duration_minutes() == Some(90)));
        assert!(latest.plan_exhausted_until(3).is_none());
        assert!(parse_rate_limits(&json!({}), None).unwrap().windows.is_empty());
    }

    #[test]
    fn codex_commands_do_not_inherit_an_api_key_or_claude_profile() {
        let env = BTreeMap::from([("OPENAI_API_KEY".into(), "must-not-inherit".into()), ("CLAUDE_CONFIG_DIR".into(), "/wrong".into()), ("CODEX_HOME".into(), "/native-codex".into())]);
        let command = command(Path::new("/fake/codex"), &env, Path::new("/user"), "/bin", "test", None);
        let env: BTreeMap<_, _> = command.get_envs().map(|(key,value)| (key.to_str().unwrap(), value.map(|value| value.to_str().unwrap()))).collect();
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("CLAUDE_CONFIG_DIR"));
        assert_eq!(env["CODEX_HOME"], Some("/native-codex"));
    }

}

#[cfg(test)]
#[test]
fn supported_codex_release_requires_an_exact_version_token() {
    assert!(supported_cli_version("codex-cli 0.163.0\n"));
    for output in ["codex-cli 0.163.01","codex-cli 0.163.0-dev","codex-cli 0.162.0",""] { assert!(!supported_cli_version(output),"{output}"); }
}
