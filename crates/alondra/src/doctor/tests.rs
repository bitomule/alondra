use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use alondra_core::config::{Config, FileConfig};
use alondra_core::startup::Record;
use alondra_core::views::Lang;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use super::checks::*;
use super::system::{MacInfo, Memory, ModsPass, ServiceView, System, TestPush};
use super::ui::{Plain, Recorded};
use super::*;

fn english() {
    crate::i18n::set_for_this_thread(Lang::En);
}

fn local() -> Config {
    Config::resolve("/h".into(), &FileConfig { name: Some("Ana".into()), wake: Some(true), ..Default::default() }, |_| None, || None)
}

fn healthy() -> Value {
    json!({"version": "0.4.0", "integrity_issues": []})
}

fn running() -> ServiceView {
    ServiceView {
        label: "com.example.alondra".into(),
        print: Some("gui/501/com.example.alondra = {\n\tstate = running\n}".into()),
        binaries: true,
        bin: "/opt/homebrew/opt/alondra/bin".into(),
        repo: Some("/opt/homebrew/opt/alondra/libexec/alondra".into()),
        ..Default::default()
    }
}

/// A Mac where every check passes, which each test then breaks in one place. Repairs change
/// it the way the real ones change the Mac, and every call that writes is recorded.
struct Fake {
    config_text: RefCell<Option<String>>,
    mac: MacInfo,
    health: RefCell<Result<Value, String>>,
    devices: Vec<Value>,
    offline: Option<Result<(u64, usize), String>>,
    startup: Option<Record>,
    service: RefCell<ServiceView>,
    service_config: RefCell<bool>,
    relay: RefCell<bool>,
    options: RefCell<Option<String>>,
    cwd: PathBuf,
    apns: Option<String>,
    claude: BTreeMap<String, (bool, String)>,
    signed_in: RefCell<Result<bool, String>>,
    profile: Result<Option<String>, String>,
    plugins: RefCell<Option<Value>>,
    marketplace: RefCell<bool>,
    node_plugin: Option<PathBuf>,
    fingerprint: Option<String>,
    tools: BTreeMap<String, String>,
    port_owner: Option<String>,
    log: Vec<String>,
    memory: RefCell<Memory>,
    starts_failing: RefCell<u32>,
    push: Result<(), String>,
    applied: RefCell<Vec<Action>>,
    claude_calls: RefCell<Vec<String>>,
    remembered: RefCell<Vec<Memory>>,
    pushed: RefCell<Vec<String>>,
}

impl Fake {
    fn healthy() -> Self {
        let claude = [
            ("--version", (true, "2.1.291 (Claude Code)".to_string())),
            ("--help", (true, "--print --resume --output-format stream-json --permission-mode auto --permission-prompts".to_string())),
            ("plugin validate --strict /m/plugin", (true, String::new())),
            ("plugin test /m/plugin", (true, String::new())),
        ];
        Fake {
            config_text: RefCell::new(Some("name = \"Ana\"\nwake = true\n".into())),
            mac: MacInfo { os: "macOS 26.1".into(), arch: "Apple Silicon".into(), install: "Homebrew".into(), dev_over_brew: false },
            health: RefCell::new(Ok(healthy())),
            devices: vec![json!({"id": "phone-1", "name": "iPhone", "created_at": 1, "last_seen": 5})],
            offline: None,
            startup: None,
            service: RefCell::new(running()),
            service_config: RefCell::new(true),
            relay: RefCell::new(true),
            options: RefCell::new(Some("projects_root = \"/\"\n".into())),
            cwd: "/".into(),
            apns: None,
            claude: claude.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            signed_in: RefCell::new(Ok(true)),
            profile: Ok(None),
            plugins: RefCell::new(Some(json!([]))),
            marketplace: RefCell::new(true),
            node_plugin: Some("/m/plugin".into()),
            fingerprint: Some("fp-1".into()),
            tools: [("tmux", "tmux 3.5a"), ("tuicr", "tuicr 0.26.0")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            port_owner: None,
            log: (1..=12).map(|n| format!("line {n}")).collect(),
            memory: RefCell::new(Memory {
                mods: Some(ModsPass { fingerprint: "fp-1".into(), at: 0 }),
                test_push: Some(TestPush { device: "phone-1".into(), name: "iPhone".into(), at: 0 }),
            }),
            starts_failing: RefCell::new(0),
            push: Ok(()),
            applied: RefCell::new(vec![]),
            claude_calls: RefCell::new(vec![]),
            remembered: RefCell::new(vec![]),
            pushed: RefCell::new(vec![]),
        }
    }

    fn stopped(self) -> Self {
        *self.health.borrow_mut() = Err("connection refused".into());
        self.service.borrow_mut().print = None;
        self
    }

    fn writes(&self) -> usize {
        self.applied.borrow().len() + self.remembered.borrow().len() + self.pushed.borrow().len()
    }
}

impl System for Fake {
    fn home(&self) -> PathBuf {
        "/h/.alondra".into()
    }
    fn user_home(&self) -> PathBuf {
        "/h".into()
    }
    fn config_path(&self) -> PathBuf {
        "/h/.alondra/config.toml".into()
    }
    fn config_text(&self) -> Option<String> {
        self.config_text.borrow().clone()
    }
    fn config(&self) -> Config {
        local()
    }
    fn options_text(&self) -> Option<String> {
        self.options.borrow().clone()
    }
    fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }
    fn set_projects(&self, folder: &Path) -> Result<()> {
        self.applied.borrow_mut().push(Action::SetProjects);
        *self.options.borrow_mut() = Some(format!("projects_root = {:?}\n", folder.display().to_string()));
        Ok(())
    }
    fn exists(&self, _path: &Path) -> bool {
        true
    }
    fn mac(&self) -> MacInfo {
        self.mac.clone()
    }
    fn health(&self) -> Result<Value, String> {
        self.health.borrow().clone()
    }
    fn devices(&self) -> Option<Vec<Value>> {
        self.health.borrow().is_ok().then(|| self.devices.clone())
    }
    fn offline_audit(&self) -> Option<Result<(u64, usize), String>> {
        self.offline.clone()
    }
    fn startup(&self) -> Option<Record> {
        self.startup.clone()
    }
    fn service(&self) -> ServiceView {
        self.service.borrow().clone()
    }
    fn service_config(&self) -> bool {
        *self.service_config.borrow()
    }
    fn relay(&self) -> bool {
        *self.relay.borrow()
    }
    fn apns(&self) -> Option<String> {
        self.apns.clone()
    }
    fn claude(&self, args: &[&str], _timeout: Duration) -> Option<(bool, String)> {
        let key = args.join(" ");
        self.claude_calls.borrow_mut().push(key.clone());
        self.claude.get(&key).cloned()
    }
    fn signed_in(&self) -> Result<bool> {
        self.signed_in.borrow().clone().map_err(|e| anyhow!(e))
    }
    fn profile(&self) -> Result<Option<String>, String> {
        self.profile.clone()
    }
    fn plugins(&self) -> Option<Value> {
        self.plugins.borrow().clone()
    }
    fn marketplace(&self) -> bool {
        *self.marketplace.borrow()
    }
    fn node_plugin(&self) -> Option<PathBuf> {
        self.node_plugin.clone()
    }
    fn fingerprint(&self, _dir: &Path) -> Option<String> {
        self.fingerprint.clone()
    }
    fn tool(&self, program: &str, _args: &[&str]) -> Option<String> {
        self.tools.get(program).cloned()
    }
    fn port_owner(&self, _port: u16) -> Option<String> {
        self.port_owner.clone()
    }
    fn log_tail(&self, lines: usize) -> Vec<String> {
        self.log[self.log.len().saturating_sub(lines)..].to_vec()
    }
    fn memory(&self) -> Memory {
        self.memory.borrow().clone()
    }
    fn remember(&self, memory: &Memory) {
        self.remembered.borrow_mut().push(memory.clone());
        *self.memory.borrow_mut() = memory.clone();
    }
    fn now(&self) -> i64 {
        86_400_000
    }
    fn apply(&self, action: Action) -> Result<()> {
        self.applied.borrow_mut().push(action);
        match action {
            Action::InstallService | Action::ReplaceOtherService | Action::RestartService => {
                if *self.starts_failing.borrow() > 0 {
                    *self.starts_failing.borrow_mut() -= 1;
                    return Err(anyhow!("Alondra didn't start."));
                }
                self.config_text.borrow_mut().get_or_insert_with(String::new);
                let mut service = self.service.borrow_mut();
                service.print = running().print;
                service.pending = false;
                service.others.clear();
                *self.health.borrow_mut() = Ok(healthy());
            }
            Action::PrepareIdentity => *self.service_config.borrow_mut() = true,
            Action::RepairPlugin => {
                *self.marketplace.borrow_mut() = true;
                *self.plugins.borrow_mut() = Some(json!([]));
                self.service.borrow_mut().marketplace_elsewhere = None;
            }
            Action::SignIn => *self.signed_in.borrow_mut() = Ok(true),
            Action::EnableRelay => *self.relay.borrow_mut() = true,
            Action::SetProjects => {}
        }
        Ok(())
    }
    fn test_push(&self, device: &str) -> Result<()> {
        self.pushed.borrow_mut().push(device.to_string());
        self.push.clone().map_err(|e| anyhow!(e))
    }
}

