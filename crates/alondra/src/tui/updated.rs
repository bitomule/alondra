//! The TUI noticing it is behind its daemon: what the header says, Esc and R, the check of the
//! binary a restart would run, and the state handed to the new process.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use alondra_core::views::Service;
use crossterm::event::{KeyCode, KeyEvent};

use super::restore::{Draft, Restore};
use super::version_watch::{self, Decision, Disk, Inputs, Side, Warning};
use super::{App, Composer, Flow, Msg, Pane, Screen, now_millis};

const CLI: &str = env!("CARGO_PKG_VERSION");

pub struct Watch {
    pub service: Option<Service>,
    older_since: Option<i64>,
    disk: Disk,
    disk_for: Option<String>,
    last_key_at: i64,
    countdown_until: Option<i64>,
    stayed_for: Option<String>,
    asked_for: Option<String>,
    pub restarted_for: Option<String>,
    /// Actions and requests sent whose answer has not come back.
    pub actions_out: usize,
    /// The daemon serves no views protocol this terminal speaks; its version.
    pub no_common: Option<String>,
    pub notice: Option<Notice>,
    pub restart: bool,
}

impl Default for Watch {
    fn default() -> Self {
        Watch {
            service: None,
            older_since: None,
            disk: Disk::Unchecked,
            disk_for: None,
            last_key_at: now_millis(),
            countdown_until: None,
            stayed_for: None,
            asked_for: None,
            restarted_for: None,
            actions_out: 0,
            no_common: None,
            notice: None,
            restart: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub restarting: bool,
}

impl App {
    pub(super) fn service_seen(&mut self, service: Service) {
        let older = version_watch::compare(CLI, &service.version) == Side::ServiceOlder;
        let w = &mut self.updated;
        w.older_since = if older { w.older_since.or(Some(now_millis())) } else { None };
        w.no_common = match crate::client::pick_protocol(service.views_protocol) {
            Some(_) => None,
            None => Some(service.version.clone()),
        };
        w.service = Some(service);
    }

    pub(super) fn disk_checked(&mut self, version: String, disk: Disk) {
        if self.updated.service.as_ref().is_some_and(|s| s.version == version) {
            self.updated.disk = disk;
        }
    }

    fn busy(&self) -> bool {
        let sending = |chips: &[super::dropped::Chip]| {
            chips.iter().any(|c| c.upload == super::dropped::Upload::Sending)
        };
        self.updated.actions_out > 0
            || self.composer.as_ref().is_some_and(|c| sending(&c.chips))
            || self.drafts.values().any(|(_, chips)| sending(chips))
    }

    fn confirming(&self) -> bool {
        self.modal.is_some() || self.deferred_modal.is_some()
    }

    /// Whether anything in this instance holds back a restart of the whole terminal.
    pub(super) fn holds_restart(&self) -> bool {
        self.busy() || self.confirming()
    }

    pub(super) fn version_decision(&self, now: i64) -> Decision {
        let w = &self.updated;
        let service = w.service.as_ref().map(|s| s.version.as_str());
        let disk = if w.disk_for.as_deref() == service { &w.disk } else { &Disk::Unchecked };
        let rejected = self.daemon.as_ref().or(self.runtime.as_ref())
            .and_then(|r| r.update.as_ref())
            .is_some_and(|u| u.rejected.is_some());
        version_watch::decide(&Inputs {
            cli: CLI,
            service,
            remote: self.remote_mac().is_some(),
            disk,
            idle_ms: now - w.last_key_at,
            busy: self.busy(),
            confirming: self.confirming(),
            restarted_for: w.restarted_for.as_deref(),
            stayed: w.stayed_for.as_deref() == service && service.is_some(),
            asked: w.asked_for.as_deref() == service && service.is_some(),
            countdown_left_ms: w.countdown_until.map(|until| until - now),
            older_for_ms: w.older_since.map_or(0, |since| now - since),
            rejected,
        })
    }

    /// Runs every tick: says what the header shows and whether to restart now.
    pub(super) fn version_tick(&mut self) -> bool {
        let now = now_millis();
        let before = self.updated.notice.clone();
        let decision = self.version_decision(now);
        let version = self.updated.service.as_ref().map(|s| s.version.clone()).unwrap_or_default();
        if decision != Decision::Countdown {
            self.updated.countdown_until = None;
        }
        self.updated.restart = decision == Decision::Restart;
        let line = |text: String| Some(Notice { text, restarting: false });
        self.updated.notice = match decision {
            Decision::Nothing | Decision::Restart => None,
            Decision::CheckDisk => {
                self.check_disk(version);
                None
            }
            Decision::Countdown => {
                self.updated.countdown_until.get_or_insert(now + version_watch::COUNTDOWN_MS);
                Some(Notice {
                    text: trf!("Updated to {version} · restarting… (Esc stays)", "Actualizado a {version} · reiniciando… (Esc para quedarte)"),
                    restarting: true,
                })
            }
            Decision::Warn(warning) => line(self.warning_text(warning, &version)),
        };
        if let Some(version) = &self.updated.no_common {
            self.updated.notice = line(trf!(
                "Waiting for the service to restart on {version}…",
                "Esperando a que el servicio arranque con {version}…"
            ));
        }
        before != self.updated.notice
    }

    fn warning_text(&self, warning: Warning, daemon: &str) -> String {
        match warning {
            Warning::Updated => trf!(
                "Alondra was updated ({daemon}): restart the TUI · R",
                "Alondra se ha actualizado ({daemon}): reinicia la TUI · R"
            ),
            Warning::WaitAction => tr!("Waiting for the action to finish…", "Esperando a que termine la acción…").to_owned(),
            Warning::DiskOld => {
                let cli = match &self.updated.disk {
                    Disk::Version(v) => v.clone(),
                    _ => CLI.to_owned(),
                };
                trf!(
                    "The service runs {daemon}; this Mac has {cli} installed · alondra update",
                    "El servicio va con {daemon}; en este Mac está instalada {cli} · alondra update"
                )
            }
            Warning::ServiceOld => trf!(
                "The service is still on {daemon} · alondra doctor",
                "El servicio sigue en {daemon} · alondra doctor"
            ),
            Warning::RemoteNewer => {
                let mac = self.mac_tab(true).name;
                trf!(
                    "{mac} runs {daemon}; this terminal has {CLI} · alondra update",
                    "{mac} va con {daemon}; este terminal tiene {CLI} · alondra update"
                )
            }
        }
    }

    fn check_disk(&mut self, version: String) {
        if self.updated.disk_for.as_deref() == Some(&version) {
            return;
        }
        self.updated.disk_for = Some(version.clone());
        self.updated.disk = Disk::Unchecked;
        self.spawn(move |tx, _| {
            let disk = match disk_version(&stable_path()) {
                Ok(v) => Disk::Version(v),
                Err(e) => Disk::Failed(e),
            };
            let _ = tx.send(Msg::DiskVersion { version, disk });
        });
    }

    /// Esc stays during the countdown and R restarts from the fixed line; any key counts as use.
    pub(super) fn version_key(&mut self, k: &KeyEvent) -> Option<Flow> {
        self.updated.last_key_at = now_millis();
        let version = self.updated.service.as_ref().map(|s| s.version.clone())?;
        let restarting = self.updated.notice.as_ref().is_some_and(|n| n.restarting);
        if restarting && k.code == KeyCode::Esc {
            self.updated.stayed_for = Some(version);
            self.updated.countdown_until = None;
            return Some(Flow::Go);
        }
        let fixed = self.updated.notice.is_some() && !restarting && self.updated.no_common.is_none();
        if fixed && k.code == KeyCode::Char('R') && !self.typing()
            && matches!(self.version_decision(now_millis()), Decision::Warn(Warning::Updated | Warning::WaitAction))
        {
            self.updated.asked_for = Some(version);
            return Some(Flow::Go);
        }
        None
    }

    /// The request leaves: a restart waits for its answer.
    pub(super) fn action_sent(&mut self) {
        self.updated.actions_out += 1;
    }

    pub(super) fn action_answered(&mut self) {
        self.updated.actions_out = self.updated.actions_out.saturating_sub(1);
    }

    /// What the new process needs to open where this one is. Takes the composer: on a failed
    /// `exec` it comes back with `restored`.
    pub(super) fn to_restore(&mut self) -> Restore {
        Restore {
            restarted_for: self.updated.service.as_ref().map(|s| s.version.clone()).unwrap_or_default(),
            screen: match self.screen {
                Screen::Inbox => "inbox",
                Screen::Tasks => "tasks",
                Screen::Fleet => "fleet",
            }
            .into(),
            schedules_open: self.schedules_open,
            rules_open: self.rules.is_some(),
            detail_focused: self.focus == Pane::Detail,
            sel: self.sel.clone(),
            list_scroll: self.list_scroll,
            detail_scroll: self.detail_scroll,
            task_sel: self.task_sel.clone(),
            task_scroll: self.task_scroll,
            task_detail_scroll: self.task_detail_scroll,
            agent_sel: self.agent_sel.clone(),
            agent_scroll: self.agent_scroll,
            agent_detail_scroll: self.agent_detail_scroll,
            composer: self.composer.take().filter(|c| !matches!(c.target, super::Target::McpReturn { .. }))
                .map(|c| Draft { text: c.input.text(), chips: c.chips, target: c.target }),
            drafts: self.drafts.iter().map(|(k, (text, chips))| (k.clone(), text.clone(), chips.clone())).collect(),
        }
    }

    /// The `exec` failed: the composer is where it was.
    pub(super) fn exec_failed(&mut self, restore: Restore, error: &std::io::Error) {
        if let Some(draft) = restore.composer {
            self.reopen(draft);
        }
        self.updated.restarted_for = Some(restore.restarted_for);
        self.updated.restart = false;
        self.fail(trf!("Could not restart the TUI: {error}", "No pude reiniciar la TUI: {error}"));
    }

    pub(super) fn restore(&mut self, r: Restore) {
        self.show(match r.screen.as_str() {
            "tasks" => Screen::Tasks,
            "fleet" => Screen::Fleet,
            _ => Screen::Inbox,
        });
        if r.schedules_open {
            self.schedules_open = true;
            self.home_view = None;
            self.screen_view_asked = None;
            self.load_screen_view();
        }
        if r.rules_open {
            self.open_rules();
        }
        if r.detail_focused {
            self.focus = Pane::Detail;
        }
        self.sel = r.sel;
        self.list_scroll = r.list_scroll;
        self.detail_scroll = r.detail_scroll;
        self.task_sel = r.task_sel;
        self.task_scroll = r.task_scroll;
        self.task_detail_scroll = r.task_detail_scroll;
        self.agent_sel = r.agent_sel;
        self.agent_scroll = r.agent_scroll;
        self.agent_detail_scroll = r.agent_detail_scroll;
        self.drafts.extend(r.drafts.into_iter().map(|(k, text, chips)| (k, (text, chips))));
        if let Some(draft) = r.composer {
            self.reopen(draft);
        }
        self.updated.restarted_for = Some(r.restarted_for);
    }

    fn reopen(&mut self, draft: Draft) {
        self.compose(draft.target, &draft.text);
        if let Some(Composer { input, chips, .. }) = self.composer.as_mut() {
            *input = super::input::TextInput::with(&draft.text);
            *chips = draft.chips;
        }
    }
}

/// The binary a restart runs: Homebrew's stable link, `~/.local/bin`, or the path this one was
/// started from, so a replaced Cellar folder or a copied build still finds the new binary.
pub fn stable_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("alondra"));
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    stable_for(&exe, &home)
}

