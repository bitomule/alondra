use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alondra_core::Event;

use crate::hub::Hub;

type Pending = Vec<(u64, String, Option<u64>)>;

const RETRY_AFTER: Duration = Duration::from_secs(120);
const PAINT_TIMEOUT: Duration = Duration::from_secs(25);
const MENU_MARKERS: &[&str] = &["Enter to select", "to navigate", "Do you want to proceed", "❯ 1.", "trust this folder", "Enter to confirm"];

pub async fn run(hub: Arc<Hub>) {
    let mut last_try: HashMap<String, Instant> = HashMap::new();
    loop {
        hub.wait_event(Duration::from_secs(10)).await;
        let due: Vec<(String, String, Pending)> = hub.read(|v| {
            v.nodes
                .values()
                .filter(|n| n.in_daemon)
                .filter_map(|n| {
                    let msgs: Pending = v
                        .pending_messages(&n.session_id)
                        .map(|m| (m.id, m.text.clone(), m.answers_question))
                        .collect();
                    (!msgs.is_empty()).then(|| (n.session_id.clone(), n.short_id.clone(), msgs))
                })
                .collect()
        });
        last_try.retain(|_, t| t.elapsed() < RETRY_AFTER);
        for (session_id, short, msgs) in due {
            if last_try.contains_key(&session_id) {
                continue;
            }
            last_try.insert(session_id.clone(), Instant::now());
            let text = msgs.iter().map(|(_, t, _)| t.replace('\n', " ")).collect::<Vec<_>>().join(" · ");
            let short_for_task = short.clone();
            let outcome = tokio::task::spawn_blocking(move || type_into(&short_for_task, &text)).await;
            match outcome {
                Ok(Ok(())) => {
                    for (id, _, question) in msgs {
                        let _ = hub.append(Event::MessageHanded { id });
                        if let Some(q) = question {
                            let _ = hub.append(Event::QuestionDelivered { id: q });
                        }
                    }
                    eprintln!("alondrad: message delivered to {short} through its terminal");
                }
                Ok(Err(e)) => eprintln!("alondrad: could not deliver to {short}: {e}"),
                Err(e) => eprintln!("alondrad: delivery task broken for {short}: {e}"),
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

fn type_into(short: &str, text: &str) -> Result<(), String> {
    let session = attach(short)?;
    std::thread::sleep(Duration::from_secs(2));
    let screen = tmux(&["capture-pane", "-p", "-t", &session.0]).unwrap_or_default();
    if MENU_MARKERS.iter().any(|m| screen.contains(m)) {
        return Err("the node is in a menu; not typing over it".into());
    }
    tmux(&["send-keys", "-t", &session.0, "-l", text])?;
    std::thread::sleep(Duration::from_millis(600));
    tmux(&["send-keys", "-t", &session.0, "Enter"])?;
    std::thread::sleep(Duration::from_secs(2));
    Ok(())
}