fn env(sys: &Fake) -> Env<'_> {
    Env { sys, deep: false, first_run: false, yes: false, select: Selection::All }
}

fn find<'a>(outcomes: &'a [Outcome], id: &str) -> &'a Outcome {
    outcomes.iter().find(|o| o.id == id).unwrap_or_else(|| panic!("no {id} in {outcomes:#?}"))
}

#[test]
fn every_check_has_a_unique_id_and_reports_under_it() {
    let mut ids: Vec<&str> = checks::CHECKS.iter().map(|c| c.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), checks::CHECKS.len());
    let sys = Fake::healthy();
    for check in checks::CHECKS {
        if let Some(outcome) = (check.run)(&env(&sys)) {
            assert_eq!(outcome.id, check.id);
        }
    }
}

#[test]
fn a_healthy_mac_passes_every_check_and_exits_zero() {
    let sys = Fake::healthy();
    let outcomes = diagnose(&env(&sys));
    let broken: Vec<_> = outcomes.iter().filter(|o| o.status != Status::Pass).collect();
    assert!(broken.is_empty(), "{broken:#?}");
    assert_eq!(exit_code(&outcomes), 0);
    assert_eq!(sys.writes(), 0, "a check never writes");
}

#[test]
fn the_service_check_names_the_repair_for_each_state() {
    english();
    assert_eq!(service_check(&running()).status, Status::Pass);
    let unloaded = ServiceView { print: None, ..running() };
    let c = service_check(&unloaded);
    assert_eq!((c.status, c.fix), (Status::Fail, Some(Fix::Auto(Action::InstallService))));
    let outdated = ServiceView { pending: true, ..running() };
    let c = service_check(&outdated);
    assert_eq!((c.status, c.fix), (Status::Warn, Some(Fix::Auto(Action::InstallService))));
    let stopped = ServiceView { print: Some("state = not running".into()), ..running() };
    assert_eq!(service_check(&stopped).fix, Some(Fix::Auto(Action::RestartService)));
    let missing = ServiceView { binaries: false, ..running() };
    let c = service_check(&missing);
    assert_eq!((c.status, c.fix), (Status::Fail, Some(Fix::Manual("brew reinstall alondra".into()))));
    assert!(c.detail.contains("alondrad is missing"), "{}", c.detail);
}

#[test]
fn another_service_is_replaced_only_after_a_yes_that_defaults_to_no() {
    english();
    let other = ServiceView { others: vec!["/h/Library/LaunchAgents/dev.alondra.alondrad.plist".into()], ..running() };
    let c = service_check(&other);
    assert_eq!(c.status, Status::Fail);
    let Some(Fix::Ask { question, default, action, instead }) = c.fix else { panic!("{c:?}") };
    assert!(question.contains("(dev.alondra.alondrad)"), "{question}");
    assert!(!default);
    assert_eq!(action, Action::ReplaceOtherService);
    assert!(instead.contains("alondra doctor --fix"), "{instead}");
}

#[test]
fn a_development_build_never_repairs_a_homebrew_service() {
    english();
    for view in [ServiceView { print: None, dev_over_brew: true, ..running() },
                 ServiceView { others: vec!["/x.plist".into()], dev_over_brew: true, ..running() }] {
        let c = service_check(&view);
        assert!(matches!(&c.fix, Some(Fix::Manual(text)) if text.contains("make e2e-sandbox")), "{c:?}");
    }
    let outdated = ServiceView { pending: true, dev_over_brew: true, ..running() };
    assert_eq!(service_check(&outdated).status, Status::Pass, "a dev build's own paths say nothing about the Homebrew service");
    let dev = MacInfo { dev_over_brew: true, ..Default::default() };
    assert_eq!(mac_check(&dev).status, Status::Warn);
}

#[test]
fn the_daemon_check_restarts_installs_or_names_the_port_owner() {
    english();
    let l = local();
    let same = json!({"name": "Ana", "port": 47800, "listen": "127.0.0.1", "public_url": null, "legacy_bridge": false, "wake": true});
    assert!(drift(&same, &l).is_empty());
    let restart = Some(Fix::Auto(Action::RestartService));
    assert_eq!(daemon_check(Ok(json!({"version": "0.1.0", "config": same})), &l, restart.clone(), None).status, Status::Pass);
    let stale = json!({"name": "Bea", "port": 47800, "listen": "127.0.0.1", "public_url": null, "legacy_bridge": false, "wake": true});
    assert_eq!(drift(&stale, &l), vec!["name: \"Bea\" ≠ \"Ana\""]);
    let c = daemon_check(Ok(json!({"config": stale})), &l, restart.clone(), None);
    assert_eq!((c.status, c.fix), (Status::Warn, restart.clone()));
    let c = daemon_check(Err("no responde".into()), &l, restart.clone(), None);
    assert_eq!((c.status, c.fix), (Status::Fail, restart.clone()));
    let c = daemon_check(Err("no responde".into()), &l, restart, Some("nginx"));
    assert_eq!(c.status, Status::Fail);
    assert!(c.detail.contains("Port 47800 is in use by nginx"), "{}", c.detail);
    assert!(matches!(c.fix, Some(Fix::Manual(t)) if t.contains("lsof -iTCP:47800")));
}

#[test]
fn the_daemon_check_installs_the_service_launchd_does_not_have() {
    let sys = Fake::healthy().stopped();
    let outcomes = diagnose(&env(&sys));
    assert_eq!(find(&outcomes, "alondrad").fix, Some(Fix::Auto(Action::InstallService)));
    sys.service.borrow_mut().print = running().print;
    assert_eq!(find(&diagnose(&env(&sys)), "alondrad").fix, Some(Fix::Auto(Action::RestartService)));
}

