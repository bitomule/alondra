use crate::domain_runtime::DomainRuntime;
use super::*;
use alondra_domain_rules::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
use serde_json::{Value, json};
use std::sync::Mutex;

const ID: &str = "r-20261120-1a2b3c4d";
const SAVED: &str = "ana/informes";
const CURRENT: &str = "ana/otros";

struct World {
    home: crate::test_support::Directory,
    domain: Arc<DomainRuntime>,
    memory: Memory,
    /// Every `alondra` command the watcher ran.
    ran: Arc<Mutex<Vec<String>>>,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn world() -> World {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(crate::wiring::open(&home).unwrap());
    World { home, domain, memory: Memory::default(), ran: Arc::default() }
}

impl World {
    fn reports(&self) -> PathBuf {
        self.home.join("reports")
    }

    fn write(&self, sending: Value) {
        let dir = self.reports().join(ID);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sending.json"), sending.to_string()).unwrap();
    }

    fn held(&self, current: &str, refused: Option<&str>) {
        let mut held = json!({"reason": "repo_changed", "current": current});
        if let Some(refused) = refused {
            held["refused"] = json!(refused);
        }
        self.write(json!({"repo": SAVED, "state": "pending", "kind": "other", "package_sha256": "", "held": held}));
    }

    /// One check, with `repo` as the settings' report repository and `cli` as what each command does.
    fn check(&mut self, repo: Option<&str>, cli: impl Fn(&World, &[&str])) {
        let mut memory = std::mem::take(&mut self.memory);
        let ran = self.ran.clone();
        let run = |args: &[&str]| {
            ran.lock().unwrap().push(args.join(" "));
            cli(self, args);
        };
        check(&*self.domain, &self.reports(), repo.map(Into::into), &run, &mut memory);
        self.memory = memory;
    }

    fn open(&self) -> Vec<(InteractionId, ReportHeldCard)> {
        let mut open: Vec<_> = ours(&*self.domain).into_iter().filter(|card| card.answer.is_none()).map(|card| (card.id, card.card.unwrap())).collect();
        open.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        open
    }

    fn answer(&self, id: &InteractionId, option: &str) {
        let state = self.domain.state().unwrap();
        self.domain.execute(&Actor::User { user_id: "device:phone".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("answer-{option}").into(), expected_revision: state.revision,
            command: Command::AnswerQuestion { attachments: vec![], interaction_id: id.clone(), option_id: option.into(), inputs: Default::default() },
        }, None, || Ok(())).unwrap();
    }

    fn ran(&self) -> Vec<String> {
        self.ran.lock().unwrap().clone()
    }
}

fn nothing(_: &World, _: &[&str]) {}

fn card(refused: Option<&str>) -> ReportHeldCard {
    ReportHeldCard { report: ID.into(), saved: SAVED.into(), current: CURRENT.into(), refused: refused.map(Into::into) }
}

#[test]
fn a_held_report_opens_one_card_that_names_both_repositories() {
    let mut world = world();
    world.write(json!({"repo": SAVED, "state": "pending", "kind": "other", "package_sha256": ""}));
    world.check(None, nothing);
    assert!(world.open().is_empty(), "a pending report that is not held opens nothing");
    world.held(CURRENT, None);
    world.check(None, nothing);
    world.check(None, nothing);
    let open = world.open();
    assert_eq!(open.len(), 1, "one card, not one per check");
    assert_eq!(open[0].1, card(None));
    let state = world.domain.state().unwrap();
    let stored = &state.interactions[&open[0].0];
    assert_eq!(stored.system_card, Some(SystemCard::ReportHeld { report: ID.into() }));
    assert_eq!(stored.card.effective_title(), "Report r-20261120-1a2b3c4d is not being sent: report.repo changed");
    assert!(world.ran().is_empty(), "nothing is sent while the user has not chosen");
}

#[test]
fn each_held_report_has_its_own_card() {
    let mut world = world();
    world.held(CURRENT, None);
    let other = world.reports().join("r-20261120-0000000b");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("sending.json"), json!({"repo": SAVED, "state": "pending", "held": {"reason": "repo_changed", "current": CURRENT}}).to_string()).unwrap();
    world.check(None, nothing);
    world.check(None, nothing);
    let mut reports: Vec<String> = world.open().into_iter().map(|(_, card)| card.report).collect();
    reports.sort();
    assert_eq!(reports, ["r-20261120-0000000b", ID]);
}

