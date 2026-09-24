use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alondra_core::Event;

use crate::hub::Hub;
use crate::uploads;

type Pending = Vec<(u64, String, Option<u64>)>;

const RETRY_AFTER: Duration = Duration::from_secs(120);
const PAINT_TIMEOUT: Duration = Duration::from_secs(25);
const MENU_MARKERS: &[&str] = &["Enter to select", "to navigate", "Do you want to proceed", "❯ 1.", "trust this folder", "Enter to confirm"];

/// Claude Code takes a burst of about 900 typed bytes or more as a paste and folds it into
/// "[Pasted text #N]", and through `claude attach` that block never reaches the node: only what
/// followed it does. Anything longer than this is stored whole and the node is told where.
const MAX_TYPED_BYTES: usize = 600;
const PASTE_MARKERS: &[&str] = &["[Pasted text #", "paste again to expand"];

/// The single line typed into the node for these messages: the messages themselves when they
/// fit, or a pointer to a file holding them whole, paragraphs included.
fn turn_for(home: &Path, msgs: &Pending) -> std::io::Result<String> {
    let typed = msgs.iter().map(|(_, t, _)| t.replace('\n', " ")).collect::<Vec<_>>().join(" · ");
    let Some((first, _, _)) = msgs.first().filter(|_| typed.len() > MAX_TYPED_BYTES) else {
        return Ok(typed);
    };
    let whole = msgs.iter().map(|(_, t, _)| t.as_str()).collect::<Vec<_>>().join("\n\n---\n\n");
    let dir = uploads::messages_dir(home).join(format!("m-{first}"));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("message.md");
    std::fs::write(&path, &whole)?;
    Ok(format!(
        "[Alondra] A message for you is too long to type into this terminal ({} characters), so it is stored whole in `{}`. Read that file with the Read tool now and act on its full text as this turn's message.",
        whole.chars().count(),
        path.display()
    ))
}

/// A node whose last delivery failed is left alone for a while; one that took its messages gets
/// the next ones as soon as they are queued, since Claude Code queues what is typed mid-turn.
#[derive(Default)]
struct Backoff {
    failed: HashMap<String, Instant>,
}

impl Backoff {
    fn ready(&mut self, session_id: &str) -> bool {
        self.failed.retain(|_, t| t.elapsed() < RETRY_AFTER);
        !self.failed.contains_key(session_id)
    }

    fn outcome(&mut self, session_id: &str, delivered: bool) {
        if delivered {
            self.failed.remove(session_id);
        } else {
            self.failed.insert(session_id.to_string(), Instant::now());
        }
    }
}

pub async fn run(hub: Arc<Hub>) {
    let mut backoff = Backoff::default();
    loop {
        hub.wait_event(Duration::from_secs(10)).await;
        let due: Vec<(String, String, Pending)> = hub.read(|v| {
            v.nodes
                .values()
                .filter(|n| n.in_daemon)
                .filter_map(|n| {
                    let msgs: Pending = v
                        .deliverable_messages(&n.session_id)
                        .map(|m| (m.id, m.text.clone(), m.answers_question))
                        .collect();
                    (!msgs.is_empty()).then(|| (n.session_id.clone(), n.short_id.clone(), msgs))
                })
                .collect()
        });
        for (session_id, short, msgs) in due {
            if !backoff.ready(&session_id) {
                continue;
            }
            let text = match turn_for(&hub.home, &msgs) {
                Ok(t) => t,
                Err(e) => {
                    log!("could not store the long message for {short}: {e}");
                    backoff.outcome(&session_id, false);
                    continue;
                }
            };
            let short_for_task = short.clone();
            let own = hub.own_attach(&short);
            let outcome = tokio::task::spawn_blocking(move || type_into(&short_for_task, &text)).await;
            drop(own);
            backoff.outcome(&session_id, matches!(outcome, Ok(Ok(()))));
            match outcome {
                Ok(Ok(())) => {
                    for (id, _, question) in msgs {
                        let _ = hub.append(Event::MessageHanded { id });
                        if let Some(q) = question {
                            let _ = hub.append(Event::QuestionDelivered { id: q });
                        }
                    }
                    log!("message delivered to {short} through its terminal");
                }
                Ok(Err(e)) => log!("could not deliver to {short}: {e}"),
                Err(e) => log!("delivery task broken for {short}: {e}"),
            }
        }
    }
}