#[test]
fn the_plugin_check_repairs_what_setup_would() {
    english();
    let repo = Path::new("/src/alondra");
    let global = json!([{"id": "alondra@alondra", "version": "0.1.2", "scope": "user", "enabled": true}]);
    let c = plugin_check(Some(&global), true, Some(repo), None);
    assert_eq!((c.status, c.fix.clone()), (Status::Warn, Some(Fix::Auto(Action::RepairPlugin))), "a global install reaches sessions Alondra did not launch");
    assert!(c.detail.contains("alondra doctor --fix turns it off"), "{}", c.detail);
    assert_eq!(plugin_check(Some(&json!([{"id": "alondra@alondra", "enabled": false}])), true, Some(repo), None).status, Status::Pass);
    assert_eq!(plugin_check(Some(&json!([])), false, Some(repo), None).fix, Some(Fix::Auto(Action::RepairPlugin)));
    assert!(matches!(plugin_check(Some(&json!([])), false, None, None).fix, Some(Fix::Manual(_))), "without the repo there is nothing to register");
    let elsewhere = plugin_check(Some(&json!([])), true, Some(repo), Some(Path::new("/old/alondra")));
    assert_eq!((elsewhere.status, elsewhere.fix), (Status::Warn, Some(Fix::Auto(Action::RepairPlugin))));
}

#[test]
fn claude_sign_in_is_a_question_and_the_profile_is_shown_without_account_data() {
    english();
    assert_eq!(signin_check(Ok(true)).status, Status::Pass);
    let c = signin_check(Ok(false));
    assert_eq!(c.status, Status::Fail);
    let Some(Fix::Ask { question, default, action, instead }) = c.fix else { panic!("{c:?}") };
    assert_eq!(question, "Sign in to Claude Code now? It opens your browser.");
    assert!(default);
    assert_eq!(action, Action::SignIn);
    assert_eq!(instead, "claude auth login, then alondra doctor --fix");
    assert!(matches!(signin_check(Err(anyhow!("timed out"))).fix, Some(Fix::Manual(_))));
    let home = Path::new("/Users/ana");
    assert_eq!(profile_check(Ok(None), home).detail, "Claude profile: default");
    assert_eq!(profile_check(Ok(Some("/Users/ana/.claude-work".into())), home).detail, "Claude profile: ~/.claude-work (CLAUDE_CONFIG_DIR)");
    assert_eq!(profile_check(Err("disagree".into()), home).status, Status::Fail);
}

#[test]
fn notifications_need_the_relay_and_a_test_the_device_accepted() {
    english();
    let devices = [json!({"id": "phone-1"})];
    let tested = TestPush { device: "phone-1".into(), name: "iPhone".into(), at: 0 };
    let off = notifications_check(false, Some(&tested), Some(&devices));
    assert_eq!((off.status, off.detail.as_str()), (Status::Warn, "off — local network only"));
    assert_eq!(off.fix, Some(Fix::Manual("alondra relay on".into())));
    let never = notifications_check(true, None, Some(&devices));
    assert_eq!((never.status, never.fix), (Status::Warn, Some(Fix::Manual("alondra doctor --test-push".into()))));
    assert_eq!(notifications_check(true, Some(&tested), Some(&devices)).status, Status::Pass);
    assert_eq!(notifications_check(true, Some(&tested), Some(&[json!({"id": "other"})])).status, Status::Warn, "the tested device was removed");
    assert_eq!(notifications_check(true, Some(&tested), None).status, Status::Pass, "with the daemon down the last test still counts");
}

#[test]
fn an_older_install_reports_its_own_apns_key_without_claiming_delivery() {
    english();
    let apns = r#"{"key_path": "/k.p8", "key_id": "K", "team_id": "T", "topic": "a.b"}"#;
    let c = apns_file_check(Some(apns), |_| true);
    assert_eq!(c.status, Status::Pass);
    assert!(c.detail.contains("never tested"), "{}", c.detail);
    assert_eq!(apns_file_check(Some(apns), |_| false).status, Status::Fail);
    assert_eq!(apns_file_check(Some("{}"), |_| true).status, Status::Fail);
    let mut sys = Fake::healthy();
    *sys.service_config.borrow_mut() = false;
    sys.apns = Some(apns.into());
    let outcomes = diagnose(&env(&sys));
    assert_eq!(find(&outcomes, "notifications").status, Status::Pass);
    assert_eq!(find(&outcomes, "identity").fix, Some(Fix::Auto(Action::PrepareIdentity)));
}

#[test]
fn devices_identity_config_and_tools() {
    english();
    assert_eq!(devices_check(&[json!({"id": "a"}), json!({"id": "b"})]).detail, "2 paired");
    assert_eq!(devices_check(&[]).fix, Some(Fix::Manual("open Alondra on your iPhone".into())));
    assert_eq!(identity_check(true).status, Status::Pass);
    assert_eq!(identity_check(false).fix, Some(Fix::Auto(Action::PrepareIdentity)));
    let p = Path::new("/h/config.toml");
    let install = Some(Fix::Auto(Action::InstallService));
    assert_eq!(config_check(p, None, install.clone()).fix, install);
    assert_eq!(config_check(p, Some("name = \"Ana\""), install.clone()).status, Status::Pass);
    assert!(matches!(config_check(p, Some("nmae = 1"), install).fix, Some(Fix::Manual(_))));
    assert!(hooks_port_check(alondra_core::DEFAULT_PORT).is_none());
    assert_eq!(hooks_port_check(47897).map(|c| c.status), Some(Status::Warn));
    assert_eq!(tmux_check(None, true).status, Status::Fail);
    assert_eq!(tmux_check(None, false).status, Status::Warn);
    assert_eq!(tuicr_check(Some("tuicr 0.26.0")).status, Status::Pass);
    assert_eq!(tuicr_check(Some("tuicr 1.2.0\n")).status, Status::Pass);
    assert_eq!(tuicr_check(Some("tuicr 0.18.3")).status, Status::Warn);
    assert_eq!(tuicr_check(None).fix, Some(Fix::Manual("brew install tuicr".into())));
}

#[test]
fn claude_must_be_new_enough_and_able_to_orchestrate() {
    english();
    assert_eq!(claude_version_check(Some("2.1.287 (Claude Code)\n")).detail, "2.1.287 (Claude Code)");
    let old = claude_version_check(Some("2.1.280 (Claude Code)\n"));
    assert_eq!((old.status, old.fix), (Status::Fail, Some(Fix::Manual("claude update".into()))), "nodes need mods");
    assert!(old.detail.contains("2.1.287"), "{}", old.detail);
    assert_eq!(claude_version_check(None).status, Status::Fail);
    let help = "--print --resume --output-format stream-json --permission-mode auto --permission-prompts";
    assert_eq!(orchestrator_capability_check(Some(help)).status, Status::Pass);
    assert_eq!(orchestrator_capability_check(Some(&help.replace("auto", "default"))).status, Status::Fail);
    assert_eq!(orchestrator_capability_check(None).status, Status::Fail);
}