fn stable_for(exe: &Path, home: &Path) -> PathBuf {
    if let Some(prefix) = alondra_core::update::brew_prefix(exe) {
        return prefix.join("opt/alondra/bin/alondra");
    }
    let local = home.join(".local/bin/alondra");
    if exe.parent() == local.parent() {
        return local;
    }
    exe.to_path_buf()
}

trait ProbeProcess {
    fn poll(&mut self) -> std::io::Result<Option<std::process::ExitStatus>>;
    fn output(&mut self) -> String;
    fn kill(&mut self) -> std::io::Result<()>;
    fn reap(&mut self) -> std::io::Result<std::process::ExitStatus>;
}

impl ProbeProcess for std::process::Child {
    fn poll(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.try_wait()
    }
    fn output(&mut self) -> String {
        use std::io::Read;
        let mut out = String::new();
        self.stdout
            .take()
            .map(|mut stdout| stdout.read_to_string(&mut out));
        out
    }
    fn kill(&mut self) -> std::io::Result<()> {
        std::process::Child::kill(self)
    }
    fn reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.wait()
    }
}

trait ProbeClock {
    fn now(&mut self) -> Instant;
    fn pause(&mut self, duration: Duration);
}
struct RealProbeClock;
impl ProbeClock for RealProbeClock {
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

fn spawn_probe(path: &Path) -> Result<std::process::Child, String> {
    std::process::Command::new(path)
        .args(["--version", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())
}

fn probe_output(
    child: &mut impl ProbeProcess,
    status: std::process::ExitStatus,
) -> Result<String, String> {
    let out = child.output();
    if !status.success() {
        return Err(trf!("it exited with {status}", "salió con {status}"));
    }
    serde_json::from_str::<serde_json::Value>(&out)
        .ok()
        .and_then(|doc| doc["cli"]["version"].as_str().map(str::to_owned))
        .ok_or_else(|| tr!("it did not say its version", "no dijo su versión").to_owned())
}

fn probe_version(
    child: &mut impl ProbeProcess,
    clock: &mut impl ProbeClock,
) -> Result<String, String> {
    let deadline = clock.now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.poll().map_err(|error| error.to_string())? {
            break status;
        }
        if clock.now() >= deadline {
            let _ = child.kill();
            let _ = child.reap();
            return Err(tr!("it did not answer within 2 s", "no respondió en 2 s").to_owned());
        }
        clock.pause(Duration::from_millis(20));
    };
    probe_output(child, status)
}

/// The version a restart would run, using the two-second production probe policy.
pub fn disk_version(path: &Path) -> Result<String, String> {
    let mut child = spawn_probe(path)?;
    probe_version(&mut child, &mut RealProbeClock)
}

/// Replaces this process with `path`, same arguments, handing it the restore file. Returns only
/// on failure.
pub fn exec(path: &Path, restore: &Path) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(path)
        .args(std::env::args_os().skip(1))
        .env(super::restore::ENV, restore)
        .exec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    const NEWER: &str = "999.0.0";