struct TmuxSession(String);

impl Drop for TmuxSession {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).output();
    }
}

fn tmux(args: &[&str]) -> Result<String, String> {
    let out = Command::new("tmux").args(args).output().map_err(|e| format!("tmux: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Presses Esc in the node's session, which stops the call in flight the way it would at the keyboard.
pub fn interrupt(short: &str) -> Result<(), String> {
    let session = attach(short)?;
    tmux(&["send-keys", "-t", &session.0, "Escape"])?;
    std::thread::sleep(Duration::from_secs(2));
    Ok(())
}

/// What the node's terminal shows right now.
pub fn read_screen(short: &str) -> Result<String, String> {
    let session = attach(short)?;
    std::thread::sleep(Duration::from_secs(2));
    tmux(&["capture-pane", "-p", "-t", &session.0])
}

/// Picks `label` in the menu on the node's screen, by its number or with the arrows and Enter,
/// and fails if the menu is still on screen afterwards.
pub fn press(short: &str, label: &str) -> Result<(), String> {
    let session = attach(short)?;
    std::thread::sleep(Duration::from_secs(2));
    let before = tmux(&["capture-pane", "-p", "-t", &session.0])?;
    let Some(menu) = crate::menus::parse_menu(&before) else {
        return Err("the node is no longer on a menu".into());
    };
    let Some(keys) = crate::menus::key_for(&menu, label) else {
        return Err(format!("the menu on screen no longer offers \"{label}\""));
    };
    for key in &keys {
        tmux(&["send-keys", "-t", &session.0, key])?;
        std::thread::sleep(Duration::from_millis(300));
    }
    std::thread::sleep(Duration::from_secs(2));
    let after = tmux(&["capture-pane", "-p", "-t", &session.0])?;
    if crate::menus::parse_menu(&after).is_some_and(|m| m.same_as(&menu)) {
        return Err("the menu did not take the key".into());
    }
    Ok(())
}

fn attach(short: &str) -> Result<TmuxSession, String> {
    let name = format!("alondra-{short}");
    let _ = tmux(&["kill-session", "-t", &name]);
    tmux(&["new-session", "-d", "-s", &name, "-c", "/tmp", "-x", "200", "-y", "50", &format!("claude attach {short}")])?;
    let session = TmuxSession(name);
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let screen = tmux(&["capture-pane", "-p", "-t", &session.0]).unwrap_or_default();
        if screen.contains('❯') || screen.lines().filter(|l| !l.trim().is_empty()).count() > 8 {
            return Ok(session);
        }
        if start.elapsed() > PAINT_TIMEOUT {
            return Err("the session did not render in time".into());
        }
    }
}

/// A menu replaces the prompt box at the bottom of the screen, so only what follows the
/// second-to-last rule counts. The node's own text above it can quote a menu's words.
fn menu_on_screen(screen: &str) -> Option<(&'static str, String)> {
    let lines: Vec<&str> = screen.lines().collect();
    let is_rule = |l: &&str| l.trim().chars().count() > 20 && l.trim().chars().all(|c| c == '─');
    let rules: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| is_rule(l)).map(|(i, _)| i).collect();
    let from = match rules.as_slice() {
        [.., before_last, _] => before_last + 1,
        [only] => only + 1,
        [] => 0,
    };
    lines[from.min(lines.len())..]
        .iter()
        .find_map(|l| MENU_MARKERS.iter().find(|m| l.contains(*m)).map(|m| (*m, l.trim().to_string())))
}

/// The prompt box is the last block that starts with `❯`, with its hints below it; turns already
/// sent sit above it. The fold shows as a placeholder in the box, or only as the hint.
fn folded_into_a_paste(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    let from = lines.iter().rposition(|l| l.starts_with('❯')).unwrap_or(0);
    lines[from..].iter().any(|l| PASTE_MARKERS.iter().any(|m| l.contains(m)))
}

fn type_into(short: &str, text: &str) -> Result<(), String> {
    let session = attach(short)?;
    std::thread::sleep(Duration::from_secs(2));
    let screen = tmux(&["capture-pane", "-p", "-t", &session.0]).unwrap_or_default();
    if let Some((marker, line)) = menu_on_screen(&screen) {
        return Err(format!("the node is in a menu ({marker:?} in {line:?}); not typing over it"));
    }
    tmux(&["send-keys", "-t", &session.0, "-l", text])?;
    std::thread::sleep(Duration::from_millis(600));
    let typed = tmux(&["capture-pane", "-p", "-t", &session.0]).unwrap_or_default();
    if folded_into_a_paste(&typed) {
        return Err("the terminal folded the text into a paste, which would reach the node cut; not submitting it".into());
    }
    tmux(&["send-keys", "-t", &session.0, "Enter"])?;
    std::thread::sleep(Duration::from_secs(2));
    Ok(())
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────────────────────────";

    #[test]
    fn only_the_bottom_of_the_screen_can_hold_a_menu() {
        let idle_quoting_a_menu = format!(
            "⏺ Two options, pick one to navigate:\n ❯ 1. Valoraciones\n   2. Popularidad\n Enter to confirm\n{RULE}\n❯ \n{RULE}\n  ? for shortcuts\n"
        );
        let permission = format!(
            "⏺ Running 1 shell command…\n{RULE}\n Bash command\n\n   touch /tmp/x\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n\n Esc to cancel · Tab to amend\n"
        );
        let picker = format!("{RULE}\n Select model\n ❯ 1. Opus\n   2. Sonnet\n Enter to select · ↑/↓ to navigate\n{RULE}\n");
        let cases = [
            ("an idle node whose text quotes a menu's words", idle_quoting_a_menu.as_str(), None),
            ("a permission prompt", permission.as_str(), Some("Do you want to proceed")),
            ("a menu between two rules", picker.as_str(), Some("❯ 1.")),
            ("a bare trust prompt with no rules", "Do you trust this folder?\n ❯ 1. Yes, proceed\n", Some("trust this folder")),
            ("an empty screen", "", None),
        ];
        for (what, screen, marker) in cases {
            assert_eq!(menu_on_screen(screen).map(|(m, _)| m), marker, "{what}");
        }
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn only_a_failed_delivery_holds_back_the_next_one() {
        let cases = [("delivered", true, true), ("failed", false, false)];
        for (what, delivered, ready_again) in cases {
            let mut b = Backoff::default();
            assert!(b.ready("s"), "{what}: nothing tried yet");
            b.outcome("s", delivered);
            assert_eq!(b.ready("s"), ready_again, "{what}");
            assert!(b.ready("other"), "{what}: other nodes are not held back");
        }
        let mut b = Backoff::default();
        b.outcome("s", false);
        b.outcome("s", true);
        assert!(b.ready("s"), "a delivery clears an earlier failure");
    }
}

#[cfg(test)]
mod long_message_tests {
    use super::*;

    const LONG_REQUEST: &str = "[Alondra] New request from David: Estaba pensando que quizás estamos haciendo algo mal en Alondra. Tenemos un sistema de notificaciones donde los nodos usan el mcp para darme a mí info o al orquestador y estamos teniendo todo el rato conflictos por el estado. Por ejemplo hay 2 pr abiertas pero como conteste a una ahora no puedo hacer merge de esa. Y si los estados fueran por tareas y no por nodos? Así el orquestador podría incluso repartir en diferentes nodos. No quiero que hagas cambios, solo que evalúes pros/contras y los problemas que tenemos ahora vs ventajas y me los des en un report sencillito de leer\n\nOptions chosen by David (request r-1790263152254): project not chosen (you decide) · PR handling: review each PR · Effort: auto.\nPass request_id \"r-1790263152254\" to launch: Alondra writes these options into the node's mission and sets its model and effort, so do not restate them. Effort is auto: for each launch, choose light, normal or hard from the task and pass it as effort.\n\nPrepare the mission and launch the node with Alondra's launch tool, or use ask if something is missing.";

    fn home(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("alondra-wake-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn stored_path(turn: &str) -> Option<std::path::PathBuf> {
        turn.split('`').nth(1).map(std::path::PathBuf::from)
    }

    #[test]
    fn a_long_multi_paragraph_message_reaches_the_node_whole_through_a_short_turn() {
        let home = home("long");
        let turn = turn_for(&home, &vec![(594, LONG_REQUEST.to_string(), None)]).unwrap();
        assert!(turn.len() <= MAX_TYPED_BYTES, "the typed turn must stay under the paste fold: {} bytes", turn.len());
        assert!(!turn.contains('\n'), "a line break would submit the turn early");
        let path = stored_path(&turn).expect("the turn names the file holding the message");
        assert!(path.starts_with(uploads::messages_dir(&home)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), LONG_REQUEST, "the node reads every paragraph, not the tail");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn short_messages_are_typed_as_they_are() {
        let home = home("short");
        let msgs = vec![(1, "Sí,\nadelante".to_string(), None), (2, "Y cierra el PR".to_string(), None)];
        assert_eq!(turn_for(&home, &msgs).unwrap(), "Sí, adelante · Y cierra el PR");
        assert!(!home.exists(), "nothing is stored for a message that fits");
    }

    #[test]
    fn several_messages_that_only_fit_together_in_a_file_are_all_stored() {
        let home = home("several");
        let msgs = vec![(7, LONG_REQUEST.to_string(), None), (8, "Otra cosa más".to_string(), None)];
        let turn = turn_for(&home, &msgs).unwrap();
        let stored = std::fs::read_to_string(stored_path(&turn).unwrap()).unwrap();
        assert!(stored.starts_with(LONG_REQUEST) && stored.ends_with("Otra cosa más"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_fold_into_a_paste_is_caught_before_submitting() {
        let rule = "─".repeat(60);
        let cases = [
            ("the placeholder in the box", format!("❯ hola\n⏺ 4\n{rule}\n❯ [Pasted text #8]170 w0171 w0172\n{rule}\n  status\n"), true),
            (
                "only the hint, as the orchestrator's screen showed it",
                format!("{rule}\n❯ on and launch the node with Alondra's launch tool\n{rule}\n  📁 ~/Projects\n  paste again to expand\n"),
                true,
            ),
            ("a paste in a turn already sent", format!("❯ [Pasted text #1]\n⏺ ok\n{rule}\n❯ Sí, adelante\n{rule}\n"), false),
            ("plain text in the box", format!("{rule}\n❯ Sí, adelante\n{rule}\n  -- INSERT --\n"), false),
        ];
        for (what, screen, folded) in cases {
            assert_eq!(folded_into_a_paste(&screen), folded, "{what}");
        }
    }

    /// Types `LONG_REQUEST` into a real background session the way the daemon does. Run with
    /// `ALONDRA_LIVE_SESSION=<short id> cargo test -p alondrad live_ -- --ignored`, then read the
    /// session's transcript: its last user turn must lead it to the whole request.
    #[test]
    #[ignore]
    fn live_long_message_into_a_real_session() {
        let short = std::env::var("ALONDRA_LIVE_SESSION").expect("ALONDRA_LIVE_SESSION");
        let home = home("live");
        let turn = turn_for(&home, &vec![(1, LONG_REQUEST.to_string(), None)]).unwrap();
        type_into(&short, &turn).unwrap();
    }
}