#[test]
fn the_node_mod_must_validate_and_pass_its_tests() {
    english();
    let plugin = Path::new("/r/plugin");
    let ok = || Some((true, String::new()));
    assert_eq!(mods_check(Some(plugin), ok(), ok()).status, Status::Pass);
    let invalid = mods_check(Some(plugin), Some((false, "❯ author: No author\n✘ Validation failed".into())), ok());
    assert_eq!((invalid.status, invalid.fix), (Status::Fail, Some(Fix::Manual("claude plugin validate --strict /r/plugin".into()))));
    assert!(invalid.detail.contains("Validation failed"), "{}", invalid.detail);
    let off = mods_check(Some(plugin), ok(), Some((false, "claude plugin test: hooks modules are turned off: disableAllHooks".into())));
    assert_eq!((off.status, off.fix), (Status::Fail, Some(Fix::Manual("claude plugin test /r/plugin".into()))));
    assert!(off.detail.contains("hooks modules are turned off"), "{}", off.detail);
    assert_eq!(mods_check(Some(plugin), ok(), None).status, Status::Fail);
    assert_eq!(mods_check(None, ok(), ok()).fix, Some(Fix::Manual("brew reinstall alondra".into())));
}

#[test]
fn the_slow_mod_check_runs_only_with_deep_or_a_changed_plugin() {
    english();
    let sys = Fake::healthy();
    let skipped = find(&diagnose(&env(&sys)), "mods").clone();
    assert_eq!(skipped.status, Status::Pass);
    assert!(skipped.detail.contains("--deep checks again"), "{}", skipped.detail);
    assert!(!sys.claude_calls.borrow().iter().any(|c| c.starts_with("plugin")), "{:?}", sys.claude_calls.borrow());

    let deep = Env { sys: &sys, deep: true, first_run: false, yes: false, select: Selection::All };
    assert_eq!(find(&diagnose(&deep), "mods").detail, "the node mod validates and its tests pass");
    assert!(sys.claude_calls.borrow().contains(&"plugin test /m/plugin".to_string()));

    let mut changed = Fake::healthy();
    changed.fingerprint = Some("fp-2".into());
    assert_eq!(find(&diagnose(&env(&changed)), "mods").detail, "the node mod validates and its tests pass");
    assert_eq!(changed.memory().mods.map(|m| m.fingerprint), Some("fp-2".into()), "a pass is remembered");

    let mut failing = Fake::healthy();
    failing.fingerprint = Some("fp-3".into());
    failing.claude.insert("plugin test /m/plugin".into(), (false, "boom".into()));
    assert_eq!(find(&diagnose(&env(&failing)), "mods").status, Status::Fail);
    assert!(failing.remembered.borrow().is_empty(), "a failure is never remembered as a pass");
}

#[test]
fn the_fingerprint_changes_with_any_file_of_the_plugin() {
    let root = crate::test_support::Directory::new();
    std::fs::create_dir_all(root.join("hooks")).unwrap();
    std::fs::write(root.join("plugin.json"), "{}").unwrap();
    std::fs::write(root.join("hooks/stop.sh"), "echo a").unwrap();
    let first = system::fingerprint(&root).unwrap();
    assert_eq!(system::fingerprint(&root), Some(first.clone()));
    std::fs::write(root.join("hooks/stop.sh"), "echo b").unwrap();
    assert_ne!(system::fingerprint(&root), Some(first));
}

#[test]
fn startup_update_and_upgrade_reports() {
    english();
    let home = Path::new("/h");
    let record = Record { failures: 3, last_at: 1, reason: "bootstrap failed integrity audit: t1".into(), version: "0.1.12".into() };
    assert!(startup_check(Some(&record), true, home).is_none());
    let down = startup_check(Some(&record), false, home).unwrap();
    assert_eq!(down.status, Status::Fail);
    assert!(down.detail.contains("bootstrap failed integrity audit: t1") && down.detail.contains('3'), "{}", down.detail);
    let health = |update: Value| json!({"version": "0.1.12", "update": update});
    assert!(update_check(&json!({"version": "0.1.12"})).is_none());
    let newer = update_check(&health(json!({"current": "0.1.12", "latest": "0.1.13", "available": true}))).unwrap();
    assert_eq!((newer.status, newer.fix), (Status::Warn, Some(Fix::Manual("alondra update".into()))));
    assert_eq!(update_check(&health(json!({"current": "0.1.13", "latest": "0.1.13", "available": false}))).unwrap().status, Status::Pass);
    assert!(preflight_check(&json!({"version": "0.1.12", "preflight_rejected": null})).is_none());
    let warned = preflight_check(&json!({"version": "0.1.12", "preflight_rejected": {"version": "0.1.13", "reason": "preflight failed: audit"}})).unwrap();
    assert!(warned.detail.contains("0.1.13") && warned.detail.contains("preflight failed: audit"), "{}", warned.detail);
    assert_eq!(state_integrity_check(&json!({"integrity_issues": ["t1 broken"]})).status, Status::Fail);
    assert_eq!(offline_state_check(Ok((81_233, 4))).status, Status::Pass);
}

#[test]
fn fix_installs_a_stopped_service_once_and_a_second_run_changes_nothing() {
    english();
    let sys = Fake::healthy().stopped();
    let mut ui = Recorded::default();
    let repaired = repair(&env(&sys), &mut ui).unwrap();
    assert_eq!(repaired.applied, vec![Action::InstallService], "{:?}", ui.lines);
    assert_eq!(*sys.applied.borrow(), vec![Action::InstallService], "one install for the service and the daemon");
    assert!(repaired.outcomes.iter().all(|o| o.status == Status::Pass), "{:#?}", repaired.outcomes);
    assert_eq!(outro(&env(&sys), &repaired), "Everything is set.");
    assert_eq!(exit_code(&repaired.outcomes), 0);

    let before = sys.writes();
    let mut again = Recorded::default();
    let second = repair(&env(&sys), &mut again).unwrap();
    assert!(second.applied.is_empty());
    assert_eq!(sys.writes(), before, "a second run writes nothing");
    assert_eq!(outro(&env(&sys), &second), "Nothing to change.");
    assert!(again.lines.iter().any(|l| l == "✓ This Mac: already set"), "{:?}", again.lines);
}

#[test]
fn fix_applies_each_automatic_repair_and_checks_again() {
    english();
    let sys = Fake::healthy();
    *sys.service_config.borrow_mut() = false;
    *sys.marketplace.borrow_mut() = false;
    let mut ui = Recorded::default();
    let repaired = repair(&env(&sys), &mut ui).unwrap();
    assert_eq!(repaired.applied, vec![Action::PrepareIdentity, Action::RepairPlugin]);
    assert_eq!(find(&repaired.outcomes, "identity").status, Status::Pass);
    assert_eq!(find(&repaired.outcomes, "plugin").status, Status::Pass);
    assert!(ui.lines.iter().any(|l| l == "progress Setting up the Claude plugin…"), "{:?}", ui.lines);
}

#[test]
fn plain_doctor_never_repairs() {
    let sys = Fake::healthy().stopped();
    let outcomes = diagnose(&env(&sys));
    assert!(sys.applied.borrow().is_empty());
    assert_eq!(exit_code(&outcomes), 4);
}

#[test]
fn a_question_answered_no_changes_nothing_and_waits_for_the_person() {
    english();
    let sys = Fake::healthy();
    *sys.signed_in.borrow_mut() = Ok(false);
    let mut ui = Recorded { answers: vec![Some(false)], ..Default::default() };
    let repaired = repair(&env(&sys), &mut ui).unwrap();
    assert!(sys.applied.borrow().is_empty());
    assert!(ui.lines.contains(&"ask Sign in to Claude Code now? It opens your browser.".to_string()), "{:?}", ui.lines);
    assert_eq!(exit_code(&repaired.outcomes), 3);
    assert_eq!(outro(&env(&sys), &repaired), "Not finished.\nNext: claude auth login, then alondra doctor --fix");
}

