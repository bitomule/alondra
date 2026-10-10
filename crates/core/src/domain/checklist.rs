//! A Task's scope: the points of the plan the user approved, written as Markdown checklist lines
//! (`- [ ] …`). Its final delivery marks every point done with what proves it.
use super::InteractionId;
use serde::{Deserialize, Serialize};

pub const CHECKLIST_MAX_ITEMS: usize = 40;
pub const CHECKLIST_MAX_TEXT: usize = 300;
pub const EVIDENCE_MAX_PER_ITEM: usize = 5;
pub const EVIDENCE_MAX_TEXT: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checklist {
    /// The plan card the user approved.
    pub plan: InteractionId,
    pub items: Vec<ChecklistItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecklistItem {
    pub id: String,
    pub text: String,
}

/// A point of the approved plan the final delivery says is done, and what proves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChecklistMark {
    pub item: String,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Commit,
    Test,
    Command,
    Screenshot,
    Pr,
    File,
    Link,
}

impl EvidenceKind {
    pub const ALL: [EvidenceKind; 7] = [Self::Commit, Self::Test, Self::Command, Self::Screenshot, Self::Pr, Self::File, Self::Link];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Test => "test",
            Self::Command => "command",
            Self::Screenshot => "screenshot",
            Self::Pr => "pr",
            Self::File => "file",
            Self::Link => "link",
        }
    }

    /// "commit, test, command, …": what a node may cite, for the texts that teach it.
    pub fn listed() -> String {
        Self::ALL.map(Self::as_str).join(", ")
    }
}

/// Why a plan cannot be approved as a task's scope, as a refusal code and how to fix it; `None`
/// when its checklist is fine.
pub fn plan_checklist_problem(plan: &str) -> Option<(&'static str, String)> {
    let items = checklist_items(plan);
    if items.is_empty() {
        return Some(("plan_without_checklist", "your plan has no checklist. Write its scope as Markdown checklist lines (`- [ ] …`), one per thing you will deliver: your final result must mark each one done with its evidence".into()));
    }
    if items.len() > CHECKLIST_MAX_ITEMS || items.iter().any(|item| item.text.chars().count() > CHECKLIST_MAX_TEXT) {
        return Some(("plan_checklist_too_long", format!("your plan's checklist holds at most {CHECKLIST_MAX_ITEMS} points of at most {CHECKLIST_MAX_TEXT} characters each: group the points into the things you will deliver")));
    }
    None
}

/// Whether `value` reads as a commit sha, abbreviated or full.
pub fn is_commit_sha(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// The checklist lines of a plan, numbered from 1 in the order they appear. Lines inside code
/// blocks and items with no text are not points.
pub fn checklist_items(markdown: &str) -> Vec<ChecklistItem> {
    let mut fence: Option<&str> = None;
    let mut items = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if let Some(marker) = ["```", "~~~"].into_iter().find(|marker| trimmed.starts_with(marker)) {
            fence = Some(marker);
            continue;
        }
        if let Some(text) = task_text(trimmed) {
            items.push(ChecklistItem { id: (items.len() + 1).to_string(), text: text.to_owned() });
        }
    }
    items
}

fn task_text(line: &str) -> Option<&str> {
    let rest = match line.as_bytes().first()? {
        b'-' | b'*' | b'+' => &line[1..],
        b'0'..=b'9' => {
            let digits = line.bytes().take_while(u8::is_ascii_digit).count();
            line[digits..].strip_prefix(['.', ')'])?
        }
        _ => return None,
    };
    let rest = rest.strip_prefix(' ')?.trim_start();
    let rest = ["[ ] ", "[x] ", "[X] "].into_iter().find_map(|box_| rest.strip_prefix(box_))?;
    Some(rest.trim()).filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(markdown: &str) -> Vec<String> {
        checklist_items(markdown).into_iter().map(|item| item.text).collect()
    }

    #[test]
    fn every_kind_of_checklist_line_is_a_point_numbered_in_order() {
        let items = checklist_items("# Plan\n\n- [ ] Parse it\n* [x] Store it\n+ [X] Show it\n1. [ ] Test it\n2) [ ] Ship it\n  - [ ] nested one\n");
        let ids: Vec<_> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, ["1", "2", "3", "4", "5", "6"]);
        assert_eq!(texts("- [ ] **Bold** first  \n"), ["**Bold** first"]);
        assert_eq!(items[5].text, "nested one");
    }

    #[test]
    fn plain_bullets_broken_boxes_and_empty_items_are_not_points() {
        assert!(texts("- plain bullet\n- []  empty box\n-[ ] no space\n- [ ]no space after\n- [ ]   \n- [-] other\nText - [ ] inline\n").is_empty());
    }

    #[test]
    fn lines_inside_code_blocks_are_not_points() {
        let markdown = "- [ ] real\n```md\n- [ ] example\n```\n~~~\n- [ ] tilde example\n~~~\n- [ ] after";
        assert_eq!(texts(markdown), ["real", "after"]);
    }

    #[test]
    fn a_commit_is_seven_to_forty_hex_digits() {
        assert!(is_commit_sha("3f2a1c9"));
        assert!(is_commit_sha(&"a".repeat(40)));
        assert!(!is_commit_sha("3f2a1c"));
        assert!(!is_commit_sha(&"a".repeat(41)));
        assert!(!is_commit_sha("main"));
    }
}
