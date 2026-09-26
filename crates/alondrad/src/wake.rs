use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alondra_core::{Event, now_millis, upkeep};

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
        compact_waiting(&hub).await;
        let due: Vec<(String, String)> = hub.read(|v| v.nodes.values().filter(|n|n.in_daemon)
            .map(|n|(n.session_id.clone(),n.short_id.clone())).collect());
        for (session_id, short) in due {
            if !backoff.ready(&session_id) { continue; }
            let attempt=match crate::delivery::claim(&hub,&session_id,alondra_core::delivery::Channel::Terminal) {
                Ok(Some(a))=>a, Ok(None)=>continue, Err(e)=>{log!("delivery claim failed: {e}");continue;}
            };
            let msgs:Pending=hub.read(|v|attempt.message_ids.iter().filter_map(|id|v.messages.get(id))
                .map(|m|(m.id,m.text.clone(),m.answers_question)).collect());
            let outcome=match turn_for(&hub.home,&msgs) {
                Err(e)=>{log!("could not prepare message: {e}");crate::delivery::Outcome::NotStarted("prepare_failed")}
                Ok(text)=>{
                    let own=hub.own_attach(&short);
                    let result=tokio::task::spawn_blocking(move||type_into(&short,&text)).await;
                    drop(own);
                    match result {
                        Ok(Ok(()))=>crate::delivery::Outcome::Handed,
                        Ok(Err(WriteFailure::NotStarted))=>crate::delivery::Outcome::NotStarted("terminal_not_ready"),
                        Ok(Err(WriteFailure::Uncertain))=>crate::delivery::Outcome::Uncertain("terminal_write_uncertain"),
                        Err(_)=>crate::delivery::Outcome::Uncertain("delivery_worker_failed"),
                    }
                }
            };
            backoff.outcome(&session_id,matches!(outcome,crate::delivery::Outcome::Handed));
            if let Err(e)=crate::delivery::finish(&hub,attempt.id,outcome) {
                log!("delivery outcome not committed; attempt {} remains blocked: {e}",attempt.id);
            }
        }
    }
}

/// Claims `/compact` before writing. A missing completion remains uncertain and blocks the queue.
async fn compact_waiting(hub: &Hub) {
    let now=now_millis();
    let (to_type,overdue):(Vec<(String,String)>,Vec<(String,String)>)=hub.read(|v| {
        let waiting=v.nodes.values().filter(|n|n.in_daemon && n.compaction.is_some() && !v.delivery_blocked(&n.session_id));
        let (overdue,rest):(Vec<_>,Vec<_>)=waiting.partition(|n|upkeep::compaction_overdue(n,now) && n.compaction.is_some_and(|c|c.typed_at.is_some()));
        let ids=|n:&&alondra_core::Node|(n.session_id.clone(),n.short_id.clone());
        (rest.iter().filter(|n|n.compaction.is_some_and(|c|c.typed_at.is_none())).map(ids).collect(),overdue.iter().map(ids).collect())
    });
    // A confirmed local handoff without PostCompact is still ambiguous. Never release its queue.
    for (sid,_) in overdue {
        if let Err(error)=crate::delivery::mark_compaction_overdue(hub,&sid,now) {
            log!("could not persist compaction uncertainty: {error}");
        }
    }
    for (sid,short) in to_type {
        let attempt=match crate::delivery::claim(hub,&sid,alondra_core::delivery::Channel::Compact) {
            Ok(Some(a))=>a, Ok(None)=>continue, Err(e)=>{log!("compaction claim failed: {e}");continue;}
        };
        let own=hub.own_attach(&short);
        let result=tokio::task::spawn_blocking(move||type_into(&short,upkeep::COMPACT_COMMAND)).await;
        drop(own);
        let outcome=match result {
            Ok(Ok(()))=>crate::delivery::Outcome::Handed,
            Ok(Err(WriteFailure::NotStarted))=>crate::delivery::Outcome::NotStarted("terminal_not_ready"),
            Ok(Err(WriteFailure::Uncertain))=>crate::delivery::Outcome::Uncertain("compaction_write_uncertain"),
            Err(_)=>crate::delivery::Outcome::Uncertain("compaction_worker_failed"),
        };
        if let Err(e)=crate::delivery::finish(hub,attempt.id,outcome) {log!("compaction outcome not committed: {e}");}
    }
}