#[test]
fn a_question_answered_yes_applies_its_repair() {
    let sys = Fake::healthy();
    *sys.signed_in.borrow_mut() = Ok(false);
    let mut ui = Recorded { answers: vec![Some(true)], ..Default::default() };
    let repaired = repair(&env(&sys), &mut ui).unwrap();
    assert_eq!(*sys.applied.borrow(), vec![Action::SignIn]);
    assert_eq!(exit_code(&repaired.outcomes), 0);
}

#[test]
fn yes_accepts_every_question_and_the_proposed_folder_without_asking() {
    english();
    let root = crate::test_support::Directory::new();
    let cwd = root.join("Projects");
    folder(&cwd, 2);
    let sys = new_mac(&cwd);
    let yes = Env { yes: true, ..wizard_env(&sys) };
    let mut ui = Recorded::default();
    let repaired = repair(&yes, &mut ui).unwrap();
    assert!(!ui.lines.iter().any(|l| l.starts_with("ask ") || l.starts_with("select ")), "{:?}", ui.lines);
    assert_eq!(*sys.applied.borrow(), vec![Action::SetProjects, Action::PrepareIdentity, Action::InstallService, Action::EnableRelay]);
    assert_eq!(*sys.options.borrow(), Some(format!("projects_root = {:?}\n", cwd.display().to_string())));
    assert_eq!(exit_code(&repaired.outcomes), 0);
}

#[test]
fn yes_without_a_person_leaves_the_browser_sign_ins_for_one() {
    english();
    let sys = Fake::healthy();
    *sys.signed_in.borrow_mut() = Ok(false);
    let yes = Env { yes: true, ..env(&sys) };
    let repaired = repair(&yes, &mut Plain { out: vec![] }).unwrap();
    assert!(sys.applied.borrow().is_empty());
    assert_eq!(exit_code(&repaired.outcomes), 3);

    let mut ui = Recorded::default();
    repair(&yes, &mut ui).unwrap();
    assert_eq!(*sys.applied.borrow(), vec![Action::SignIn], "with a person at the terminal --yes signs in: {:?}", ui.lines);
}

#[test]
fn only_and_skip_run_just_the_checks_they_name() {
    english();
    let sys = Fake::healthy();
    *sys.service_config.borrow_mut() = false;
    *sys.marketplace.borrow_mut() = false;
    let only = Env { select: Selection::Only(vec!["plugin".into()]), ..env(&sys) };
    let ids: Vec<&str> = diagnose(&only).iter().map(|o| o.id).collect();
    assert_eq!(ids, ["plugin"]);
    let repaired = repair(&only, &mut Recorded::default()).unwrap();
    assert_eq!(repaired.applied, vec![Action::RepairPlugin], "identity is not repaired outside --only");

    let skip = Env { select: Selection::Skip(vec!["identity".into(), "mods".into()]), ..env(&sys) };
    let ids: Vec<&str> = diagnose(&skip).iter().map(|o| o.id).collect();
    assert!(!ids.contains(&"identity") && !ids.contains(&"mods") && ids.contains(&"service"), "{ids:?}");
    assert!(check_ids().contains(&"service") && check_ids().contains(&"relay"));
}

#[test]
fn without_a_terminal_nothing_is_asked_and_json_says_the_next_step() {
    english();
    let sys = Fake::healthy();
    *sys.signed_in.borrow_mut() = Ok(false);
    let mut out = vec![];
    let repaired = repair(&env(&sys), &mut Plain { out: &mut out }).unwrap();
    assert!(sys.applied.borrow().is_empty());
    assert_eq!(exit_code(&repaired.outcomes), 3);
    let doc = report(&repaired.outcomes, None);
    assert_eq!(doc["next_step"], "claude auth login, then alondra doctor --fix");
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("✗ Claude Code sign-in: not signed in\n  → claude auth login, then alondra doctor --fix"), "{text}");
    assert!(text.contains("✓ Claude Code: 2.1.291"), "{text}");
}

#[test]
fn a_service_that_does_not_start_shows_why_and_offers_to_try_again() {
    english();
    let mut sys = Fake::healthy().stopped();
    sys.startup = Some(Record { failures: 3, last_at: 1, reason: "database migration 41 failed".into(), version: "0.4.0".into() });
    *sys.starts_failing.borrow_mut() = 1;
    let mut ui = Recorded { choices: vec![Some(0)], ..Default::default() };
    let repaired = repair(&env(&sys), &mut ui).unwrap();
    assert_eq!(*sys.applied.borrow(), vec![Action::InstallService, Action::InstallService]);
    assert!(ui.lines.contains(&"note Last start failed 3 times: database migration 41 failed".to_string()), "{:?}", ui.lines);
    assert!(ui.lines.contains(&"note   line 12".to_string()) && !ui.lines.contains(&"note   line 2".to_string()), "the last 10 lines: {:?}", ui.lines);
    assert_eq!(exit_code(&repaired.outcomes), 0);

    let sys = Fake::healthy().stopped();
    *sys.starts_failing.borrow_mut() = 1;
    let mut ui = Recorded { choices: vec![Some(2)], ..Default::default() };
    let stopped = repair(&env(&sys), &mut ui).unwrap();
    assert_eq!(*sys.applied.borrow(), vec![Action::InstallService]);
    assert_eq!(exit_code(&stopped.outcomes), 4);
}

#[test]
fn exit_codes_put_a_silent_daemon_first_then_breakage_then_a_person() {
    let o = |id: &'static str, status, fix| Outcome { id, name: id, group: Group::Service, status, detail: String::new(), fix };
    let ask = || Some(Fix::Ask { question: String::new(), default: true, action: Action::SignIn, instead: String::new() });
    let down = o("alondrad", Status::Fail, Some(Fix::Auto(Action::RestartService)));
    let broken = o("tmux", Status::Fail, Some(Fix::Manual("brew install tmux".into())));
    let person = o("claude.signin", Status::Fail, ask());
    let warning = o("update", Status::Warn, None);
    assert_eq!(exit_code(&[down.clone(), broken.clone(), person.clone()]), 4);
    assert_eq!(exit_code(&[broken.clone(), person.clone()]), 1);
    assert_eq!(exit_code(&[person, warning.clone()]), 3);
    assert_eq!(exit_code(&[warning]), 0);
}

#[test]
fn json_keeps_todays_fields_and_adds_schema_id_fixable_and_next_step() {
    english();
    let sys = Fake::healthy().stopped();
    let doc = report(&diagnose(&env(&sys)), None);
    assert_eq!(doc["schema"], "alondra.doctor/1");
    assert_eq!(doc["ok"], false);
    let daemon = doc["checks"].as_array().unwrap().iter().find(|c| c["id"] == "alondrad").unwrap();
    assert_eq!(daemon["check"], "alondrad");
    assert_eq!(daemon["status"], "fail");
    assert_eq!(daemon["detail"], "connection refused");
    assert_eq!(daemon["fix"], "alondra doctor --fix");
    assert_eq!(daemon["fixable"], true);
    let tmux = doc["checks"].as_array().unwrap().iter().find(|c| c["id"] == "tmux").unwrap();
    assert_eq!((tmux["fix"].as_str(), tmux["fixable"].as_bool()), (Some(""), Some(false)));
    assert_eq!(doc["next_step"], "alondra doctor --fix");
    assert_eq!(report(&diagnose(&env(&Fake::healthy())), None)["next_step"], Value::Null);
    let target = json!({"program": "/opt/homebrew/opt/alondra/bin/alondrad", "profile_mode": "implicit"});
    let doc = report(&diagnose(&env(&Fake::healthy())), Some(target.clone()));
    let checks = doc["checks"].as_array().unwrap();
    assert_eq!(checks.iter().find(|c| c["id"] == "service").unwrap()["install"], target);
    assert!(checks.iter().filter(|c| c["id"] != "service").all(|c| c.get("install").is_none()));
}

