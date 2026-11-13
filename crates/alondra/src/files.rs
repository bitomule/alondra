//! `alondra files sweep`: who owns each file Alondra keeps, and what the retention rule would
//! remove today. It only lists: a person approves the list before anything goes.
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::client::Client;

pub fn sweep(json: bool) -> Result<()> {
    let report: Value = Client::new(Duration::from_secs(60)).get("/files-sweep")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&crate::version::with_warning(report))?);
    } else {
        print!("{}", render(&report));
    }
    Ok(())
}

fn size(bytes: u64) -> String {
    let lang = match crate::i18n::lang() {
        crate::i18n::Lang::En => alondra_core::views::Lang::En,
        crate::i18n::Lang::Es => alondra_core::views::Lang::Es,
    };
    alondra_core::machine_incident::bytes_text(bytes, lang)
}

pub(crate) fn render(report: &Value) -> String {
    let entries = report["entries"].as_array().cloned().unwrap_or_default();
    let (remove, keep): (Vec<&Value>, Vec<&Value>) = entries.iter().partition(|entry| entry["remove"].as_bool().unwrap_or(false));
    let total = |rows: &[&Value]| rows.iter().map(|row| row["bytes"].as_u64().unwrap_or(0)).sum::<u64>();
    let mut out = vec![];
    if remove.is_empty() {
        out.push(tr!("Nothing to remove.", "Nada que quitar.").to_owned());
    } else {
        let (n, bytes) = (remove.len(), size(total(&remove)));
        out.push(trf!("The rule would remove {n} entries, {bytes}; nothing was removed:", "La regla quitaría {n} entradas, {bytes}; no se ha quitado nada:"));
        for row in &remove {
            let (path, owner, reason, bytes) = (row["path"].as_str().unwrap_or_default(), row["owner"].as_str().unwrap_or_default(), row["reason"].as_str().unwrap_or_default(), size(row["bytes"].as_u64().unwrap_or(0)));
            out.push(format!("  {path}  {bytes}  [{owner}] {reason}"));
        }
    }
    let (n, bytes) = (keep.len(), size(total(&keep)));
    out.push(trf!("It keeps {n} entries, {bytes}.", "Conserva {n} entradas, {bytes}."));
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_list_says_what_goes_why_and_that_nothing_was_removed() {
        let report = json!({"entries": [
            {"path": "security-audit", "bytes": 3_000_000, "owner": "nobody", "remove": true, "reason": "no task, card or upload owns it, and it is older than 30 days"},
            {"path": "results/t-open", "bytes": 1_000, "owner": "task t-open", "remove": false, "reason": "its task is open"}]});
        let text = render(&report);
        assert!(text.contains("security-audit"), "{text}");
        assert!(text.contains("nobody"), "{text}");
        assert!(!text.contains("results/t-open"), "what stays is only counted: {text}");
        assert!(text.contains("1 entries") || text.contains("1 entradas"), "{text}");
    }
}
