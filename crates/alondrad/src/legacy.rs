use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alondra_core::{Event, Question, QuestionOrigin, QuestionStatus, View, now_millis};
use serde::Deserialize;

use crate::hub::Hub;

const POLL: Duration = Duration::from_secs(5);
const TITLE_MAX: usize = 110;

#[derive(Deserialize)]
struct Store {
    #[serde(default)]
    open: Vec<Entry>,
}

#[derive(Deserialize, Clone)]
struct Entry {
    n: u64,
    q: String,
    #[serde(default)]
    node: String,
}

struct Paths {
    store: PathBuf,
    script: PathBuf,
    orchestrator_job: PathBuf,
    imported: PathBuf,
}

pub async fn run(hub: Arc<Hub>, claude_home: PathBuf) {
    let hive = std::env::var("CLAUDE_HIVE_DIR").map(PathBuf::from).unwrap_or_else(|_| claude_home.join("hive"));
    let paths = Paths {
        store: hive.join("open-questions.json"),
        script: claude_home.join("skills/orchestrator/scripts/questions.sh"),
        orchestrator_job: hive.join("orchestrator-job"),
        imported: hub.home.join("legacy-questions.json"),
    };
    if !paths.store.exists() {
        return;
    }
    let mut imported: BTreeMap<u64, u64> =
        std::fs::read_to_string(&paths.imported).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    loop {
        if let Err(e) = sync(&hub, &paths, &mut imported).await {
            log!("bridge to the old questions: {e}");
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn sync(hub: &Hub, paths: &Paths, imported: &mut BTreeMap<u64, u64>) -> anyhow::Result<()> {
    let Ok(text) = std::fs::read_to_string(&paths.store) else { return Ok(()) };
    let store: Store = serde_json::from_str(&text)?;
    let orchestrator_short = std::fs::read_to_string(&paths.orchestrator_job).ok().map(|s| s.trim().to_string());
    let mut changed = false;

    for entry in &store.open {
        if imported.contains_key(&entry.n) {
            continue;
        }
        let res = hub.append_with(|v| {
            let session_id = resolve(v, &entry.node, orchestrator_short.as_deref())?;
            let node_name = v.nodes.get(&session_id).and_then(|n| n.name.clone());
            let task = v.fact_task(&session_id, None);
            Some(Event::QuestionAsked(Question {
                task,
                replaces: None, about_pr: None,
                id: v.questions.keys().next_back().copied().unwrap_or(0) + 1,
                session_id,
                node_name,
                title: title_of(&entry.q),
                detail: Some(detail_of(&entry.q)),
                options: vec![],
                links: vec![],
                attachments: vec![],
                blocking: true,
                origin: QuestionOrigin::Ask,
                tool_use_id: None,
                to_orchestrator: false,
                node_context: Some(format!("Question #{} from the previous system", entry.n)),
                status: QuestionStatus::Open,
                answer: None,
                asked_at: now_millis(),
                action: false,
                steps: vec![],
                mission: None,
            }))
        })?;
        if let Some((_, Event::QuestionAsked(q))) = res {
            imported.insert(entry.n, q.id);
            changed = true;
        }
    }

    let still_open: std::collections::HashSet<u64> = store.open.iter().map(|e| e.n).collect();
    let mut finished = vec![];
    for (&n, &qid) in imported.iter() {
        let status = hub.read(|v| v.questions.get(&qid).map(|q| (q.status, q.answer.as_ref().map(|a| a.text.clone()))));
        match status {
            Some((s, _)) if s.is_open() && !still_open.contains(&n) => {
                hub.append(Event::QuestionClosed { id: qid, status: QuestionStatus::Withdrawn, note: None })?;
                finished.push(n);
            }
            Some((s, answer)) if !s.is_open() && still_open.contains(&n) => {
                close_in_old_store(&paths.script, n, answer.as_deref()).await;
                finished.push(n);
            }
            None => finished.push(n),
            _ if !still_open.contains(&n) => finished.push(n),
            _ => {}
        }
    }
    for n in finished {
        if !still_open.contains(&n) {
            imported.remove(&n);
            changed = true;
        }
    }
    if changed {
        save(&paths.imported, imported)?;
    }
    Ok(())
}

fn resolve(v: &View, node: &str, orchestrator_short: Option<&str>) -> Option<String> {
    let node = node.trim();
    let by = |key: &str| {
        v.nodes
            .values()
            .find(|n| n.short_id == key || n.session_id == key || n.name.as_deref() == Some(key))
            .map(|n| n.session_id.clone())
    };
    if !node.is_empty() {
        return by(node).or_else(|| orchestrator_short.and_then(by));
    }
    v.nodes
        .values()
        .find(|n| n.is_orchestrator && n.in_daemon)
        .map(|n| n.session_id.clone())
        .or_else(|| orchestrator_short.and_then(by))
}

async fn close_in_old_store(script: &Path, n: u64, answer: Option<&str>) {
    let n = n.to_string();
    let args: Vec<Vec<String>> = match answer {
        Some(text) => vec![
            vec!["answer".into(), n.clone(), format!("{text} (answered in Alondra)")],
            vec!["delivered".into(), n],
        ],
        None => vec![vec!["close".into(), n]],
    };
    for a in args {
        let out = tokio::process::Command::new("/bin/sh").arg(script).args(&a).output().await;
        if let Ok(o) = out
            && !o.status.success()
        {
            log!("questions.sh {}: {}", a.join(" "), String::from_utf8_lossy(&o.stderr).trim());
        }
    }
}

fn save(path: &Path, imported: &BTreeMap<u64, u64>) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(imported)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn detail_of(q: &str) -> String {
    match q.split_once(". ") {
        Some((first, rest)) if first.chars().count() <= TITLE_MAX && !rest.trim().is_empty() => rest.trim().to_string(),
        _ => q.to_string(),
    }
}

fn title_of(q: &str) -> String {
    let first = q.split(". ").next().unwrap_or(q).trim();
    if first.chars().count() <= TITLE_MAX {
        return first.to_string();
    }
    let cut: String = first.chars().take(TITLE_MAX).collect();
    let cut = cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut);
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_the_first_sentence_capped_on_a_word() {
        assert_eq!(title_of("Lumo: the fix is in PR #150. With tests."), "Lumo: the fix is in PR #150");
        assert_eq!(detail_of("Lumo: the fix is in PR #150. With tests."), "With tests.");
        let long = format!("{} fin. Resto.", "palabra ".repeat(40));
        assert_eq!(detail_of(&long), long);
        let long = "palabra ".repeat(40);
        let t = title_of(&long);
        assert!(t.ends_with('…') && t.chars().count() <= TITLE_MAX + 1 && !t.contains("  "));
    }
}