#[test]
fn test_push_goes_to_the_device_used_most_recently_and_is_remembered() {
    english();
    let mut sys = Fake::healthy();
    sys.devices = vec![
        json!({"id": "ipad", "name": "iPad", "created_at": 9, "last_seen": 3}),
        json!({"id": "phone-1", "name": "iPhone", "created_at": 1, "last_seen": 7}),
        json!({"id": "new", "name": "Mac", "created_at": 5, "last_seen": null}),
    ];
    *sys.memory.borrow_mut() = Memory::default();
    let sent = test_push(&env(&sys));
    assert_eq!(sent.code, 0);
    assert_eq!(*sys.pushed.borrow(), vec!["phone-1".to_string()]);
    assert_eq!(sent.detail, "Test notification sent to “iPhone”. Check that it arrived.");
    assert_eq!(sys.memory().test_push.map(|t| (t.device, t.at)), Some(("phone-1".into(), 86_400_000)));
    assert_eq!(find(&diagnose(&env(&sys)), "notifications").status, Status::Pass);
}

#[test]
fn test_push_exit_codes() {
    let no_relay = Fake::healthy();
    *no_relay.relay.borrow_mut() = false;
    let r = test_push(&env(&no_relay));
    assert_eq!((r.code, r.next.as_deref()), (3, Some("alondra relay on")));
    assert!(no_relay.pushed.borrow().is_empty());
    assert_eq!(test_push(&env(&Fake::healthy().stopped())).code, 4);
    let mut alone = Fake::healthy();
    alone.devices.clear();
    assert_eq!(test_push(&env(&alone)).code, 3);
    let mut refused = Fake::healthy();
    refused.push = Err("device has no active push binding".into());
    let r = test_push(&env(&refused));
    assert_eq!((r.code, r.detail.as_str()), (1, "device has no active push binding"));
    assert!(refused.remembered.borrow().is_empty());
}

#[test]
fn grouped_output_aligns_details_and_fixes_in_one_column() {
    english();
    let sys = Fake::healthy();
    sys.memory.borrow_mut().test_push = None;
    let lines = render::grouped("Alondra 0.4.0 on Ana's Mac", &diagnose(&env(&sys)));
    assert_eq!(lines[0], "Alondra 0.4.0 on Ana's Mac");
    for group in ["Service", "Claude", "Devices", "Tools"] {
        assert!(lines.contains(&group.to_string()), "{lines:#?}");
    }
    assert!(lines.contains(&"  ✓ tmux · tuicr".to_string()), "{lines:#?}");
    assert_eq!(lines.last().unwrap(), "1 warning. Nothing is broken.");
    let column = |needle: &str| lines.iter().find_map(|l| l.find(needle).map(|at| l[..at].chars().count())).unwrap_or_else(|| panic!("{needle}: {lines:#?}"));
    assert_eq!(column("never tested on a device"), column("→ alondra doctor --test-push"));
    assert_eq!(column("never tested on a device"), column("answers on port 47800"));
}

#[test]
fn the_summary_counts_problems_and_warnings() {
    english();
    let o = |status| Outcome { id: "x", name: "x", group: Group::Tools, status, detail: String::new(), fix: None };
    assert_eq!(render::summary(&[o(Status::Pass)]), "Nothing is broken.");
    assert_eq!(render::summary(&[o(Status::Warn), o(Status::Warn)]), "2 warnings. Nothing is broken.");
    assert_eq!(render::summary(&[o(Status::Fail), o(Status::Warn)]), "1 problem, 1 warning.");
    crate::i18n::set_for_this_thread(Lang::Es);
    assert_eq!(render::summary(&[o(Status::Fail), o(Status::Fail)]), "2 problemas.");
}

#[test]
fn the_day_of_a_timestamp() {
    assert_eq!(day(alondra_core::now_millis()).len(), 10);
}

fn repo(dir: &Path) {
    std::fs::create_dir_all(dir.join(".git")).unwrap();
}

fn folder(path: &Path, repos: usize) {
    std::fs::create_dir_all(path).unwrap();
    for n in 0..repos {
        repo(&path.join(format!("app-{n}")));
    }
}

fn paths(proposal: &projects::Proposal) -> Vec<(PathBuf, usize)> {
    proposal.folders.iter().map(|f| (f.path.clone(), f.repos)).collect()
}

#[test]
fn the_cwd_with_repos_is_proposed_alone() {
    let root = crate::test_support::Directory::new();
    let (home, cwd) = (root.join("home"), root.join("home/Projects"));
    folder(&cwd, 3);
    let proposal = projects::propose(&cwd, &home);
    assert_eq!(paths(&proposal), vec![(cwd, 3)]);
    assert_eq!(proposal.refused, None);
}

#[test]
fn from_home_the_subfolder_with_most_repos_comes_second() {
    english();
    let root = crate::test_support::Directory::new();
    let home = root.join("home");
    folder(&home.join("Projects"), 3);
    folder(&home.join("Other"), 1);
    folder(&home.join("Documents"), 0);
    let proposal = projects::propose(&home, &home);
    assert_eq!(paths(&proposal), vec![(home.clone(), 0), (home.join("Projects"), 3)]);
    assert_eq!(projects::label(&proposal.folders[0], &home), "~ · no repos at the top level");
    assert_eq!(projects::label(&proposal.folders[1], &home), "~/Projects · 3 repos");
}

#[test]
fn inside_one_repo_its_parent_comes_second() {
    let root = crate::test_support::Directory::new();
    let home = root.join("home");
    folder(&home.join("Projects"), 4);
    let cwd = home.join("Projects/app-0");
    assert_eq!(paths(&projects::propose(&cwd, &home)), vec![(cwd.clone(), 0), (home.join("Projects"), 4)]);
}

#[test]
fn a_folder_without_repos_gets_the_best_one_in_home() {
    let root = crate::test_support::Directory::new();
    let home = root.join("home");
    folder(&home.join("code"), 2);
    let cwd = home.join("Downloads");
    folder(&cwd, 0);
    assert_eq!(paths(&projects::propose(&cwd, &home)), vec![(cwd, 0), (home.join("code"), 2)]);
}

#[test]
fn a_folder_outside_home_is_accepted_and_only_the_root_is_refused() {
    let root = crate::test_support::Directory::new();
    let (home, outside) = (root.join("home"), root.join("Volumes/work"));
    folder(&home, 0);
    folder(&outside, 2);
    assert_eq!(paths(&projects::propose(&outside, &home)), vec![(outside, 2)]);
    let refused = projects::propose(Path::new("/"), &home);
    assert!(refused.folders.is_empty(), "nothing is the default for /");
    assert!(refused.refused.is_some());
}