/// What Alondra asks Claude Code to do with a background session, so restarting one can run
/// against a fake.
pub trait Sessions: Sync {
    fn respawn(&self, short: &str) -> impl Future<Output = Result<(), String>> + Send;
}

pub struct ClaudeCli;

impl Sessions for ClaudeCli {
    /// `claude respawn <id>` restarts a background session, running or stopped, with its
    /// conversation intact (code.claude.com/docs/en/cli-reference).
    async fn respawn(&self, short: &str) -> Result<(), String> {
        let run = tokio::process::Command::new("claude").args(["respawn", short]).kill_on_drop(true).output();
        match tokio::time::timeout(Duration::from_secs(60), run).await {
            Ok(Ok(out)) if out.status.success() => Ok(()),
            Ok(Ok(out)) => {
                let said = format!("{}{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout)).trim().to_string();
                Err(if said.is_empty() { format!("claude respawn ended with {}", out.status) } else { said })
            }
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("claude respawn did not answer in 60 s".into()),
        }
    }
}

const RESUME_EVERY: Duration = Duration::from_secs(30);

pub async fn resume_loop(hub: Arc<Hub>) {
    loop {
        hub.wait_event(RESUME_EVERY).await;
        resume_dead(&hub, &ClaudeCli, now_millis(), hub.machine().strained()).await;
    }
}

/// Restarts the nodes whose process ended while they still had work and tells each one to carry
/// on; the orchestrator hears about those that could not be restarted, and about those that
/// kept dying. Nothing is restarted while the machine is strained: that may be why they died.
pub async fn resume_dead(hub: &Hub, sessions: &impl Sessions, now: i64, strained: bool) {
    let user = hub.config.name.clone();
    let spent: Vec<String> = hub.read(|v| v.nodes.values().filter(|n| n.resumed.as_ref().is_some_and(|r| r.streak >= upkeep::MAX_RESUMES)).map(|n| n.session_id.clone()).collect());
    for session_id in spent {
        if let Err(e) = hub.append_with(|v| v.nodes.get(&session_id).and_then(|n| upkeep::resumes_given_up_message(v, n, now)).map(Event::MessageQueued)) {
            log!("could not tell the orchestrator about a node that keeps stopping: {e}");
        }
    }
    if strained {
        return;
    }
    let due: Vec<(String, String)> = hub.read(|v| {
        v.nodes
            .values()
            .filter(|n| !hub.is_closing(&n.session_id) && upkeep::resume_due(v, n, now).is_some())
            .map(|n| (n.session_id.clone(), n.short_id.clone()))
            .collect()
    });
    for (session_id, short) in due {
        let error = sessions.respawn(&short).await.err();
        match &error {
            None => log!("restarted {short}: its process ended while it still had work"),
            Some(e) => log!("could not restart {short}: {e}"),
        }
        let ok = error.is_none();
        if let Err(e) = hub.append(Event::Resumed { session_id: session_id.clone(), ok, error }) {
            log!("could not record the restart of {short}: {e}");
            continue;
        }
        if ok && let Err(e) = hub.append_with(|v| upkeep::resumed_message(v, &session_id, now).map(Event::MessageQueued)) {
            log!("could not tell {short} it was restarted: {e}");
        }
    }
    let failed: Vec<String> = hub.read(|v| v.nodes.values().filter(|n| n.resumed.as_ref().is_some_and(|r| !r.ok)).map(|n| n.session_id.clone()).collect());
    for session_id in failed {
        if let Err(e) = hub.append_with(|v| v.nodes.get(&session_id).and_then(|n| upkeep::resume_failed_message(v, n, &user, now)).map(Event::MessageQueued)) {
            log!("could not tell the orchestrator about a failed restart: {e}");
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

#[derive(Debug)]
enum WriteFailure { NotStarted, Uncertain }

fn type_into(short: &str, text: &str) -> Result<(), WriteFailure> {
    let session=attach(short).map_err(|_|WriteFailure::NotStarted)?;
    write_terminal(&session.0,text,tmux,std::thread::sleep)
}

fn write_terminal(session:&str,text:&str,mut command:impl FnMut(&[&str])->Result<String,String>,mut wait:impl FnMut(Duration))->Result<(),WriteFailure> {
    wait(Duration::from_secs(2));
    let screen=command(&["capture-pane","-p","-t",session]).map_err(|_|WriteFailure::NotStarted)?;
    if menu_on_screen(&screen).is_some() { return Err(WriteFailure::NotStarted); }
    // Even a failing send-keys may have written a prefix. No error beyond this point is safe to retry.
    command(&["send-keys","-t",session,"-l",text]).map_err(|_|WriteFailure::Uncertain)?;
    wait(Duration::from_millis(600));
    let typed=command(&["capture-pane","-p","-t",session]).map_err(|_|WriteFailure::Uncertain)?;
    if folded_into_a_paste(&typed) { return Err(WriteFailure::Uncertain); }
    command(&["send-keys","-t",session,"Enter"]).map_err(|_|WriteFailure::Uncertain)?;
    wait(Duration::from_secs(2));
    Ok(())
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────────────────────────";

    #[test]
    fn delivery_transport_classifies_prewrite_and_partial_failures_without_resubmitting() {
        for failure in 0..=4 {
            let mut calls=vec![];
            let result=write_terminal("fake","secret text",|args| {
                let index=calls.len(); calls.push(args.iter().map(|s|s.to_string()).collect::<Vec<_>>());
                if index==failure { Err("injected I/O failure".into()) } else { Ok("normal prompt".into()) }
            },|_|());
            if failure==0 { assert!(matches!(result,Err(WriteFailure::NotStarted))); }
            else if failure<4 { assert!(matches!(result,Err(WriteFailure::Uncertain))); }
            else { assert!(result.is_ok()); }
            assert_eq!(calls.iter().filter(|c|c.first().is_some_and(|s|s=="send-keys") && c.last().is_some_and(|s|s=="secret text")).count(),usize::from(failure!=0));
        }
        let mut step=0;let mut submitted=false;
        let result=write_terminal("fake","text",|args| {step+=1;if args.last()==Some(&"Enter") {submitted=true;} Ok(if step==3 {"❯ [Pasted text #1]"} else {"normal prompt"}.into())},|_|());
        assert!(matches!(result,Err(WriteFailure::Uncertain)));assert!(!submitted);
    }

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

#[cfg(test)]
mod resume_tests {
    use super::*;
    use alondra_core::{DaemonObservation, HookEvent, MessageFrom};

    struct FakeClaude {
        refuses: Option<&'static str>,
        respawned: std::sync::Mutex<Vec<String>>,
    }

    impl FakeClaude {
        fn new(refuses: Option<&'static str>) -> Self {
            FakeClaude { refuses, respawned: Default::default() }
        }

        fn calls(&self) -> Vec<String> {
            self.respawned.lock().unwrap().clone()
        }
    }

    impl Sessions for FakeClaude {
        async fn respawn(&self, short: &str) -> Result<(), String> {
            self.respawned.lock().unwrap().push(short.to_string());
            self.refuses.map_or(Ok(()), |why| Err(why.to_string()))
        }
    }

    fn hub(tag: &str) -> Hub {
        let home = std::env::temp_dir().join(format!("alondra-resume-{tag}-{}-{}", std::process::id(), now_millis()));
        let config = alondra_core::config::Config::resolve(home, &Default::default(), |_| None, || Some("Ana".into()));
        Hub::open(config, false).unwrap()
    }

    fn seen(session: &str, state: &str, pid: Option<i32>) -> Event {
        let short = session.chars().take(8).collect();
        Event::DaemonSeen(DaemonObservation { session_id: session.into(), short_id: short, name: Some("n".into()), pid, state: Some(state.into()), ..Default::default() })
    }

    fn died(hub: &Hub, session: &str, pid: i32, after: &str) {
        hub.append(seen(session, "working", Some(pid))).unwrap();
        hub.append(Event::Hook(HookEvent { session_id: session.into(), event: "UserPromptSubmit".into(), ..Default::default() })).unwrap();
        let id = hub.read(|v| v.reports.keys().next_back().copied().unwrap_or(0) + 1);
        hub.append(Event::ReportPosted(alondra_core::Report {
            id, session_id: session.into(), node_name: None, title: "Halfway".into(), body: None, links: vec![], attachments: vec![], kind: alondra_core::ReportKind::Notice,
            created_at: now_millis(), read: false, to_orchestrator: false, mission: None, looks_like_question: false, task: None,
        }))
        .unwrap();
        hub.append(Event::ProcessExited { session_id: session.into(), pid }).unwrap();
        hub.append(seen(session, after, None)).unwrap();
    }

    fn alondra_said(hub: &Hub, session: &str, prefix: &str) -> usize {
        hub.read(|v| v.messages.values().filter(|m| m.session_id == session && m.from == MessageFrom::Alondra && m.text.starts_with(prefix)).count())
    }

    const MIN: i64 = 60_000;

    #[tokio::test]
    async fn a_node_that_died_with_work_is_respawned_once_and_told_to_carry_on() {
        let hub = hub("ok");
        died(&hub, "deadnode-1", 7, "working");
        died(&hub, "idlenode-1", 8, "done");
        died(&hub, "stopnode-1", 9, "stopped");
        let claude = FakeClaude::new(None);
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert_eq!(claude.calls(), vec!["deadnode"], "only the one that died while working: not the one the daemon let go, nor the one someone stopped");
        assert_eq!(hub.read(|v| v.nodes["deadnode-1"].resumed.as_ref().map(|r| r.ok)), Some(true));
        assert_eq!(alondra_said(&hub, "deadnode-1", upkeep::RESUMED_PREFIX), 1);

        resume_dead(&hub, &claude, now_millis() + 3 * 3600 * 1000, false).await;
        assert_eq!(claude.calls().len(), 1, "once per exit");

        tokio::time::sleep(Duration::from_millis(5)).await;
        hub.append(seen("deadnode-1", "working", Some(9))).unwrap();
        hub.append(Event::ProcessExited { session_id: "deadnode-1".into(), pid: 9 }).unwrap();
        hub.append(seen("deadnode-1", "failed", None)).unwrap();
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert_eq!(claude.calls().len(), 1, "died again within the backoff");
        resume_dead(&hub, &claude, now_millis() + upkeep::RESUME_BACKOFF_MS + MIN, false).await;
        assert_eq!(claude.calls().len(), 2, "past the backoff it is restarted again");
    }

    /// A few milliseconds after the last one, so each exit has its own time: restarts are one per exit.
    async fn died_again(hub: &Hub, session: &str, pid: i32) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        hub.append(seen(session, "working", Some(pid))).unwrap();
        hub.append(Event::ProcessExited { session_id: session.into(), pid }).unwrap();
        hub.append(seen(session, "failed", None)).unwrap();
    }

    fn with_orchestrator(tag: &str) -> Hub {
        let hub = hub(tag);
        hub.append(Event::MarkedOrchestrator { session_id: "orchestr-1".into() }).unwrap();
        hub.append(Event::Hook(HookEvent { session_id: "orchestr-1".into(), event: "SessionStart".into(), ..Default::default() })).unwrap();
        hub
    }

    /// A node whose process kept dying was restarted every 15 min for good, and each restart
    /// queued one more "your process stopped" message for it.
    #[tokio::test]
    async fn a_node_that_keeps_dying_is_restarted_three_times_then_left_to_the_orchestrator() {
        let hub = with_orchestrator("streak");
        died(&hub, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(None);
        let mut now = now_millis() + 2 * MIN;
        resume_dead(&hub, &claude, now, false).await;
        for pid in [8, 9, 10] {
            died_again(&hub, "deadnode-1", pid).await;
            now += upkeep::RESUME_BACKOFF_MS + MIN;
            resume_dead(&hub, &claude, now, false).await;
        }
        assert_eq!(claude.calls().len(), 3, "three restarts in a row, then no more");
        assert_eq!(alondra_said(&hub, "deadnode-1", upkeep::RESUMED_PREFIX), 1, "one carry-on message while it has not reached the node");
        assert_eq!(alondra_said(&hub, "orchestr-1", upkeep::RESUMES_GIVEN_UP_PREFIX), 1);
        resume_dead(&hub, &claude, now + upkeep::RESUME_BACKOFF_MS, false).await;
        assert_eq!((claude.calls().len(), alondra_said(&hub, "orchestr-1", upkeep::RESUMES_GIVEN_UP_PREFIX)), (3, 1), "told once");

        let turns = with_orchestrator("streak-reset");
        died(&turns, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(None);
        let mut now = now_millis() + 2 * MIN;
        resume_dead(&turns, &claude, now, false).await;
        for pid in [8, 9, 10] {
            turns.append(Event::Hook(HookEvent { session_id: "deadnode-1".into(), event: "Stop".into(), ..Default::default() })).unwrap();
            died_again(&turns, "deadnode-1", pid).await;
            now += upkeep::RESUME_BACKOFF_MS + MIN;
            resume_dead(&turns, &claude, now, false).await;
        }
        assert_eq!(claude.calls().len(), 4, "a turn done between deaths starts the count again");
        for pid in [11, 12, 13] {
            died_again(&turns, "deadnode-1", pid).await;
            now += upkeep::RESUME_BACKOFF_MS + MIN;
            resume_dead(&turns, &claude, now, false).await;
        }
        assert_eq!(claude.calls().len(), 6, "three in a row from the last turn done, not from the first restart");
    }

    /// Restarting nodes while the machine is short of memory adds to what may have killed them.
    #[tokio::test]
    async fn nothing_is_restarted_while_the_machine_is_strained() {
        let hub = hub("strained");
        died(&hub, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(None);
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, true).await;
        assert!(claude.calls().is_empty());
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert_eq!(claude.calls(), vec!["deadnode"], "once it has room again");
    }

    #[tokio::test]
    async fn a_node_alondra_is_closing_is_not_brought_back() {
        let hub = hub("closing");
        died(&hub, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(None);
        let closing = hub.closing("deadnode-1");
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert!(claude.calls().is_empty(), "claude rm stopped it on the way out");
        drop(closing);
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        assert_eq!(claude.calls(), vec!["deadnode"], "a close that did not go through leaves it to be restarted");
    }

    #[tokio::test]
    async fn a_failed_respawn_is_not_retried_for_that_exit_and_reaches_the_orchestrator_once() {
        let hub = hub("fails");
        hub.append(Event::MarkedOrchestrator { session_id: "orchestr-1".into() }).unwrap();
        hub.append(Event::Hook(HookEvent { session_id: "orchestr-1".into(), event: "SessionStart".into(), ..Default::default() })).unwrap();
        died(&hub, "deadnode-1", 7, "working");
        let claude = FakeClaude::new(Some("No job matching deadnode"));
        resume_dead(&hub, &claude, now_millis() + 2 * MIN, false).await;
        resume_dead(&hub, &claude, now_millis() + 3 * MIN, false).await;
        assert_eq!(claude.calls(), vec!["deadnode"]);
        assert_eq!(hub.read(|v| v.nodes["deadnode-1"].resumed.clone().and_then(|r| r.error)).as_deref(), Some("No job matching deadnode"));
        assert_eq!(alondra_said(&hub, "orchestr-1", upkeep::RESUME_FAILED_PREFIX), 1);
        assert_eq!(alondra_said(&hub, "deadnode-1", upkeep::RESUMED_PREFIX), 0, "nothing to tell a node that is still down");
    }
}
