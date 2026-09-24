//! Menus Alondra cannot see: a node's terminal parked on a permission, a question or a dialog
//! that never went through a hook Alondra holds (a subagent's worktree relocation, a menu left
//! open after the user stopped watching, nodes without the plugin). The Claude daemon says so in
//! `claude agents --json` (`waitingFor`); the menu itself is read from the screen and becomes a
//! card whose options are typed back as the menu's number, or with the arrows and Enter on a
//! dialog whose options sit side by side (an MCP server's Accept / Decline).

use std::sync::Arc;
use std::time::{Duration, Instant};

use alondra_core::{Event, Node, Question, QuestionOption, QuestionOrigin, QuestionStatus, View, now_millis, states};
use serde_json::Value;

use crate::hub::Hub;

const EVERY: Duration = Duration::from_secs(30);
/// `claude agents --json` costs ~160 MB for a moment, so it is asked only when a node looks
/// parked, and at most this often.
const AGENTS_EVERY: Duration = Duration::from_secs(60);
pub const MARK: &str = "menu:";

#[derive(Debug, PartialEq)]
pub struct Menu {
    pub title: String,
    pub detail: String,
    pub options: Vec<String>,
    /// On a row of options, the one under `❯`; numbered menus are picked by number instead.
    pub selected: Option<usize>,
}

impl Menu {
    /// The same dialog, wherever its cursor is.
    pub fn same_as(&self, other: &Menu) -> bool {
        (&self.title, &self.detail, &self.options) == (&other.title, &other.detail, &other.options)
    }
}

pub async fn run(hub: Arc<Hub>) {
    let mut asked_at: Option<Instant> = None;
    loop {
        tokio::time::sleep(EVERY).await;
        if let Err(e) = sweep(&hub, &mut asked_at).await {
            log!("menus: {e}");
        }
    }
}

async fn sweep(hub: &Arc<Hub>, asked_at: &mut Option<Instant>) -> anyhow::Result<()> {
    let now = now_millis();
    let suspects: Vec<(String, String, Option<String>)> =
        hub.read(|v| v.nodes.values().filter(|n| suspect(v, n, now)).map(|n| (n.session_id.clone(), n.short_id.clone(), n.menu_waiting.clone())).collect());
    if !suspects.is_empty() && asked_at.is_none_or(|t| t.elapsed() >= AGENTS_EVERY) {
        *asked_at = Some(Instant::now());
        if let Some(waiting) = agents_waiting().await {
            for (session, short, before) in &suspects {
                let now_waiting = waiting.iter().find(|(s, _)| s == short).map(|(_, w)| w.clone());
                if &now_waiting != before {
                    hub.append(Event::MenuSeen { session_id: session.clone(), waiting_for: now_waiting })?;
                }
            }
        }
    }

    let stale: Vec<u64> = hub.read(|v| {
        v.questions
            .values()
            .filter(|q| q.status.is_open() && is_menu_card(q))
            .filter(|q| v.nodes.get(&q.session_id).is_none_or(|n| !n.waiting_permission && n.menu_waiting.is_none()))
            .map(|q| q.id)
            .collect()
    });
    for id in stale {
        hub.append(Event::QuestionClosed { id, status: QuestionStatus::AnsweredInTerminal, note: None })?;
    }

    let parked: Vec<(String, String)> =
        hub.read(|v| v.nodes.values().filter(|n| states::unseen_menu(v, n, now_millis())).map(|n| (n.session_id.clone(), n.short_id.clone())).collect());
    for (session, short) in parked {
        let own = hub.own_attach(&short);
        let short_for_task = short.clone();
        let screen = tokio::task::spawn_blocking(move || crate::wake::read_screen(&short_for_task)).await?;
        drop(own);
        let screen = match screen {
            Ok(s) => s,
            Err(e) => {
                log!("menus: could not read {short}'s screen: {e}");
                continue;
            }
        };
        hub.append_with(|v| {
            let node = v.nodes.get(&session)?;
            states::unseen_menu(v, node, now_millis()).then(|| Event::QuestionAsked(card(v, node, &screen)))
        })?;
    }
    Ok(())
}

/// Nodes worth asking the Claude daemon about: in the background, nobody at their terminal, no
/// card open, and not visibly working. The daemon's own `state.json` can still say "working"
/// while its terminal waits on a menu, so it is not trusted for this.
fn suspect(v: &View, n: &Node, now: i64) -> bool {
    let carded = v.questions.values().any(|q| q.session_id == n.session_id && q.status.is_open());
    let idle = n.state != alondra_core::NodeState::Working || n.daemon_state.as_deref() == Some("blocked");
    n.in_daemon && n.alive && idle && !carded && !states::watching(v, n, now)
}