/// A Mac set up nowhere: no config, no service, no identity, no projects folder, no relay.
fn new_mac(cwd: &Path) -> Fake {
    let mut sys = Fake::healthy().stopped();
    *sys.config_text.borrow_mut() = None;
    *sys.service_config.borrow_mut() = false;
    *sys.relay.borrow_mut() = false;
    *sys.options.borrow_mut() = None;
    sys.devices.clear();
    sys.cwd = cwd.to_path_buf();
    sys
}

fn wizard_env(sys: &Fake) -> Env<'_> {
    Env { sys, deep: false, first_run: true, yes: false, select: Selection::All }
}

fn in_order(lines: &[String], expected: &[&str]) {
    let mut at = 0;
    for want in expected {
        match lines[at..].iter().position(|l| l.starts_with(want)) {
            Some(found) => at += found + 1,
            None => panic!("{want:?} missing or out of order in {lines:#?}"),
        }
    }
}

#[test]
fn the_wizard_asks_for_the_folder_and_the_relay_in_the_order_of_the_screens() {
    english();
    let root = crate::test_support::Directory::new();
    let cwd = root.join("Projects");
    folder(&cwd, 2);
    let sys = new_mac(&cwd);
    let mut ui = Recorded { choices: vec![Some(0)], answers: vec![Some(true)], ..Default::default() };
    let repaired = repair(&wizard_env(&sys), &mut ui).unwrap();
    in_order(&ui.lines, &[
        "✓ This Mac: macOS 26.1 · Apple Silicon · Homebrew",
        "✓ Claude Code: 2.1.291",
        "✓ Claude profile: default",
        "✓ Claude Code sign-in: signed in",
        "select Projects folder — where your git repos live. You can change it later in the app.",
        "✓ Projects folder: ",
        "progress Starting the background service…",
        "✓ Background service: running on port 47800",
        "✓ Claude plugin: already set",
        "✓ Ready for your devices",
        "ask Turn on notifications and access away from home? It needs an Alondra account and opens your browser.",
        "✓ Notifications and access away from home: on",
    ]);
    assert!(ui.lines.iter().any(|l| l.contains(&format!("{} · 2 repos", cwd.display()))), "{:?}", ui.lines);
    assert!(!ui.lines.iter().any(|l| l.contains("Devices")), "the wizard waits for a device instead: {:?}", ui.lines);
    assert_eq!(*sys.applied.borrow(), vec![Action::SetProjects, Action::PrepareIdentity, Action::InstallService, Action::EnableRelay]);
    assert_eq!(*sys.options.borrow(), Some(format!("projects_root = {:?}\n", cwd.display().to_string())));
    assert_eq!(exit_code(&repaired.outcomes), 0);
    assert_eq!(outro(&wizard_env(&sys), &repaired), "Alondra is running. Open it on your iPhone or on this Mac.");
}

#[test]
fn no_to_the_relay_leaves_the_local_network_and_says_how_to_turn_it_on() {
    english();
    let root = crate::test_support::Directory::new();
    let cwd = root.join("Projects");
    folder(&cwd, 1);
    let sys = new_mac(&cwd);
    let mut ui = Recorded { choices: vec![Some(0)], answers: vec![Some(false)], ..Default::default() };
    let repaired = repair(&wizard_env(&sys), &mut ui).unwrap();
    assert!(ui.lines.contains(&"! Notifications and access away from home: off — local network only → alondra relay on".to_string()), "{:?}", ui.lines);
    assert!(!sys.applied.borrow().contains(&Action::EnableRelay));
    assert_eq!(exit_code(&repaired.outcomes), 0, "local network only is a choice, not a failure");
}

#[test]
fn another_folder_can_be_typed_and_the_root_or_a_file_is_refused() {
    english();
    let root = crate::test_support::Directory::new();
    let cwd = root.join("home");
    folder(&cwd, 0);
    std::fs::write(cwd.join("notes.txt"), "keep").unwrap();
    let sys = new_mac(&cwd);
    let mut ui = Recorded {
        choices: vec![Some(2)],
        typed: vec![Some("/".into()), Some("notes.txt".into()), Some("work".into())],
        answers: vec![Some(true)],
        ..Default::default()
    };
    repair(&wizard_env(&sys), &mut ui).unwrap();
    assert!(ui.lines.iter().any(|l| l.starts_with("note / can't be the projects folder")), "{:?}", ui.lines);
    assert!(ui.lines.iter().any(|l| l.contains("notes.txt is not a directory")), "{:?}", ui.lines);
    assert_eq!(*sys.options.borrow(), Some(format!("projects_root = {:?}\n", cwd.join("work").display().to_string())));
}

#[test]
fn from_the_root_there_is_no_default_and_the_folder_is_typed() {
    english();
    let root = crate::test_support::Directory::new();
    let typed = root.join("Projects");
    folder(&typed, 0);
    let sys = new_mac(Path::new("/"));
    let mut ui = Recorded { typed: vec![Some(typed.display().to_string())], answers: vec![Some(true)], ..Default::default() };
    repair(&wizard_env(&sys), &mut ui).unwrap();
    assert!(!ui.lines.iter().any(|l| l.starts_with("select Projects folder")), "no list to accept by mistake: {:?}", ui.lines);
    assert!(ui.lines.iter().any(|l| l.starts_with("note / can't be the projects folder")), "{:?}", ui.lines);
    assert_eq!(*sys.options.borrow(), Some(format!("projects_root = {:?}\n", typed.display().to_string())));
}

#[test]
fn without_a_terminal_the_cwd_is_used_and_said() {
    english();
    let root = crate::test_support::Directory::new();
    let cwd = root.join("Projects");
    folder(&cwd, 0);
    let sys = new_mac(&cwd);
    *sys.config_text.borrow_mut() = Some(String::new());
    let mut out = vec![];
    repair(&env(&sys), &mut Plain { out: &mut out }).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains(&format!("  projects folder: {} (cwd)", cwd.display())), "{text}");
    assert_eq!(*sys.options.borrow(), Some(format!("projects_root = {:?}\n", cwd.display().to_string())));
}

#[test]
fn a_saved_folder_is_kept_and_one_that_is_gone_is_asked_again() {
    english();
    let root = crate::test_support::Directory::new();
    let (home, saved) = (root.join("home"), root.join("home/Projects"));
    folder(&saved, 2);
    let file = Path::new("/h/.alondra/options.toml");
    let text = format!("projects_root = {:?}\n", saved.display().to_string());
    let kept = projects::projects_check(Some(&text), file, &home, &home, true);
    assert_eq!((kept.status, kept.detail.as_str()), (Status::Pass, "~/Projects (2 repos)"), "a valid saved folder is never asked again");
    let gone = projects::projects_check(Some("projects_root = \"/nowhere-alondra\"\n"), file, &home, &home, false);
    assert_eq!(gone.status, Status::Fail);
    assert!(matches!(gone.fix, Some(Fix::Choose { .. })));
    assert_eq!(gone.next_step(), "alondra config set projects_root <folder>");
    let unset = projects::projects_check(None, file, &home, &home, false);
    assert_eq!(unset.status, Status::Pass, "a configured Mac without one runs on ~/Projects");
    assert_eq!(projects::projects_check(None, file, &home, &home, true).status, Status::Fail, "a first install always asks");
}

