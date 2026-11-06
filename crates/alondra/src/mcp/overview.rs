use anyhow::{Result, ensure};
use serde_json::{Value, json};

const SECTIONS: [&str; 6] = [
    "tasks",
    "requests",
    "nodes",
    "open_questions",
    "unread_reports",
    "orchestrator_messages",
];

fn rows<'a>(o: &'a Value, key: &str) -> Vec<&'a Value> {
    o[key]
        .as_array()
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn id(row: &Value) -> String {
    let value = row
        .get("id")
        .or_else(|| row.get("session_id"))
        .or_else(|| row.get("short_id"));
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn repo(o: &Value, row: &Value) -> Value {
    row.get("repo")
        .filter(|v| !v.is_null())
        .or_else(|| row.get("project").filter(|v| !v.is_null()))
        .cloned()
        .unwrap_or_else(|| {
            let session = row
                .pointer("/assignee/session_id")
                .or_else(|| row.get("session_id"))
                .and_then(Value::as_str);
            rows(o, "nodes")
                .into_iter()
                .find(|n| session.is_some() && n["session_id"].as_str() == session)
                .map(|n| n["project"].clone())
                .unwrap_or(Value::Null)
        })
}

fn summary(o: &Value, row: &Value, section: &str) -> Value {
    let keys: &[&str] = match section {
        "tasks" => &["id", "title", "state", "activity", "request"],
        "requests" => &["id", "title", "state", "text"],
        "nodes" => &[
            "session_id",
            "name",
            "short_id",
            "state",
            "alive",
            "in_daemon",
        ],
        _ => &[
            "id",
            "title",
            "kind",
            "node_name",
            "session_id",
            "text",
            "asked_at",
            "action",
            "to_orchestrator",
        ],
    };
    let mut result = json!({"id": id(row)});
    let mut truncated = false;
    for key in keys {
        if let Some(value) = row.get(*key) {
            if *key == "id" {
                continue;
            }
            let text = match value {
                Value::String(s) => Some(s.clone()),
                Value::Object(_) | Value::Array(_) => Some(value.to_string()),
                _ => None,
            };
            result[*key] = match text {
                Some(s) if s.chars().count() > 160 => {
                    truncated = true;
                    json!(s.chars().take(160).collect::<String>())
                }
                _ => value.clone(),
            };
        }
    }
    result["repo"] = repo(o, row);
    if truncated {
        result["truncated"] = json!(true);
    }
    result
}

fn page(o: &Value, args: &Value, section: &str, budget: usize) -> Result<Value> {
    let all = args["all"].as_bool().unwrap_or(false);
    let limit = args["limit"].as_u64().unwrap_or(20) as usize;
    ensure!((1..=50).contains(&limit), "limit must be between 1 and 50");
    let source = if section == "unread_reports" {
        "reports"
    } else {
        section
    };
    let mut selected: Vec<_> = rows(o, source)
        .into_iter()
        .filter(|row| {
            if section == "requests" && row["synthetic"] == true {
                return false;
            }
            if section == "unread_reports" && row["read"] != false {
                return false;
            }
            if !all {
                if matches!(section, "tasks" | "requests" | "nodes") && row["state"] == "done" {
                    return false;
                }
                if section == "tasks" && row["state"] == "cancelled" {
                    return false;
                }
                if section == "nodes" && row["alive"] != true && row["in_daemon"] != true {
                    let owes = rows(o, "open_questions")
                        .into_iter()
                        .chain(
                            rows(o, "reports")
                                .into_iter()
                                .filter(|r| r["read"] == false),
                        )
                        .any(|r| {
                            r["session_id"] == row["session_id"] && !row["session_id"].is_null()
                        });
                    if !owes {
                        return false;
                    }
                }
                if section == "orchestrator_messages" && !row["handed_at"].is_null() {
                    return false;
                }
            }
            if let Some(state) = args["state"].as_str() {
                if row["state"] != state {
                    return false;
                }
            }
            if let Some(wanted) = args["repo"].as_str() {
                let matches = if section == "requests" {
                    rows(o, "tasks")
                        .into_iter()
                        .any(|task| task["request"] == row["id"] && repo(o, task) == wanted)
                } else {
                    repo(o, row) == wanted
                };
                if !matches {
                    return false;
                }
            }
            if let Some(task) = args["task_id"].as_str() {
                if section == "tasks" {
                    return id(row) == task;
                }
                return row["task_id"] == task;
            }
            true
        })
        .collect();
    selected.sort_by_key(|row| id(row));
    ensure!(
        selected.iter().all(|r| !id(r).is_empty()),
        "section contains a row without an identifier"
    );
    let total = selected.len();
    if let Some(after) = args["after"].as_str() {
        selected.retain(|r| id(r).as_str() > after);
    }
    let mut items = Vec::new();
    let mut last = None;
    let available = selected.len();
    for row in selected.into_iter().take(limit) {
        let item = summary(o, row, section);
        items.push(item);
        if serde_json::to_vec(&items)?.len() > budget {
            items.pop();
            break;
        }
        last = Some(id(row));
    }
    ensure!(
        available == 0 || !items.is_empty(),
        "one row exceeds the page budget; query a narrower section"
    );
    let next = if items.len() < available {
        let mut next = args.clone();
        next["section"] = json!(section);
        next["after"] = json!(last);
        next
    } else {
        Value::Null
    };
    Ok(
        json!({"items": items, "total": total, "next": next, "detail": "Read full detail with section and item_id; task_id also reads task detail. Text previews are at most 160 characters."}),
    )
}

pub(super) fn overview(o: &Value, args: &Value) -> Result<Value> {
    if let Some(section) = args["section"].as_str() {
        ensure!(SECTIONS.contains(&section), "unknown section");
        if let Some(wanted) = args["item_id"].as_str() {
            let source = if section == "unread_reports" {
                "reports"
            } else {
                section
            };
            return rows(o, source)
                .into_iter()
                .find(|r| id(r) == wanted)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("not_found: item not found"));
        }
        if section == "tasks" {
            if let Some(task) = args["task_id"].as_str() {
                return rows(o, "tasks")
                    .into_iter()
                    .find(|r| id(r) == task)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("task not found"));
            }
        }
        return page(o, args, section, 10_000);
    }
    ensure!(args["after"].is_null(), "after requires section");
    let mut output = json!({});
    for section in SECTIONS {
        output[section] = page(o, args, section, 2_000)?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hundreds_of_tasks_are_bounded_and_paginated_without_loss() {
        let tasks: Vec<_> = (0..500).map(|n| json!({"id":format!("t-{n:04}"),"title":"🦀\"\\".repeat(200),"state":"working","repo":"example/repo"})).collect();
        let o = json!({"tasks":tasks,"requests":[{"id":"unassigned","state":"defining","text":"Needs definition"}]});
        for all in [false, true] {
            let first = overview(&o, &json!({"all":all})).unwrap();
            assert!(serde_json::to_vec(&first).unwrap().len() < 24 * 1024);
            assert_eq!(first["requests"]["items"][0]["id"], "unassigned");
            let mut args = json!({"section":"tasks","all":all});
            let mut ids = Vec::new();
            loop {
                let result = overview(&o, &args).unwrap();
                assert!(serde_json::to_vec(&result).unwrap().len() < 24 * 1024);
                for row in result["items"].as_array().unwrap() {
                    ids.push(row["id"].as_str().unwrap().to_owned());
                    assert_eq!(row["truncated"], true);
                }
                if result["next"].is_null() {
                    break;
                }
                args = result["next"].clone();
            }
            assert_eq!(ids.len(), 500);
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), 500);
        }
        assert_eq!(
            overview(&o, &json!({"section":"tasks","repo":"other"})).unwrap()["total"],
            0
        );
        assert!(overview(&o, &json!({"limit":51})).is_err());
        assert!(overview(&o, &json!({"section":"missing"})).is_err());
        assert_eq!(
            overview(&o, &json!({"section":"tasks","task_id":"t-0001"})).unwrap()["title"],
            o["tasks"][1]["title"]
        );
    }

    #[test]
    fn every_growing_section_has_a_continuation_and_recoverable_detail() {
        let records: Vec<_> = (0..500)
            .map(|n| {
                json!({"id":format!("item-{n:04}"),
            "session_id":format!("session-{n:04}"),"title":"\\\"🙂".repeat(300),
            "text":"complete text".repeat(1000),"state":"running","alive":true,"read":false})
            })
            .collect();
        let raw = json!({"tasks":records,"requests":records,"nodes":records,
            "open_questions":records,"reports":records,"orchestrator_messages":records});
        for all in [false, true] {
            let overview = overview(&raw, &json!({"all":all})).unwrap();
            let reply = super::super::output::reply(
                "tasks_in_progress",
                &json!({"all":all}),
                overview.to_string(),
            )
            .unwrap();
            assert!(super::super::output::fits(&reply));
            for section in SECTIONS {
                assert_eq!(overview[section]["total"], 500);
                assert!(overview[section]["next"].is_object());
                let detail =
                    super::overview(&raw, &json!({"section":section,"item_id":"item-0000"}))
                        .unwrap();
                assert_eq!(detail["text"], records[0]["text"]);
            }
        }
    }

    #[test]
    fn filters_finished_rows_and_keeps_requests_without_tasks() {
        let o = json!({"tasks":[{"id":"a","state":"done"},{"id":"b","state":"working","assignee":{"session_id":"s"}},{"id":"c","state":"cancelled"}],
            "nodes":[{"session_id":"s","project":"sample","alive":true},{"session_id":"gone"}],
            "requests":[{"id":"open","state":"defining"},{"id":"old","state":"done"}],
            "reports":[{"id":1,"read":false},{"id":2,"read":true}]});
        let v = overview(&o, &json!({})).unwrap();
        assert_eq!(v["tasks"]["total"], 1);
        assert_eq!(v["requests"]["items"][0]["id"], "open");
        assert_eq!(v["nodes"]["total"], 1);
        assert_eq!(v["unread_reports"]["total"], 1);
        assert_eq!(
            overview(
                &o,
                &json!({"section":"tasks","repo":"sample","state":"working"})
            )
            .unwrap()["total"],
            1
        );
        assert_eq!(
            overview(&o, &json!({"all":true})).unwrap()["tasks"]["total"],
            3
        );
        assert_eq!(
            overview(&o, &json!({"all":true})).unwrap()["nodes"]["total"],
            2
        );
    }
}