    fn app() -> App {
        let (tx, _rx) = std::sync::mpsc::sync_channel(8);
        App::new(tx)
    }

    /// A daemon newer than this terminal, the same version on disk, and 10 s without a key.
    fn behind(app: &mut App) {
        app.service_seen(Service { version: NEWER.into(), views_protocol: alondra_core::views::VIEWS_PROTOCOLS });
        app.version_tick();
        app.disk_checked(NEWER.into(), Disk::Version(NEWER.into()));
        app.updated.last_key_at = now_millis() - version_watch::IDLE_MS;
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn an_idle_terminal_counts_down_and_esc_leaves_the_fixed_line_until_r() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let mut app = app();
        behind(&mut app);
        app.version_tick();
        assert_eq!(app.updated.notice.as_ref().map(|n| n.restarting), Some(true));
        assert!(!app.updated.restart);

        press(&mut app, KeyCode::Esc);
        app.version_tick();
        assert_eq!(
            app.updated.notice,
            Some(Notice { text: format!("Alondra was updated ({NEWER}): restart the TUI · R"), restarting: false })
        );
        app.updated.last_key_at = now_millis() - version_watch::IDLE_MS;
        app.version_tick();
        assert!(!app.updated.restart, "Esc stays until R");

        press(&mut app, KeyCode::Char('R'));
        app.version_tick();
        assert!(app.updated.restart);
    }