/// The shape of the Mac this was written on: Homebrew, the `com.bitomule.alondra` label, a
/// projects folder, the relay on and twelve devices.
fn like_this_mac(projects: &Path) -> Fake {
    let mut sys = Fake::healthy();
    sys.service.borrow_mut().label = "com.bitomule.alondra".into();
    sys.service.borrow_mut().print = Some("gui/501/com.bitomule.alondra = {\n\tstate = running\n}".into());
    *sys.options.borrow_mut() = Some(format!("projects_root = {:?}\n", projects.display().to_string()));
    sys.devices = (1..=12).map(|n| json!({"id": format!("d-{n}"), "name": format!("Device {n}"), "created_at": n})).collect();
    sys.memory.borrow_mut().test_push = Some(TestPush { device: "d-1".into(), name: "Device 1".into(), at: 0 });
    sys
}

#[test]
fn an_install_like_this_macs_has_nothing_to_change() {
    english();
    let root = crate::test_support::Directory::new();
    let projects = root.join("Projects");
    folder(&projects, 3);
    let sys = like_this_mac(&projects);
    let mut ui = Recorded::default();
    let repaired = repair(&env(&sys), &mut ui).unwrap();
    assert_eq!(outro(&env(&sys), &repaired), "Nothing to change.");
    assert_eq!(sys.writes(), 0, "{:?}", sys.applied.borrow());
    assert!(!ui.lines.iter().any(|l| l.starts_with("ask") || l.starts_with("select") || l.starts_with("input")), "{:?}", ui.lines);
    assert_eq!(ui.lines, vec![
        "✓ This Mac: already set",
        "✓ Claude Code: 2.1.291",
        "✓ Claude Code sign-in: signed in",
        "✓ Background service: running (com.bitomule.alondra)",
        "✓ Claude plugin: already set",
        "✓ Devices: 12 paired · away from home: on",
    ]);
}

#[test]
fn every_check_belongs_to_exactly_one_step() {
    for check in checks::CHECKS {
        let steps: Vec<&str> = wizard::steps().iter().filter(|s| s.checks.contains(&check.id)).map(|s| s.name).collect();
        assert_eq!(steps.len(), 1, "{} is in {steps:?}", check.id);
    }
}

#[test]
fn alondra_opens_the_wizard_only_on_a_mac_never_set_up_with_someone_to_ask() {
    let never = || -> bool { panic!("a Mac never set up is not asked about its daemon") };
    assert_eq!(launch_decision(false, true, false, never, || None), Launch::Wizard);
    assert_eq!(launch_decision(false, false, false, never, || None), Launch::NotSetUp);
    assert_eq!(launch_decision(false, true, true, never, || None), Launch::NotSetUp, "a node never gets the wizard");
    assert_eq!(launch_decision(true, true, false, || true, || None), Launch::Tui);
    assert_eq!(launch_decision(true, true, false, || false, || None), Launch::OfferRepair);
    assert_eq!(launch_decision(true, true, false, || false, || Some("disk full".into())), Launch::StartFailed("disk full".into()));
    assert_eq!(launch_decision(true, false, false, || false, || None), Launch::Down);
}

#[test]
fn set_up_means_a_config_or_a_launch_agent_for_alondrad() {
    let root = crate::test_support::Directory::new();
    let (home, agents) = (root.join(".alondra"), root.join("LaunchAgents"));
    assert!(!configured(&home, Some(&agents)), "nothing at all");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("com.other.plist"), "<string>/usr/bin/true</string>").unwrap();
    assert!(!configured(&home, Some(&agents)), "another app's agent");
    std::fs::write(agents.join("com.bitomule.alondra.plist"), "<string>/opt/homebrew/opt/alondra/bin/alondrad</string>").unwrap();
    assert!(configured(&home, Some(&agents)));
    assert!(!configured(&home, None), "the sandbox looks only at its own home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.toml"), "name = \"MacBook Pro\"\n").unwrap();
    assert!(configured(&home, None));
}

struct Phone {
    pending: RefCell<Vec<devices_wait::Request>>,
    expired: Vec<String>,
    decisions: RefCell<Vec<(String, bool)>>,
}

impl devices_wait::Pairings for Phone {
    fn pending(&self) -> Vec<devices_wait::Request> {
        self.pending.borrow().clone()
    }
    fn decide(&self, id: &str, allow: bool) -> devices_wait::Decided {
        self.decisions.borrow_mut().push((id.into(), allow));
        self.pending.borrow_mut().retain(|r| r.id != id);
        if self.expired.iter().any(|e| e == id) { devices_wait::Decided::Expired } else { devices_wait::Decided::Done }
    }
}

struct Clock {
    now: RefCell<Duration>,
    enter_at: Option<Duration>,
}

impl devices_wait::Clock for Clock {
    fn elapsed(&self) -> Duration {
        *self.now.borrow()
    }
    fn pause(&self, how_long: Duration) {
        *self.now.borrow_mut() += how_long;
    }
    fn finished(&self) -> bool {
        self.enter_at.is_some_and(|at| *self.now.borrow() >= at)
    }
}

fn request(id: &str, name: &str) -> devices_wait::Request {
    devices_wait::Request { id: id.into(), name: name.into(), code: "482913".into() }
}

fn phone(requests: Vec<devices_wait::Request>, expired: &[&str]) -> Phone {
    Phone { pending: RefCell::new(requests), expired: expired.iter().map(|s| s.to_string()).collect(), decisions: RefCell::new(vec![]) }
}

#[test]
fn each_request_is_shown_alone_with_its_code_and_only_a_yes_allows_it() {
    english();
    let phone = phone(vec![request("a", "iPhone de Ana"), request("b", "iPad")], &[]);
    let clock = Clock { now: RefCell::new(Duration::ZERO), enter_at: Some(Duration::from_secs(10)) };
    let mut ui = Recorded { answers: vec![Some(true), Some(false)], ..Default::default() };
    let allowed = devices_wait::wait(&phone, &mut ui, &clock).unwrap();
    assert_eq!(allowed, 1);
    assert_eq!(*phone.decisions.borrow(), vec![("a".to_string(), true), ("b".to_string(), false)]);
    in_order(&ui.lines, &[
        "progress Waiting for your iPhone… open Alondra on it. Press Enter to finish.",
        "ask Allow “iPhone de Ana”? Code 482 913 — allow only if it shows the same code.",
        "ask Allow “iPad”? Code 482 913 — allow only if it shows the same code.",
    ]);
}

#[test]
fn an_expired_request_says_so_and_the_wait_goes_on() {
    english();
    let phone = phone(vec![request("old", "iPhone")], &["old"]);
    let clock = Clock { now: RefCell::new(Duration::ZERO), enter_at: Some(Duration::from_secs(3)) };
    let mut ui = Recorded { answers: vec![Some(true)], ..Default::default() };
    assert_eq!(devices_wait::wait(&phone, &mut ui, &clock).unwrap(), 0);
    assert!(ui.lines.contains(&"note That request expired — ask again from the device.".to_string()), "{:?}", ui.lines);
    assert_eq!(ui.lines.last().map(String::as_str), Some("progress Waiting for your iPhone… open Alondra on it. Press Enter to finish."));
}

#[test]
fn after_a_minute_without_requests_the_network_hint_shows_and_five_minutes_end_the_wait() {
    english();
    let phone = phone(vec![], &[]);
    let clock = Clock { now: RefCell::new(Duration::ZERO), enter_at: None };
    let mut ui = Recorded::default();
    devices_wait::wait(&phone, &mut ui, &clock).unwrap();
    let hints: Vec<_> = ui.lines.iter().filter(|l| l.starts_with("note Not showing up?")).collect();
    assert_eq!(hints.len(), 1, "{:?}", ui.lines);
    assert_eq!(*clock.now.borrow(), devices_wait::LIMIT);
}