async fn agents_waiting() -> Option<Vec<(String, String)>> {
    let out = tokio::time::timeout(Duration::from_secs(20), tokio::process::Command::new("claude").args(["agents", "--json"]).kill_on_drop(true).output())
        .await
        .ok()?
        .ok()?;
    Some(waiting_from(&String::from_utf8_lossy(&out.stdout)))
}

/// (short id, what it waits on) for every session whose terminal waits on a menu.
fn waiting_from(json: &str) -> Vec<(String, String)> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(json) else { return vec![] };
    list.iter()
        .filter_map(|a| {
            let id = a.get("id")?.as_str()?;
            let waiting = a.get("waitingFor")?.as_str()?;
            (a.get("status").and_then(Value::as_str) == Some("waiting")).then(|| (id.to_string(), waiting.to_string()))
        })
        .collect()
}

pub fn is_menu_card(q: &Question) -> bool {
    q.tool_use_id.as_deref().is_some_and(|t| t.starts_with(MARK))
}

fn card(v: &View, node: &Node, screen: &str) -> Question {
    let name = node.display_name().to_string();
    let (title, detail, options) = match parse_menu(screen) {
        Some(m) => (m.title, Some(m.detail).filter(|d| !d.is_empty()), m.options),
        None => (
            format!("{name} espera en un menú que Alondra no sabe leer: ábrelo en su terminal"),
            Some(format!("```\n{}\n```", last_lines(screen, 20))),
            vec![],
        ),
    };
    Question {
        id: v.questions.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: node.session_id.clone(),
        node_name: Some(name),
        title,
        detail,
        options: options.into_iter().map(|label| QuestionOption { label, description: None }).collect(),
        links: vec![],
        attachments: vec![],
        blocking: true,
        origin: QuestionOrigin::Permission,
        tool_use_id: Some(format!("{MARK}{}", node.short_id)),
        to_orchestrator: false,
        node_context: None,
        status: QuestionStatus::Open,
        answer: None,
        asked_at: now_millis(),
        action: false,
        steps: vec![],
        mission: node.mission.clone(),
    }
}