    #[test]
    fn r_waits_for_an_action_without_an_answer_and_restarts_when_it_comes() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let mut app = app();
        behind(&mut app);
        app.action_sent();
        app.version_tick();
        press(&mut app, KeyCode::Char('R'));
        app.version_tick();
        assert!(!app.updated.restart);
        assert_eq!(app.updated.notice.as_ref().map(|n| n.text.as_str()), Some("Waiting for the action to finish…"));
        assert!(app.holds_restart());

        app.action_answered();
        app.version_tick();
        assert!(app.updated.restart);
    }

    #[test]
    fn the_process_started_by_a_restart_never_restarts_again_for_that_version() {
        let mut app = app();
        let state = Restore {
            restarted_for: NEWER.into(),
            screen: "fleet".into(),
            schedules_open: false,
            rules_open: false,
            detail_focused: false,
            sel: None,
            list_scroll: 0,
            detail_scroll: 0,
            task_sel: None,
            task_scroll: 0,
            task_detail_scroll: 0,
            agent_sel: None,
            agent_scroll: 3,
            agent_detail_scroll: 0,
            composer: Some(Draft { target: super::super::Target::General { send: false }, text: "still here".into(), chips: vec![] }),
            drafts: vec![],
        };
        app.restore(state);
        assert!(app.screen == Screen::Fleet);
        assert_eq!(app.agent_scroll, 3);
        assert_eq!(app.composer.as_ref().map(|c| c.input.text()), Some("still here".into()));
        behind(&mut app);
        app.updated.countdown_until = Some(now_millis() - 1);
        app.version_tick();
        assert!(!app.updated.restart);
    }

    #[test]
    fn a_restart_runs_the_stable_link_of_each_kind_of_install() {
        let root = crate::test_support::Directory::new();
        let keg = root.join("homebrew/Cellar/alondra/0.4.0/bin");
        std::fs::create_dir_all(&keg).unwrap();
        std::fs::write(keg.join("alondra"), "").unwrap();
        let home = root.join("home");
        assert_eq!(stable_for(&keg.join("alondra"), &home), root.join("homebrew/opt/alondra/bin/alondra"));
        assert_eq!(stable_for(&home.join(".local/bin/alondra"), &home), home.join(".local/bin/alondra"));
        assert_eq!(stable_for(Path::new("/tmp/sandbox/bin/alondra"), &home), PathBuf::from("/tmp/sandbox/bin/alondra"));
    }

    #[derive(Debug, PartialEq, Eq)]
    enum ProbeEvent {
        Poll,
        Output,
        Kill,
        Reap,
        Pause(Duration),
    }

    struct FakeProbe {
        events: std::rc::Rc<std::cell::RefCell<Vec<ProbeEvent>>>,
        polls: std::collections::VecDeque<Option<std::process::ExitStatus>>,
        output: String,
        kill_error: bool,
    }
    impl ProbeProcess for FakeProbe {
        fn poll(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            self.events.borrow_mut().push(ProbeEvent::Poll);
            Ok(self.polls.pop_front().expect("scripted process poll"))
        }
        fn output(&mut self) -> String {
            self.events.borrow_mut().push(ProbeEvent::Output);
            self.output.clone()
        }
        fn kill(&mut self) -> std::io::Result<()> {
            self.events.borrow_mut().push(ProbeEvent::Kill);
            if self.kill_error {
                Err(std::io::Error::other("scripted kill error"))
            } else {
                Ok(())
            }
        }
        fn reap(&mut self) -> std::io::Result<std::process::ExitStatus> {
            self.events.borrow_mut().push(ProbeEvent::Reap);
            Ok(exit_status(9))
        }
    }
    struct FakeProbeClock {
        events: std::rc::Rc<std::cell::RefCell<Vec<ProbeEvent>>>,
        base: Instant,
        ticks: std::collections::VecDeque<Duration>,
    }
    impl ProbeClock for FakeProbeClock {
        fn now(&mut self) -> Instant {
            self.base + self.ticks.pop_front().expect("scripted clock read")
        }
        fn pause(&mut self, duration: Duration) {
            self.events.borrow_mut().push(ProbeEvent::Pause(duration));
        }
    }
    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }
    fn probe_fixture(
        polls: Vec<Option<std::process::ExitStatus>>,
        ticks: Vec<Duration>,
        output: &str,
    ) -> (FakeProbe, FakeProbeClock) {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        (
            FakeProbe {
                events: events.clone(),
                polls: polls.into(),
                output: output.into(),
                kill_error: false,
            },
            FakeProbeClock {
                events,
                base: Instant::now(),
                ticks: ticks.into(),
            },
        )
    }

    #[test]
    fn the_binary_on_disk_says_its_version_or_why_it_could_not() {
        let root = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*root).unwrap();
        let script = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let path = root.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let good = script("good", r#"echo '{"cli":{"version":"0.2.53"}}'"#);
        let mut child = spawn_probe(&good).unwrap();
        let status = child.reap().unwrap();
        assert_eq!(probe_output(&mut child, status), Ok("0.2.53".into()));
        let mut child =
            spawn_probe(&script("old", "echo 'error: unexpected argument'; exit 2")).unwrap();
        let status = child.reap().unwrap();
        assert!(probe_output(&mut child, status).is_err());
        let (mut child, mut clock) =
            probe_fixture(vec![None], vec![Duration::ZERO, Duration::from_secs(2)], "");
        assert!(probe_version(&mut child, &mut clock).is_err());
        assert!(disk_version(&root.join("missing")).is_err());
    }

    #[test]
    fn the_two_second_boundary_kills_then_reaps_even_when_kill_fails() {
        for kill_error in [false, true] {
            let (mut child, mut clock) =
                probe_fixture(vec![None], vec![Duration::ZERO, Duration::from_secs(2)], "");
            child.kill_error = kill_error;
            assert!(probe_version(&mut child, &mut clock).is_err());
            assert_eq!(
                *child.events.borrow(),
                [ProbeEvent::Poll, ProbeEvent::Kill, ProbeEvent::Reap]
            );
            assert!(clock.ticks.is_empty());
        }
    }

    #[test]
    fn pending_poll_keeps_the_twenty_millisecond_policy_without_real_sleep() {
        let (mut child, mut clock) = probe_fixture(
            vec![None, Some(exit_status(0))],
            vec![Duration::ZERO, Duration::from_millis(1999)],
            r#"{"cli":{"version":"0.2.53"}}"#,
        );
        assert_eq!(probe_version(&mut child, &mut clock), Ok("0.2.53".into()));
        assert_eq!(
            *child.events.borrow(),
            [
                ProbeEvent::Poll,
                ProbeEvent::Pause(Duration::from_millis(20)),
                ProbeEvent::Poll,
                ProbeEvent::Output
            ]
        );
    }

    #[test]
    fn a_completed_process_is_checked_before_the_deadline_and_json_is_required() {
        let (mut child, mut clock) = probe_fixture(
            vec![Some(exit_status(0))],
            vec![Duration::ZERO, Duration::from_secs(2)],
            r#"{"cli":{"version":"0.2.53"}}"#,
        );
        assert_eq!(probe_version(&mut child, &mut clock), Ok("0.2.53".into()));
        assert_eq!(clock.ticks, [Duration::from_secs(2)]);
        assert_eq!(
            *child.events.borrow(),
            [ProbeEvent::Poll, ProbeEvent::Output]
        );
        for output in ["not json", r#"{"cli":{}}"#, r#"{"cli":{"version":2}}"#] {
            let (mut child, mut clock) =
                probe_fixture(vec![Some(exit_status(0))], vec![Duration::ZERO], output);
            assert!(probe_version(&mut child, &mut clock).is_err());
        }
        let (mut child, mut clock) = probe_fixture(
            vec![Some(exit_status(2))],
            vec![Duration::ZERO],
            r#"{"cli":{"version":"0.2.53"}}"#,
        );
        assert!(probe_version(&mut child, &mut clock).is_err());
        assert_eq!(
            *child.events.borrow(),
            [ProbeEvent::Poll, ProbeEvent::Output]
        );
    }
}