#[test]
fn send_it_runs_the_send_that_confirms_the_new_repository_and_the_card_stays_closed() {
    let mut world = world();
    world.held(CURRENT, None);
    world.check(None, nothing);
    let (id, _) = world.open().remove(0);
    world.answer(&id, SEND_OPTION);
    world.check(Some(CURRENT), |world, _| world.write(json!({"repo": CURRENT, "state": "sent", "kind": "other", "package_sha256": ""})));
    assert_eq!(world.ran(), [format!("report send {ID}")]);
    assert!(world.open().is_empty());
    world.check(Some(CURRENT), nothing);
    assert_eq!(world.ran().len(), 1, "an answer is acted on once");
}

#[test]
fn a_send_github_refuses_brings_the_card_back_saying_why() {
    let mut world = world();
    world.held(CURRENT, None);
    world.check(None, nothing);
    let (id, _) = world.open().remove(0);
    world.answer(&id, SEND_OPTION);
    world.check(None, |world, _| world.held(CURRENT, Some("GitHub has no repository ana/otros that takes issues")));
    let open = world.open();
    assert_eq!(open.len(), 1);
    assert_ne!(open[0].0, id);
    assert_eq!(open[0].1, card(Some("GitHub has no repository ana/otros that takes issues")));
}

#[test]
fn keep_it_runs_the_keep_and_no_card_comes_back() {
    let mut world = world();
    world.held(CURRENT, None);
    world.check(None, nothing);
    let (id, _) = world.open().remove(0);
    world.answer(&id, KEEP_OPTION);
    world.check(None, |world, _| world.write(json!({"repo": SAVED, "state": "kept", "kind": "other", "package_sha256": ""})));
    assert_eq!(world.ran(), [format!("report keep {ID}")]);
    assert!(world.open().is_empty());
    world.check(None, nothing);
    assert!(world.open().is_empty());
}

#[test]
fn a_report_sent_another_way_withdraws_its_card() {
    let mut world = world();
    world.held(CURRENT, None);
    world.check(None, nothing);
    assert_eq!(world.open().len(), 1);
    world.write(json!({"repo": CURRENT, "state": "sent", "kind": "other", "package_sha256": ""}));
    world.check(None, nothing);
    assert!(world.open().is_empty());
    std::fs::remove_dir_all(world.reports().join(ID)).unwrap();
    world.check(None, nothing);
    assert!(world.open().is_empty() && world.ran().is_empty());
}

#[test]
fn the_repository_back_to_the_saved_one_tries_it_again_at_once_and_withdraws_the_card() {
    let mut world = world();
    world.held(CURRENT, None);
    world.check(None, nothing);
    assert_eq!(world.open().len(), 1);
    world.check(Some(SAVED), |world, _| world.write(json!({"repo": SAVED, "state": "sent", "kind": "other", "package_sha256": ""})));
    assert_eq!(world.ran(), [format!("report send {ID} --retry")]);
    assert!(world.open().is_empty());
}

#[test]
fn a_try_that_goes_on_by_itself_withdraws_the_card() {
    let mut world = world();
    world.held(CURRENT, None);
    world.check(None, nothing);
    world.write(json!({"repo": SAVED, "state": "pending", "kind": "other", "package_sha256": ""}));
    world.check(None, nothing);
    assert!(world.open().is_empty(), "the daemon's own try found the saved repository again");
}

#[test]
fn a_repository_changed_again_replaces_the_card_and_sends_nothing_nobody_confirmed() {
    let mut world = world();
    world.held(CURRENT, None);
    world.check(None, nothing);
    let (id, _) = world.open().remove(0);
    world.answer(&id, SEND_OPTION);
    world.check(Some("ana/terceros"), |world, args| if args.contains(&"--retry") { world.held("ana/terceros", None) });
    assert_eq!(world.ran(), [format!("report send {ID} --retry")], "the answer named ana/otros, so nothing goes to ana/terceros");
    let open = world.open();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].1.current, "ana/terceros");
    world.check(Some("ana/terceros"), nothing);
    assert_eq!(world.ran().len(), 1, "it asks for another try once per repository");
}

#[tokio::test]
async fn an_answer_an_earlier_daemon_saw_is_not_acted_on_again() {
    let world = world();
    world.held(CURRENT, None);
    raise(&*world.domain, &card(None));
    let (id, _) = world.open().remove(0);
    world.answer(&id, SEND_OPTION);
    let ran = world.ran.clone();
    let watcher = tokio::spawn(watch(world.domain.clone(), world.reports(), || None, move |args: &[&str]| ran.lock().unwrap().push(args.join(" ")), Duration::from_millis(10)));
    for _ in 0..300 {
        if world.open().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    watcher.abort();
    assert!(world.ran().is_empty(), "the user is asked again instead");
    assert_eq!(world.open().len(), 1);
}