fn last_lines(screen: &str, n: usize) -> String {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn option_line(line: &str) -> Option<(u32, String)> {
    let t = line.trim().trim_start_matches('❯').trim_start();
    let (num, rest) = t.split_once(". ")?;
    let n: u32 = num.parse().ok()?;
    Some((n, rest.trim().to_string()))
}

/// Options side by side, `❯` on the selected one: `❯ Accept    Decline` or `Accept / ❯ Decline`.
fn option_row(line: &str) -> Option<(Vec<String>, usize)> {
    let t = line.trim().replace(" / ", "   ");
    if !t.contains('❯') {
        return None;
    }
    let cells: Vec<&str> = t.split("  ").map(str::trim).filter(|c| !c.is_empty()).collect();
    let selected = cells.iter().position(|c| c.starts_with('❯'))?;
    let labels: Vec<String> = cells.iter().map(|c| c.trim_start_matches('❯').trim().to_string()).collect();
    let short = labels.iter().all(|l| !l.is_empty() && l.chars().count() <= 30 && !l.contains('❯') && option_line(l).is_none());
    let one_cursor = cells.iter().filter(|c| c.contains('❯')).count() == 1;
    ((2..=4).contains(&labels.len()) && short && one_cursor).then_some((labels, selected))
}

/// A dialog whose options sit on one row under what it asks, with the key hint below.
fn parse_row(block: &[&str]) -> Option<Menu> {
    let at = block.iter().rposition(|l| option_row(l).is_some())?;
    let (options, selected) = option_row(block[at])?;
    if at == 0 || !block[at + 1..].iter().any(|l| l.contains("Esc")) {
        return None;
    }
    let about: Vec<String> = block[..at].iter().map(|l| l.trim().to_string()).collect();
    let detail = if about.len() > 1 { format!("```\n{}\n```", about[1..].join("\n")) } else { String::new() };
    Some(Menu { title: about[0].clone(), detail, options, selected: Some(selected) })
}

/// Claude Code draws a menu below a rule: a header, what it is about, the question, then
/// numbered options with `❯` on the selected one and a key hint underneath. A dialog with its
/// options on one row (an MCP server's Accept / Decline) is read too.
pub fn parse_menu(screen: &str) -> Option<Menu> {
    let lines: Vec<&str> = screen.lines().collect();
    let rule = lines.iter().rposition(|l| l.trim().chars().count() > 20 && l.trim().chars().all(|c| c == '─'))?;
    let block: Vec<&str> = lines[rule + 1..].iter().copied().filter(|l| !l.trim().is_empty()).collect();
    let Some(first) = block.iter().position(|l| option_line(l).is_some()) else {
        return parse_row(&block);
    };
    let options: Vec<(u32, String)> = block[first..].iter().map_while(|l| option_line(l)).collect();
    let contiguous = options.iter().enumerate().all(|(i, (n, _))| *n as usize == i + 1);
    if options.len() < 2 || !contiguous {
        return None;
    }
    let question = block[..first].last()?.trim().to_string();
    let about: Vec<String> = block[..first.saturating_sub(1)].iter().map(|l| l.trim().to_string()).collect();
    let title = match about.first() {
        Some(head) => format!("{head}: {question}"),
        None => question.clone(),
    };
    let detail = if about.len() > 1 { format!("```\n{}\n```", about[1..].join("\n")) } else { String::new() };
    Some(Menu { title, detail, options: options.into_iter().map(|(_, l)| l).collect(), selected: None })
}

/// The tmux keys that pick `label` in the menu on screen: its number on a numbered menu, or the
/// arrows from the selected option to it and Enter on a row.
pub fn key_for(menu: &Menu, label: &str) -> Option<Vec<String>> {
    let target = menu.options.iter().position(|o| o == label)?;
    let Some(selected) = menu.selected else {
        return Some(vec![(target + 1).to_string()]);
    };
    let arrow = if target > selected { "Right" } else { "Left" };
    let mut keys = vec![arrow.to_string(); target.abs_diff(selected)];
    keys.push("Enter".into());
    Some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured on 23 Sep from a throwaway Haiku session asked to `touch` a file with permission mode default.
    const PERMISSION: &str = " ▐▛███▛█   Claude Code v2.1.280
❯ Run exactly this bash command and nothing else: touch /tmp/alondra-menu-probe-file
  Running 1 shell command…
  ⎿  $ touch /tmp/alondra-menu-probe-file
────────────────────────────────────────────────────────────────────────────────
 Bash command

   touch /tmp/alondra-menu-probe-file
   Create empty probe file

 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to /tmp from this project
   3. No

 Esc to cancel · Tab to amend
";

    /// Quill, 24 Sep: context7's elicitation dialog as Claude Code v2.1.280 draws it (the pointer
    /// before the focused button, the buttons side by side, the key hint below). Its buttons line
    /// is rebuilt from the dialog's render code, not captured from a live terminal.
    fn elicitation(buttons: &str) -> String {
        format!(
            " ▐▛███▜▌   Claude Code v2.1.280
❯ Find the axum docs for extractors
● context7 - resolve-library-id (MCP)(libraryName: \"axum\")
────────────────────────────────────────────────────────────────────────────────
 MCP server “context7” requests your input
 You're using Context7 anonymously. Get a free API key at context7.com/dashboard
 for higher rate limits.

 {buttons}

 Esc to cancel · ↑/↓ to navigate
"
        )
    }

    #[test]
    fn a_dialog_with_its_options_on_one_row_becomes_a_card_answered_with_the_arrows() {
        let cases: [(&str, &str, Option<usize>, &str, Option<&[&str]>); 7] = [
            ("Accept focused, Accept picked", "❯ Accept    Decline", Some(0), "Accept", Some(&["Enter"])),
            ("Accept focused, Decline picked", "❯ Accept    Decline", Some(0), "Decline", Some(&["Right", "Enter"])),
            ("Decline focused, Accept picked", "  Accept  ❯ Decline", Some(1), "Accept", Some(&["Left", "Enter"])),
            ("written with a slash", "❯ Accept / Decline", Some(0), "Decline", Some(&["Right", "Enter"])),
            ("an option it does not offer", "❯ Accept    Decline", Some(0), "Maybe", None),
            ("no cursor on the row", "Accept / Decline", None, "Accept", None),
            ("two cursors on the row", "❯ Accept  ❯ Decline", None, "Accept", None),
        ];
        for (what, buttons, selected, label, keys) in cases {
            let menu = parse_menu(&elicitation(buttons));
            assert_eq!(menu.as_ref().and_then(|m| m.selected), selected, "{what}");
            let Some(menu) = menu else { continue };
            assert_eq!(menu.title, "MCP server “context7” requests your input", "{what}");
            assert!(menu.detail.contains("You're using Context7 anonymously"), "{what}");
            assert_eq!(menu.options, ["Accept", "Decline"], "{what}");
            assert_eq!(key_for(&menu, label), keys.map(|k| k.iter().map(|s| s.to_string()).collect()), "{what}");
        }
    }

    #[test]
    fn a_row_is_the_same_menu_wherever_its_cursor_is() {
        let accept = parse_menu(&elicitation("❯ Accept    Decline")).unwrap();
        let decline = parse_menu(&elicitation("  Accept  ❯ Decline")).unwrap();
        assert!(accept.same_as(&decline));
        assert!(!accept.same_as(&parse_menu(PERMISSION).unwrap()));
    }

    #[test]
    fn the_prompt_box_is_not_a_row_of_options() {
        let screens = [
            "● done\n────────────────────────────────\n❯ fix  this  now\n────────────────────────────────\n  ? for shortcuts\n",
            "● done\n────────────────────────────────\n❯ fix  this\n",
            "────────────────────────────────\n❯ Accept    Decline\n Esc to cancel\n",
        ];
        for screen in screens {
            assert_eq!(parse_menu(screen), None, "{screen}");
        }
    }

    #[test]
    fn a_permission_menu_becomes_a_card_with_its_own_options() {
        let m = parse_menu(PERMISSION).unwrap();
        assert_eq!(m.title, "Bash command: Do you want to proceed?");
        assert!(m.detail.contains("touch /tmp/alondra-menu-probe-file"));
        assert_eq!(m.options, ["Yes", "Yes, and always allow access to /tmp from this project", "No"]);
        assert_eq!(m.selected, None);
    }

    #[test]
    fn a_screen_without_a_numbered_menu_is_not_parsed() {
        assert_eq!(parse_menu("❯ hello\n────────────────────────────────\n❯ \n"), None);
        assert_eq!(parse_menu("no rule at all\n1. one\n2. two"), None);
    }

    #[test]
    fn only_sessions_waiting_on_their_terminal_are_taken_from_the_daemon() {
        let json = r#"[{"id":"aa","status":"waiting","waitingFor":"permission prompt"},{"id":"bb","status":"idle"},{"id":"cc","status":"busy","waitingFor":"dialog open"},{"id":"dd","status":"waiting","waitingFor":"dialog open"}]"#;
        assert_eq!(waiting_from(json), vec![("aa".to_string(), "permission prompt".to_string()), ("dd".to_string(), "dialog open".to_string())]);
        assert!(waiting_from("not json").is_empty());
    }

    fn seen(state: &str) -> Event {
        Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "s".into(), short_id: "s".into(), pid: Some(7), state: Some(state.into()), tempo: Some("idle".into()), ..Default::default() })
    }

    /// test/menu-probe, 23 Sep: parked on a permission while its state.json still said
    /// "working", and the sentinel had already turned its needs into a notice.
    #[test]
    fn a_node_is_asked_about_whenever_it_is_not_visibly_working() {
        let notice = Event::ReportPosted(alondra_core::Report {
            id: 1,
            session_id: "s".into(),
            node_name: None,
            title: "approve Bash".into(),
            body: None,
            links: vec![],
            attachments: vec![],
            kind: alondra_core::ReportKind::Notice,
            created_at: 2,
            read: true,
            mission: None,
            looks_like_question: false,
        });
        let hook = |e: &str| Event::Hook(alondra_core::HookEvent { session_id: "s".into(), event: e.into(), tool_name: Some("Bash".into()), ..Default::default() });
        let cases: Vec<(&str, Vec<Event>, bool)> = vec![
            ("state.json says working but a notice was delivered", vec![seen("working"), notice], true),
            ("the daemon says blocked", vec![seen("blocked")], true),
            ("a tool in flight", vec![seen("working"), hook("UserPromptSubmit"), hook("PreToolUse")], false),
        ];
        for (what, events, expected) in cases {
            let mut v = View::default();
            for (i, e) in events.iter().enumerate() {
                v.apply(i as u64 + 1, 1000 + i as i64, e);
            }
            assert_eq!(suspect(&v, &v.nodes["s"], 5000), expected, "{what}");
        }
    }

    #[test]
    fn an_option_is_answered_with_its_number() {
        let mut v = View::default();
        v.apply(1, 1, &Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "s".into(), short_id: "s".into(), ..Default::default() }));
        let q = card(&v, &v.nodes["s"], PERMISSION);
        assert!(is_menu_card(&q));
        let menu = parse_menu(PERMISSION).unwrap();
        assert_eq!(key_for(&menu, "No"), Some(vec!["3".to_string()]));
        assert_eq!(key_for(&menu, "Maybe"), None);
    }

    #[test]
    fn a_menu_alondra_cannot_read_sends_the_user_to_the_terminal_in_spanish() {
        let mut v = View::default();
        v.apply(1, 1, &Event::DaemonSeen(alondra_core::DaemonObservation { session_id: "s".into(), short_id: "s".into(), ..Default::default() }));
        let screen = "Some dialog\nwith no numbered options\nPress any key";
        let q = card(&v, &v.nodes["s"], screen);
        assert_eq!(q.title, format!("{} espera en un menú que Alondra no sabe leer: ábrelo en su terminal", v.nodes["s"].display_name()));
        assert!(q.detail.as_deref().is_some_and(|d| d.contains("Press any key")));
        assert!(q.options.is_empty());
    }
}
