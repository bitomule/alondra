//! The four list screens and the task actions, rendered from views the daemon's own builder makes
//! for the terminal, the way the owner sees them.

use super::*;
use alondra_core::domain::{Agent, DomainState, Request, Task, TaskState};
use alondra_core::views::{self, Client, Lang, LiveNode, LiveSnapshot, RenderContext, Transport};
use std::collections::{BTreeMap, BTreeSet};

const NIGHTLY: &str = "Por qué se atascó la tarea nocturna";
const ODD_STATE: &str = "Estado raro del nodo Parcel";
const WEEKLY: &str = "Parcel · mejora semanal";

/// Opens Finished the way Enter on its header does, with the rows the daemon sends for it.
fn open_finished(app: &mut App, domain: &DomainState, live: &LiveSnapshot) {
    app.open_sections.insert("finished".into());
    app.on_msg(Msg::ViewSection {
        section: "finished".into(),
        result: Ok(build(domain, live, "tasks?section=finished&after=0")),
    });
}

pub(super) fn task_screen(domain: &DomainState, live: &LiveSnapshot, request: &str, task: &str) -> App {
    let mut app = app_on(Screen::Tasks, build(domain, live, "tasks"));
    open_finished(&mut app, domain, live);
    let row = if app.inbox_keys().iter().any(|key| key.0 == task) { task } else { request };
    app.sel = Some(Key(row.into()));
    app.on_msg(Msg::ViewCard {
        key: format!("task:{task}"),
        result: Ok(build(domain, live, &format!("task:{task}"))),
    });
    app
}

fn iso(millis: i64) -> String {
    let seconds = millis.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_adj = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_adj + 2) / 5 + 1;
    let month = if month_adj < 10 { month_adj + 3 } else { month_adj - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000+00:00",
        second_of_day / 3_600,
        second_of_day % 3_600 / 60,
        second_of_day % 60
    )
}

pub(super) fn minutes_ago(minutes: i64) -> String {
    iso(alondra_core::now_millis() - minutes * 60_000)
}

fn task(id: &str, request: &str, agent: Option<&str>, title: &str, state: TaskState) -> Task {
    Task {
        id: id.into(),
        request_id: request.into(),
        agent_id: agent.map(Into::into),
        title: title.into(),
        state,
        revision: 1,
        started: state != TaskState::NotStarted,
        review_prs: false,
        final_presentation: None,
        pr_id: None,
        later_prs: vec![],
        opened_prs: Default::default(),
        dependencies: BTreeSet::new(),
        stack: None,
        documents: BTreeMap::new(),
        archived: false,
        model: None,
        waiting_for_tasks: Default::default(), ended_task_waits: Default::default(), delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: Default::default(), priority: Default::default(), ending: None, stalls: 0, stall_closing: None, open_pr: None, continues: None, worktrees: Default::default(), provider: Default::default(), selection: None, pending_selection: None, account: None, allowed: Default::default(), project: None,
    }
}

/// Four nodes with one task each, three finished and one working, plus a scheduled task nobody
/// has started: the owner's nodes on 2026-10-03.
pub(super) fn busy() -> (DomainState, LiveSnapshot) {
    let mut domain = DomainState::new("screens");
    domain.revision = 1;
    let mut live = LiveSnapshot::default();
    for (index, (title, state)) in [
        (NIGHTLY, TaskState::Done),
        ("Permisos y pantalla del orquestador", TaskState::Done),
        ("Badge del inbox en push y al abrir", TaskState::Done),
        (ODD_STATE, TaskState::Running),
    ]
    .into_iter()
    .enumerate()
    {
        let (request, agent, task_id) = (format!("r-{index}"), format!("a-{index}"), format!("t-{index}"));
        let mut body = Request::new(request.as_str().into(), format!("{title}\n\nDetalles."));
        body.closed = state == TaskState::Done;
        domain.requests.insert(request.as_str().into(), body);
        domain.agents.insert(
            agent.as_str().into(),
            Agent { id: agent.as_str().into(), name: title.into(), since: 0, phase: match state {
                TaskState::Done => alondra_core::domain::NodePhase::Closing,
                _ => alondra_core::domain::NodePhase::Working,
            } },
        );
        domain
            .tasks
            .insert(task_id.as_str().into(), task(&task_id, &request, Some(&agent), title, state));
        live.nodes.push(LiveNode {
            agent_id: agent.clone(),
            short_id: Some(format!("n{index}")),
            name: title.into(),
            project: Some("alondra".into()),
            working: state == TaskState::Running,
            state_since: Some(minutes_ago(5 + index as i64 * 30)),
            ..LiveNode::default()
        });
        live.request_times.insert(request, minutes_ago(5 + index as i64 * 30));
    }
    domain.requests.insert("r-weekly".into(), Request::new("r-weekly".into(), WEEKLY.into()));
    domain
        .tasks
        .insert("t-weekly".into(), task("t-weekly", "r-weekly", None, WEEKLY, TaskState::NotStarted));
    live.request_times.insert("r-weekly".into(), minutes_ago(14 * 60));
    (domain, live)
}

pub(super) fn build(domain: &DomainState, live: &LiveSnapshot, key: &str) -> views::View {
    views::build(
        key,
        RenderContext {
            domain,
            index: &alondra_core::domain::ReadIndex::build(&domain),
            live,
            client: Client::Tui,
            lang: Lang::Es,
            transport: Transport::Loopback,
            cursor: "c",
            rendered_at: "",
        },
    )
    .unwrap()
}

pub(super) fn app_on(screen: Screen, home: views::View) -> App {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    let mut app = App::new(tx);
    app.screen = screen;
    app.on_msg(Msg::ViewHome(Ok(home)));
    app.reconcile();
    app
}

fn rendered(app: &mut App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn dump(name: &str, screen: &str) {
    println!("===== {name} =====\n{screen}");
}

#[test]
fn empty_inbox_shows_one_empty_state_and_no_empty_sections() {
    let domain = DomainState::new("screens");
    let live = LiveSnapshot::default();
    let mut app = app_on(Screen::Inbox, build(&domain, &live, "home"));
    let screen = rendered(&mut app, 100, 16);
    dump("inbox (empty)", &screen);
    assert!(screen.contains("Todo al día"), "{screen}");
    for section in ["TE ESPERA", "SOLICITUDES DE CAMBIOS", "PULL REQUESTS", "POR LEER"] {
        assert!(!screen.contains(section), "{section}: {screen}");
    }
}

#[test]
fn tab_alternates_inbox_and_tasks_and_no_screen_or_command_is_the_fleet() {
    let (domain, live) = busy();
    let mut app = app_on(Screen::Inbox, build(&domain, &live, "home"));
    let screen = rendered(&mut app, 120, 24);
    dump("inbox header", &screen);
    assert!(screen.contains("Bandeja") && screen.contains("Tareas"), "{screen}");
    assert!(!screen.contains("Flota"), "{screen}");
    press(&mut app, KeyCode::Tab);
    assert!(app.screen == Screen::Tasks);
    press(&mut app, KeyCode::Tab);
    assert!(app.screen == Screen::Inbox);
    app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    assert!(app.screen == Screen::Tasks, "Shift+Tab goes the other way round the same two screens");
    for command in ["fleet", "f", "agents"] {
        app.flash = None;
        app.on_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        for c in command.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.screen == Screen::Tasks, ":{command}");
        assert!(app.flash.as_ref().is_some_and(|flash| flash.text.contains("No es un comando")), ":{command}");
    }
}

#[test]
fn tasks_list_one_row_per_single_task_request_with_people_times() {
    let (domain, live) = busy();
    let mut app = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
    open_finished(&mut app, &domain, &live);
    let screen = rendered(&mut app, 100, 24);
    dump("tasks", &screen);
    for title in [ODD_STATE, WEEKLY] {
        assert_eq!(screen.matches(title).count(), 1, "{title}: {screen}");
    }
    assert!(!screen.contains("↳"), "{screen}");
    assert!(!screen.contains("· 0"), "{screen}");
    assert!(!screen.contains("+00:00") && !screen.contains("T0") && !screen.contains("T1"), "{screen}");
    assert!(screen.contains("hace 5 min"), "{screen}");
}

#[test]
fn the_tasks_list_is_three_blocks_with_waiting_for_another_in_waiting_and_names_a_new_request_by_its_first_sentence() {
    let mut app = app_on(Screen::Tasks, fixture_view("tasks-tui.json"));
    let screen = rendered(&mut app, 120, 34);
    dump("tasks (blocks)", &screen);
    let lines: Vec<&str> = screen.lines().collect();
    let at = |needle: &str| lines.iter().position(|line| line.contains(needle)).unwrap_or_else(|| panic!("{needle}: {screen}"));
    assert!(at("FOR YOU 1") < at("IN PROGRESS 3") && at("IN PROGRESS 3") < at("WAITING 5"), "{screen}");
    assert!(!screen.contains("ACTIVE"), "the blocks replace the Active title: {screen}");
    assert_eq!(at("New starts wait: the Mac is short of memory"), at("WAITING 5") + 1, "said once, under the header: {screen}");
    assert!(!screen.contains("% free") && screen.matches("memory").count() == 1, "{screen}");
    let waiting = at("Decrement the badge on retraction");
    assert!(waiting > at("WAITING 5"), "{screen}");
    assert!(lines[waiting..waiting + 2].iter().any(|line| line.contains("Waits for")), "it says what it waits for: {screen}");
    assert!(!screen.contains("waiting for this") && !screen.contains("from: "), "{screen}");
    for loose in ["Review the schedule editor", "Write the release notes"] {
        assert!(lines[at(loose)].contains(loose), "{screen}");
    }
    assert!(screen.contains("Import lists from Notes"), "{screen}");
    assert!(!screen.contains("Keep the order") && !screen.contains("Look at the schedule editor"), "{screen}");
    assert!(!screen.contains("Launch two more"), "a message to the orchestrator without tasks is not listed: {screen}");
}

/// 13 October: a task read as in review with no card left to review, after the release task's
/// deliveries were accepted. The terminal never says it waits for you when no card does.
#[test]
fn a_task_in_review_without_a_card_is_not_for_you_in_tasks() {
    let (mut domain, mut live) = busy();
    domain.tasks.get_mut(&"t-3".into()).unwrap().state = TaskState::ForYourReview;
    domain.agents.get_mut(&"a-3".into()).unwrap().phase = alondra_core::domain::NodePhase::Delivered { in_turn: false };
    live.nodes.iter_mut().for_each(|node| node.working = false);
    assert!(alondra_core::domain::ReadIndex::build(&domain).waiting().is_empty(), "no card waits");
    let mut tasks = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
    let screen = rendered(&mut tasks, 120, 30);
    dump("tasks (in review, no card)", &screen);
    assert!(!screen.contains("PARA TI") && !screen.contains("Para revisar"), "{screen}");
    assert!(screen.contains(ODD_STATE) && screen.contains("Entregada"), "{screen}");
}

#[test]
fn completed_starts_folded_and_enter_on_the_header_opens_and_folds_it() {
    let (mut domain, live) = busy();
    domain.tasks.get_mut(&"t-weekly".into()).unwrap().state = TaskState::Done;
    let mut app = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
    let folded = rendered(&mut app, 100, 30);
    dump("tasks (finished folded)", &folded);
    assert!(folded.contains("▸ COMPLETADAS"), "{folded}");
    assert!(!folded.contains(WEEKLY), "{folded}");
    let finished = app.home_view.as_ref().unwrap().sections.iter().find(|section| section.id == "finished").unwrap();
    assert!(finished.items.is_empty(), "the list view only says how many finished requests there are");

    let header = app.inbox_keys().iter().position(|key| key.0 == "section:finished").unwrap();
    app.move_inbox(header as isize);
    press(&mut app, KeyCode::Enter);
    app.on_msg(Msg::ViewSection {
        section: "finished".into(),
        result: Ok(build(&domain, &live, "tasks?section=finished&after=0")),
    });
    let open = rendered(&mut app, 100, 30);
    dump("tasks (finished open)", &open);
    assert!(open.contains("▾ COMPLETADAS") && open.contains(WEEKLY), "{open}");

    press(&mut app, KeyCode::Char('o'));
    let again = rendered(&mut app, 100, 30);
    assert!(again.contains("▸ COMPLETADAS") && !again.contains(WEEKLY), "{again}");
}

/// A task whose node kept stopping closes without a delivery: the terminal says so, with what
/// got done, and never as a card in the inbox.
#[test]
fn a_task_closed_because_its_node_kept_stopping_says_so_with_its_result() {
    let (mut domain, live) = busy();
    domain.requests.insert("r-stop".into(), Request::new("r-stop".into(), "Renombrar el saludo".into()));
    let mut stopped = task("t-stop", "r-stop", None, "Renombrar el saludo", TaskState::Cancelled);
    stopped.stall_closing = Some(alondra_core::domain::StallClosing::Settled { result: "Se leyó greeting.txt; no se renombró nada.".into() });
    domain.tasks.insert("t-stop".into(), stopped);
    let mut app = task_screen(&domain, &live, "r-stop", "t-stop");
    let screen = rendered(&mut app, 130, 30);
    dump("task closed because its node kept stopping", &screen);
    assert!(screen.contains("Cerrada sin entrega") && screen.contains("no se renombró nada"), "{screen}");
    assert!(!screen.contains("sin entregar ni preguntar"), "{screen}");
}

#[test]
fn task_actions_have_unique_keys_and_hide_what_does_not_apply() {
    let (domain, live) = busy();
    let mut app = task_screen(&domain, &live, "r-3", "t-3");
    let screen = rendered(&mut app, 130, 30);
    dump("task actions (working task)", &screen);
    assert!(screen.contains(" r  Mensaje") && screen.contains(" x  Cancelar tarea"), "{screen}");
    assert!(!screen.contains("Reabrir") && !screen.contains("Archivar ·"), "{screen}");
    assert!(!screen.contains("ya no está activa"), "{screen}");
    for (task, request) in [("t-3", "r-3"), ("t-0", "r-0"), ("t-weekly", "r-weekly")] {
        let actions = view_render::actions(&build(&domain, &live, &format!("task:{task}")));
        assert!(actions.iter().all(|action| action.enabled), "{task}: {actions:?}");
        let keys: Vec<&str> = actions.iter().map(view_render::action_hint).collect();
        assert!(keys.iter().all(|key| !key.is_empty()), "{task}: {keys:?}");
        assert_eq!(keys.iter().collect::<BTreeSet<_>>().len(), keys.len(), "{task}: {keys:?}");
        let mut app = task_screen(&domain, &live, request, task);
        let footer = draw::footer_keys(&app);
        for key in &keys {
            assert!(
                footer.iter().any(|(shown, _)| *shown == if key.chars().all(|c| c.is_ascii_uppercase()) { format!("Shift+{key}") } else { key.to_string() }),
                "{task}: {key} missing from {footer:?}"
            );
        }
        if task == "t-0" {
            dump("task actions (finished task)", &rendered(&mut app, 130, 30));
        }
    }
}

#[test]
fn task_keys_message_the_node_and_capitals_work_with_shift() {
    let (domain, live) = busy();
    let mut app = task_screen(&domain, &live, "r-3", "t-3");
    press(&mut app, KeyCode::Char('r'));
    assert!(
        matches!(
            app.composer.as_ref().map(|composer| &composer.target),
            Some(Target::ViewAction { action_id, .. }) if action_id.starts_with("msg:a-3@")
        ),
        "r writes to the task's node"
    );
    let mut app = task_screen(&domain, &live, "r-3", "t-3");
    app.on_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("clear_completed:"), "{sent:?}");
}

#[test]
fn the_completed_header_offers_clear_completed_and_shift_a_empties_it_at_once() {
    let (domain, live) = busy();
    let mut app = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
    open_finished(&mut app, &domain, &live);
    let before = rendered(&mut app, 100, 30);
    dump("tasks (completed, before clearing)", &before);
    let header = lines_with(&before, "COMPLETADAS");
    assert!(header.len() == 1 && header[0].contains(" A  Limpiar completadas │") && header[0].contains("COMPLETADAS 3"), "{before}");
    assert!(before.contains(NIGHTLY), "{before}");

    app.on_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("clear_completed:view@"), "{sent:?}");
    let cleared = rendered(&mut app, 100, 30);
    dump("tasks (completed, just cleared)", &cleared);
    assert!(!cleared.contains(NIGHTLY) && !cleared.contains("Limpiar completadas"), "{cleared}");
    assert!(!cleared.contains("COMPLETADAS"), "an empty section is not drawn: {cleared}");
    app.on_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
    assert_eq!(app.posted.borrow().len(), 1, "an emptied section offers nothing to clear");

    app.on_msg(Msg::ViewAction { request: sent[0].1.clone(), result: Err(Delivery::Answered(Some("Nothing completed is left to clear".into()))) });
    let back = rendered(&mut app, 100, 30);
    dump("tasks (completed, clearing refused)", &back);
    assert!(back.contains(NIGHTLY) && back.contains("Limpiar completadas"), "the section comes back when the Mac refuses: {back}");
    assert!(back.contains("Tu Mac no pudo hacerlo."), "{back}");

    app.on_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT));
    let again = app.posted.borrow().last().unwrap().1.clone();
    let done = alondra_core::views::Outcome { kind: OutcomeKind::Done, title: "Completadas limpiadas.".into(), body: None, next: None };
    app.on_msg(Msg::ViewAction { request: again, result: Ok(ViewActionReply::Outcome(done)) });
    let mut cleared = domain.clone();
    cleared.revision += 1;
    for task in cleared.tasks.values_mut().filter(|task| task.state == TaskState::Done) {
        task.archived = true;
    }
    app.on_msg(Msg::ViewHome(Ok(build(&cleared, &live, "tasks"))));
    let after = rendered(&mut app, 100, 30);
    dump("tasks (completed, cleared and confirmed)", &after);
    assert!(!after.contains(NIGHTLY) && !after.contains("COMPLETADAS"), "the page opened before clearing does not stay: {after}");
}

#[test]
fn a_working_tasks_footer_names_its_real_keys() {
    let (domain, live) = busy();
    let mut app = task_screen(&domain, &live, "r-3", "t-3");
    let footer = draw::footer_keys(&app);
    for (key, label) in [("r", "Mensaje"), ("i", "Interrumpir")] {
        assert!(footer.iter().any(|(k, l)| k == key && l == label), "{key}: {footer:?}");
    }
    assert!(!footer.iter().any(|(_, l)| l == "Cerrar nodo"), "a node ends with its task: {footer:?}");
    dump("working task detail", &rendered(&mut app, 120, 30));
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn fixture_view(name: &str) -> views::View {
    serde_json::from_value(super::parity::view_fixture(name)).unwrap()
}

/// The inbox on the daemon's own golden list, with `card` open in the detail pane.
fn card_screen(card: views::View, lang: crate::i18n::Lang) -> App {
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    let mut app = App::new(tx);
    app.on_msg(Msg::ViewHome(Ok(fixture_view("home-tui.json"))));
    app.reconcile();
    crate::i18n::set_for_this_thread(lang);
    app.view_navigation.push(card.key.clone());
    app.card_view = Some(card);
    app
}

fn english(card: views::View) -> App {
    card_screen(card, crate::i18n::Lang::En)
}

fn edited(name: &str, change: impl FnOnce(&mut serde_json::Value)) -> views::View {
    let mut value = super::parity::view_fixture(name);
    change(&mut value["sections"][0]["items"][0]);
    serde_json::from_value(value).unwrap()
}

/// The PR card as a server that still sent a held-back delivery would: its merge disabled with
/// the reason and the card that holds it back.
fn blocked_pr() -> views::View {
    edited("card-pr.json", |card| {
        let question = "8fa4034b-249e-4487-a89b-57de38c26372";
        card["blocked_by"] = serde_json::json!([{
            "id": question,
            "label": "blocked by Ship the compact schedule editor with this beta?",
            "link": {"kind": "view", "key": format!("card:{question}")},
        }]);
        card["actions"].as_array_mut().unwrap().remove(0);
        card["more"].as_array_mut().unwrap().retain(|action| !action["id"].as_str().is_some_and(|id| id.starts_with("merge:")));
    })
}

fn held_card(hold: alondra_core::domain::Hold, spec: alondra_core::domain::CardSpec) -> views::View {
    use alondra_core::domain::*;
    let mut state = DomainState::new("e");
    let mut run = |actor: Actor, command: Command| {
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("c{}", state.revision).into(),
            expected_revision: state.revision,
            command,
        };
        state.execute(&actor, &envelope).unwrap();
    };
    let user = || Actor::User { user_id: "u".into() };
    run(user(), Command::CreateRequest { request_id: "r".into(), body: "Ship the release".into(), auto_merge: false, maintenance: false, message: false });
    run(Actor::System, Command::RegisterAgent { agent_id: "agent-a".into(), name: "release".into(), at: 0 });
    run(Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "t".into(), request_id: "r".into(), title: "Release".into(),
        agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
    run(Actor::System, Command::HoldHook {
        interaction_id: "hold-1".into(), task_id: "t".into(), agent_id: "agent-a".into(),
        options: hold.options(), hold, body: spec.need.clone(), card: spec,
    });
    build(&state, &LiveSnapshot::default(), "card:hold-1")
}

fn lines_with<'a>(screen: &'a str, needle: &str) -> Vec<&'a str> {
    screen.lines().filter(|line| line.contains(needle)).collect()
}

fn typed(app: &mut App, text: &str) {
    for character in text.chars() {
        press(app, KeyCode::Char(character));
    }
}

#[test]
fn inbox_lists_one_row_per_card_with_its_kind_glyph_and_meta_on_the_right() {
    let mut app = english(fixture_view("card-question.json"));
    app.view_navigation.clear();
    app.card_view = None;
    app.sel = Some(Key("a98d034b-249e-4487-a89b-57de38c26372".into()));
    let screen = rendered(&mut app, 120, 24);
    dump("inbox: one row per card", &screen);
    assert!(screen.contains("WAITING FOR YOU 5"), "{screen}");
    let welcome = lines_with(&screen, "Choose the welcome copy");
    assert!(welcome[0].contains("▌? Choose the welcome copy"), "{welcome:?}");
    assert!(screen.contains("▌  Restore the native review experience · De…"), "meta that does not fit beside goes whole below: {screen}");
    assert!(screen.contains("◎ Review the beta candidate"), "{screen}");
    assert!(screen.contains("· The simulator pass is clean"), "{screen}");
    assert!(!screen.contains("Merge") && !screen.contains("Reply"), "the list has no buttons: {screen}");
}

#[test]
fn list_glyphs_are_coloured_by_card_type() {
    use alondra_core::domain::CardKind::*;
    let kinds = [Decide, Review, Authorize, Know, Provide, Test, Do, Problem, Propose];
    let colors: Vec<_> = kinds.into_iter().map(view_render::kind_color).collect();
    assert_eq!(colors[1], colors[5], "Review and Test share the green of the board");
    let distinct: BTreeSet<_> = colors.iter().map(|color| format!("{color:?}")).collect();
    assert_eq!(distinct.len(), 8, "{colors:?}");
    let glyphs: BTreeSet<_> = kinds.into_iter().map(view_render::kind_glyph).collect();
    assert_eq!(glyphs.len(), 9);
}

#[test]
fn decide_detail_shows_header_content_context_then_numbered_buttons() {
    let mut app = english(fixture_view("card-question.json"));
    let screen = rendered(&mut app, 120, 40);
    dump("detail: Decide", &screen);
    let at = |needle: &str| screen.find(needle).unwrap_or_else(|| panic!("{needle}: {screen}"));
    assert!(at("╭ decide ") < at("Choose the welcome copy"));
    assert!(at("Choose the welcome copy") < at("The new empty state is ready"));
    assert!(at("The new empty state is ready") < at("Context"));
    assert!(!screen.contains("? DECIDE ·") && !screen.contains("│Content"), "{screen}");
    assert!(at("Context") < at(" 1  ★ Ship the concise copy"));
    assert!(at(" 1  ★ Ship the concise copy") < at(" 2  Keep the warmer copy"));
    assert!(at(" 2  Keep the warmer copy") < at(" r  Reply"));
    assert!(!screen.contains("More…"), "nothing waits behind More: {screen}");
    let footer = draw::footer_keys(&app);
    assert_eq!(
        footer
            .iter()
            .map(|(key, _)| key.as_str())
            .filter(|key| matches!(*key, "1" | "2" | "3" | "m" | "r" | "⏎/o"))
            .collect::<Vec<_>>(),
        ["1", "2", "r", "⏎/o"],
        "{footer:?}"
    );
}

const MARKDOWN_WHY: &str = "## What changed\n\n- The parser keeps **every** heading\n- Lists stay `lists`";

/// The question card with Markdown where agents write it: its need, its why, a button and an option.
fn markdown_card() -> views::View {
    edited("card-question.json", |card| {
        card["need"] = serde_json::json!("Choose the **welcome** copy");
        card["title"] = card["need"].clone();
        card["why"] = serde_json::json!(MARKDOWN_WHY);
        card["subtitle"] = card["why"].clone();
        card["actions"][1]["label"] = serde_json::json!("Keep the *warmer* copy");
        card["actions"][1]["options"] = serde_json::json!([{
            "id": "warm", "label": "Use the **warm** one", "description": "## Why\n- It reads `friendlier`",
            "style": "default", "input": {"kind": "none"},
        }]);
    })
}

#[test]
fn an_agents_markdown_in_a_card_is_drawn_never_its_marks() {
    let mut app = english(markdown_card());
    let screen = rendered(&mut app, 120, 40);
    dump("detail: Markdown from the agent", &screen);
    for mark in ["**", "##", "- The", "`"] {
        assert!(!screen.contains(mark), "{mark:?} shows: {screen}");
    }
    let at = |needle: &str| screen.find(needle).unwrap_or_else(|| panic!("{needle}: {screen}"));
    assert!(at("Choose the welcome copy") < at("What changed"));
    assert!(at("What changed") < at("• The parser keeps every heading"), "the why keeps its heading and list: {screen}");
    assert!(at("• The parser keeps every heading") < at("• Lists stay lists"));
    assert!(at(" 2  Keep the warmer copy") > at("Lists stay lists"));
    let footer = draw::footer_keys(&app);
    assert!(footer.iter().any(|(_, label)| label == "Keep the warmer copy"), "the key bar reads the words: {footer:?}");

    let card = view_render::detail_card(app.card_view.as_ref().unwrap()).unwrap().clone();
    let lines = view_render::card_lines(&card, 80);
    let need = lines.iter().flat_map(|line| &line.spans).find(|span| span.content == "welcome").expect("need span");
    assert!(need.style.add_modifier.contains(ratatui::style::Modifier::BOLD));

    let row = view_render::card_row_lines(&card, 80, false);
    assert_eq!(text(&row[0]).trim_start_matches([' ', '?']).trim(), "Choose the welcome copy", "{row:?}");

    let menu = view_render::menu_lines(&view_render::option_menu(&card.actions[1]), 10);
    let menu = menu.iter().map(text).collect::<Vec<_>>().join("\n");
    assert!(menu.contains("Use the warm one") && menu.contains("    Why · It reads friendlier"), "{menu}");
    assert!(!menu.contains("**") && !menu.contains("##"), "{menu}");
}

#[test]
fn a_row_draws_an_agents_markdown_on_its_one_line() {
    let row: views::Row = serde_json::from_value(serde_json::json!({
        "id": "h-1", "icon": "check", "title": "Answered **Ship it**",
        "subtitle": "## Summary\n- Merged `#12`\n- Released", "depth": 0, "struck": false,
    })).unwrap();
    let lines = view_render::row_lines(&row, 80, false, false);
    let drawn = lines.iter().map(text).collect::<Vec<_>>();
    assert_eq!(drawn.len(), 2, "{drawn:?}");
    assert!(drawn[0].contains("Answered Ship it"), "{drawn:?}");
    assert!(drawn[1].contains("Summary · Merged #12 · Released"), "{drawn:?}");
    assert!(lines[0].spans.iter().any(|span| span.content == "Ship it" && span.style.add_modifier.contains(ratatui::style::Modifier::BOLD)), "{lines:?}");
}

#[test]
fn the_reader_reads_a_cards_why_as_markdown() {
    let mut app = english(markdown_card());
    app.open_reader();
    let screen = rendered(&mut app, 120, 30);
    assert!(screen.lines().nth(1).is_some_and(|border| border.contains("Choose the welcome copy")), "{screen}");
    assert!(!screen.contains("**"), "{screen}");
    let reader = app.reader.as_ref().unwrap();
    let headings: Vec<String> = reader.blocks.iter().filter(|block| matches!(block.kind, md::Kind::Heading(_))).map(md::Block::plain).collect();
    assert!(headings.iter().any(|heading| heading == "What changed"), "{headings:?}");
    assert!(reader.blocks.iter().any(|block| matches!(block.kind, md::Kind::Item(_)) && block.plain() == "The parser keeps every heading"));
}

#[test]
fn a_fact_draws_the_markdown_it_quotes_on_its_line() {
    let mut view = super::parity::view_fixture("card-question.json");
    view["sections"] = serde_json::json!([{"id": "facts", "collapsed": false, "items": [{"type": "facts", "items": [
        {"label": "State", "value": "Needs you · “Pick the **parser** for `settings.json`”"},
    ]}]}]);
    let view: views::View = serde_json::from_value(view).unwrap();
    let drawn = view_render::document_lines(&view, 80, false, false).iter().map(text).collect::<Vec<_>>().join("\n");
    assert!(drawn.contains("State: Needs you · “Pick the parser for settings.json”"), "{drawn}");
}

#[test]
fn summarized_checks_show_their_line_and_the_list_only_once_opened() {
    let mut view = super::parity::view_fixture("card-question.json");
    view["sections"] = serde_json::json!([
        {"id": "status", "collapsed": false, "items": [{"type": "facts", "items": [
            {"label": "State", "value": "Checks failing", "tone": "critical"},
        ]}]},
        {"id": "checks", "collapsed": false, "items": [{"type": "checks",
            "summary": {"text": "1 of 2 passed, 1 failing", "tone": "critical"},
            "items": [{"name": "ui tests", "state": "fail"}, {"name": "build", "state": "pass"}]}]},
    ]);
    let view: views::View = serde_json::from_value(view).unwrap();
    assert!(view_render::summarized_checks(&view));
    let folded = view_render::document_lines(&view, 80, false, false).iter().map(text).collect::<Vec<_>>().join("\n");
    assert!(folded.contains("State: Checks failing"), "{folded}");
    assert!(folded.contains("▸ 1 of 2 passed, 1 failing"), "{folded}");
    assert!(!folded.contains("ui tests"), "the list waits until the user opens it: {folded}");
    let open = view_render::document_lines(&view, 80, true, false).iter().map(text).collect::<Vec<_>>().join("\n");
    assert!(open.contains("▾ 1 of 2 passed, 1 failing\n✗ ui tests\n✓ build"), "{open}");
}

#[test]
fn sections_the_daemon_sends_folded_show_their_title_and_open_with_z() {
    let mut view = super::parity::view_fixture("card-question.json");
    view["sections"] = serde_json::json!([
        {"id": "subagents", "title": "Subagents", "collapsed": false, "items": [
            {"type": "row", "id": "subagent:0", "icon": "history", "title": "Explore the parser", "depth": 0, "struck": false}]},
        {"id": "subagents_finished", "title": "Finished subagents", "count": 2, "collapsed": true, "items": [
            {"type": "row", "id": "subagent:1", "icon": "history", "title": "Review the diff", "depth": 0, "struck": false},
            {"type": "row", "id": "subagent:2", "icon": "history", "title": "Audit the tests", "depth": 0, "struck": false}]},
        {"id": "request", "title": "Request", "collapsed": true, "items": [
            {"type": "markdown", "text": "Show only what matters", "truncated": false, "commentable": false}]},
    ]);
    let view: views::View = serde_json::from_value(view).unwrap();
    assert!(view_render::folded_sections(&view));
    let folded = view_render::document_lines(&view, 80, false, false).iter().map(text).collect::<Vec<_>>().join("\n");
    assert!(folded.contains("Explore the parser"), "what runs stays in sight: {folded}");
    assert!(folded.contains("▸ Finished subagents (2)") && folded.contains("▸ Request"), "{folded}");
    assert!(!folded.contains("Review the diff") && !folded.contains("Show only what matters"), "{folded}");
    let open = view_render::document_lines(&view, 80, false, true).iter().map(text).collect::<Vec<_>>().join("\n");
    assert!(open.contains("▾ Finished subagents (2)") && open.contains("Review the diff") && open.contains("Audit the tests"), "{open}");
    assert!(open.contains("▾ Request\nShow only what matters"), "{open}");
}

fn text(line: &ratatui::text::Line) -> String {
    line.spans.iter().map(|span| span.content.as_ref()).collect()
}

#[test]
fn authorize_detail_has_a_risk_button_and_the_command_to_run() {
    use alondra_core::domain::{CardKind, CardSpec, ContentKind, Hold};
    let spec = CardSpec::new(CardKind::Authorize, "Allow publishing Brew 0.2.36")
        .why("Restarts the daemon for a few seconds.")
        .content(ContentKind::Command, "alondra update");
    let mut app = card_screen(held_card(Hold::Permission, spec), crate::i18n::Lang::Es);
    let screen = rendered(&mut app, 120, 34);
    dump("detail: Authorize", &screen);
    assert!(screen.contains("╭ autorizar "), "{screen}");
    assert!(screen.contains("Allow publishing Brew 0.2.36") && screen.contains("Restarts the daemon"), "{screen}");
    assert!(screen.contains("alondra update"), "the command is shown whole: {screen}");
    assert!(screen.contains(" 1  Permitir") && screen.contains(" 2  Denegar") && screen.contains(" r  Responder"), "{screen}");
}

#[test]
fn review_pr_detail_shows_what_blocks_it_and_offers_no_merge() {
    let mut app = english(blocked_pr());
    let screen = rendered(&mut app, 120, 46);
    dump("detail: Review PR, blocked", &screen);
    assert!(screen.contains("╭ review · pr #12 "), "{screen}");
    assert!(screen.contains("▤ Changes · 4 files · +120 −30 · d view changes") && !screen.contains("0 B"), "{screen}");
    assert!(screen.contains("blocked by Ship the compact schedule editor"), "{screen}");
    assert!(!screen.contains("Merge") && !screen.contains("Answer “Ship"), "a blocked merge is not on the card, grey or not: {screen}");
    let footer = draw::footer_keys(&app);
    assert!(footer.iter().any(|(key, _)| key == "r"), "{footer:?}");
    press(&mut app, KeyCode::Char('1'));
    assert!(app.posted.borrow().is_empty(), "nothing to press sends nothing");
}

#[test]
fn problem_detail_leads_with_its_kind_and_offers_what_to_do() {
    let mut app = english(edited("card-question.json", |card| {
        card["kind"] = "problem".into();
        card["eyebrow"] = serde_json::json!({"text": "Problem", "tone": "critical"});
        card["need"] = "The orchestrator run failed".into();
        card["title"] = "The orchestrator run failed".into();
        card["why"] = "Claude denied a required tool permission.".into();
        card["subtitle"] = "Claude denied a required tool permission.".into();
        card["actions"] = serde_json::json!([
            {"id": "retry:view@1", "label": "Retry", "style": "primary", "role": "advance", "presentation": "button",
             "input": {"kind": "none"}, "attachments": false, "enabled": true},
            {"id": "stop:view@1", "label": "Stop", "style": "secondary", "role": "neutral", "presentation": "button",
             "input": {"kind": "none"}, "attachments": false, "enabled": true}
        ]);
    }));
    let screen = rendered(&mut app, 120, 34);
    dump("detail: Problem", &screen);
    assert!(screen.contains("╭ problem ") && screen.contains("The orchestrator run failed"), "{screen}");
    assert!(screen.contains(" 1  Retry") && screen.contains(" 2  Stop"), "{screen}");
}

#[test]
fn know_detail_only_offers_seen_and_the_reply() {
    let mut app = english(fixture_view("card-information.json"));
    let screen = rendered(&mut app, 120, 34);
    dump("detail: Know", &screen);
    assert!(screen.contains("╭ fyi ") || screen.contains("╭ know "), "{screen}");
    assert!(screen.contains("The simulator pass is clean"), "{screen}");
    assert!(screen.contains(" 1  Seen") && screen.contains(" r  Reply") && !screen.contains(" m  "), "{screen}");
    let footer = draw::footer_keys(&app);
    assert!(
        footer.iter().any(|(key, _)| key == "r") && !footer.iter().any(|(key, _)| key == "2" || key == "m"),
        "{footer:?}"
    );
}

#[test]
fn m_opens_the_more_menu_with_its_own_numbering_and_escape_closes_it() {
    let mut app = english(blocked_pr());
    press(&mut app, KeyCode::Char('m'));
    let screen = rendered(&mut app, 120, 46);
    dump("detail: More menu", &screen);
    assert!(screen.contains("MORE"), "{screen}");
    assert!(screen.contains(" 1  Close without merging") && !screen.contains("Accept without merging"), "{screen}");
    assert!(screen.contains(" 2  Open on GitHub"), "{screen}");
    assert!(!screen.contains(" m  More…"), "the menu replaces the buttons: {screen}");
    let footer = draw::footer_keys(&app);
    assert_eq!(footer.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(), ["1-2", "Esc"]);
    press(&mut app, KeyCode::Esc);
    assert!(app.card_menu.is_none());
    assert!(rendered(&mut app, 120, 46).contains(" m  More… (2)"));
}

#[test]
fn r_replies_to_the_node_with_a_text_box() {
    let mut app = english(fixture_view("card-question.json"));
    press(&mut app, KeyCode::Char('r'));
    let reply_id = super::parity::view_fixture("card-question.json")["sections"][0]["items"][0]["reply"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(matches!(
        app.composer.as_ref().map(|composer| &composer.target),
        Some(Target::ViewAction { action_id, required: true, .. }) if *action_id == reply_id
    ));
    let screen = rendered(&mut app, 120, 34);
    dump("reply composer", &screen);
    typed(&mut app, "Use the warm one");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.posted.borrow()[0].1["action_id"], reply_id.as_str());
    assert_eq!(app.posted.borrow()[0].1["input"]["text"], "Use the warm one");
}

#[test]
fn a_secret_card_names_its_file_and_its_value_is_typed_hidden() {
    let mut app = card_screen(fixture_view("card-secret-es-tui.json"), crate::i18n::Lang::Es);
    let screen = rendered(&mut app, 120, 34);
    dump("secret card", &screen);
    assert!(screen.contains(".env") && screen.contains("STRIPE_KEY"), "{screen}");
    press(&mut app, KeyCode::Char('r'));
    assert!(matches!(app.composer.as_ref().map(|composer| &composer.target), Some(Target::ViewAction { secret: true, .. })));
    typed(&mut app, "sk_test_4eC39HqLyjWDarjtT1zdp7dc");
    let screen = rendered(&mut app, 120, 34);
    dump("secret composer", &screen);
    assert!(!screen.contains("sk_test_4eC39HqLyjWDarjtT1zdp7dc") && screen.contains("••••••••"), "{screen}");
    assert!(app.to_restore().composer.is_none(), "a secret is never kept for the next process");
}

fn with_note_input(required: bool) -> views::View {
    edited("card-question.json", |card| {
        for action in card["actions"].as_array_mut().unwrap() {
            action["input"] = serde_json::json!({"kind": "text", "placeholder": "Add a note", "required": required, "multiline": true});
        }
    })
}

#[test]
fn pressing_an_option_answers_at_once_even_on_a_card_that_asked_for_a_note() {
    for required in [false, true] {
        for key in ['1', '2'] {
            let mut app = english(with_note_input(required));
            press(&mut app, KeyCode::Char(key));
            let sent = app.posted.borrow().clone();
            assert!(app.composer.is_none(), "key {key} (note required: {required}) opened the text box");
            assert_eq!(sent.len(), 1, "key {key}: {sent:?}");
            assert!(sent[0].1["option_id"].is_string(), "{sent:?}");
            assert_eq!(sent[0].1["input"], serde_json::json!({}));
        }
    }
}

#[test]
fn a_field_opens_empty_after_its_words_were_sent_or_closed_with_esc() {
    let mut app = english(question_whose_reply_stays());
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "Rotated both keys");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.posted.borrow().len(), 1, "{:?}", app.posted.borrow());
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(app.composer.as_ref().map(|c| c.input.text()), Some(String::new()), "after sending");
    typed(&mut app, "Half a thought");
    press(&mut app, KeyCode::Esc);
    assert!(app.composer.is_none());
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(app.composer.as_ref().map(|c| c.input.text()), Some(String::new()), "after Esc");
}

fn question_whose_reply_stays() -> views::View {
    edited("card-question.json", |card| card["reply"]["resolves_card"] = serde_json::json!(false))
}

#[test]
fn what_is_sent_is_what_the_field_holds_when_it_is_sent() {
    let mut app = english(question_whose_reply_stays());
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "First words");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "What I mean now");
    press(&mut app, KeyCode::Enter);
    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].1["input"]["text"], "What I mean now");

    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "An older thought");
    press(&mut app, KeyCode::Esc);
    let screen = rendered(&mut app, 120, 60);
    assert!(!screen.contains("An older thought"), "{screen}");
    press(&mut app, KeyCode::Char('1'));
    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[1].1["option_id"].is_string(), "{sent:?}");
    assert_eq!(sent[1].1["input"], serde_json::json!({}), "nothing closed with Esc goes with the option");
}

fn commented_plan() -> views::View {
    let mut value = super::parity::view_fixture("card-delivery-plan.json");
    for section in value["sections"].as_array_mut().unwrap() {
        for item in section["items"].as_array_mut().unwrap() {
            if item["type"] == "markdown" {
                item["commentable"] = serde_json::json!(true);
            }
        }
    }
    serde_json::from_value(value).unwrap()
}

fn comment_on(app: &mut App, block: usize, text: &str) {
    app.reader.as_mut().unwrap().cursor = block;
    press(app, KeyCode::Char('c'));
    typed(app, text);
    press(app, KeyCode::Enter);
}

#[test]
fn a_review_sends_every_comment_written_even_after_its_send_field_was_closed_once() {
    let mut app = english(commented_plan());
    app.open_reader();
    let blocks: Vec<usize> = app.reader.as_ref().unwrap().blocks.iter().enumerate()
        .filter(|(_, block)| block.commentable()).map(|(index, _)| index).collect();
    assert!(blocks.len() >= 3, "{blocks:?}");
    comment_on(&mut app, blocks[0], "Say which store");
    press(&mut app, KeyCode::Char('s'));
    assert!(app.composer.as_ref().is_some_and(|c| c.input.text().contains("Say which store")));
    press(&mut app, KeyCode::Esc);

    comment_on(&mut app, blocks[1], "Only on Wi-Fi");
    comment_on(&mut app, blocks[2], "Hide it when done");
    press(&mut app, KeyCode::Char('C'));
    typed(&mut app, "Otherwise good");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('s'));
    let field = app.composer.as_ref().map(|c| c.input.text()).unwrap_or_default();
    let screen = rendered(&mut app, 120, 60);
    dump("reader: the send field holds every comment", &screen);
    press(&mut app, KeyCode::Enter);

    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("changes:"), "{sent:?}");
    let text = sent[0].1["input"]["text"].as_str().unwrap_or_default().to_owned();
    assert_eq!(text, field.trim(), "what is sent is what the field showed");
    for comment in ["Say which store", "Only on Wi-Fi", "Hide it when done", "Otherwise good"] {
        assert!(text.contains(comment), "{comment} is missing from {text:?}");
    }
}

#[test]
fn the_cards_field_alone_still_answers_in_the_users_own_words() {
    let mut app = english(fixture_view("card-question.json"));
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "Neither: keep the old one");
    press(&mut app, KeyCode::Enter);
    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("reply:"), "{sent:?}");
    assert!(sent[0].1["option_id"].is_null(), "{sent:?}");
    assert_eq!(sent[0].1["input"]["text"], "Neither: keep the old one");
}

#[test]
fn a_button_with_options_asks_which_before_it_sends() {
    let mut app = english(fixture_view("card-pr.json"));
    app.card_view = Some(edited("card-pr.json", |card| {
        for action in card["actions"].as_array_mut().unwrap() {
            action["enabled"] = true.into();
            action.as_object_mut().unwrap().remove("disabled_reason");
        }
        card["actions"][0].as_object_mut().unwrap().remove("confirm");
        card["actions"][0].as_object_mut().unwrap().remove("option_id");
        card["actions"][0]["label"] = "Merge".into();
        card["actions"][0]["presentation"] = "menu".into();
        card["actions"][0]["options"] = serde_json::json!([
            {"id": "squash", "label": "Squash and merge", "style": "secondary", "input": {"kind": "none"}},
            {"id": "merge", "label": "Merge commit", "style": "secondary", "input": {"kind": "none"}},
        ]);
    }));
    press(&mut app, KeyCode::Char('1'));
    assert!(app.posted.borrow().is_empty());
    let screen = rendered(&mut app, 120, 46);
    dump("detail: merge methods", &screen);
    assert!(
        screen.contains("MERGE") && screen.contains(" 1  Squash and merge") && screen.contains(" 2  Merge commit"),
        "{screen}"
    );
    press(&mut app, KeyCode::Char('2'));
    let sent = app.posted.borrow().clone();
    assert_eq!(sent[0].1["option_id"], "merge", "{sent:?}");
}

#[test]
fn every_button_a_card_shows_answers_to_its_own_digit() {
    for (digit, buttons) in [('4', 4), ('5', 5), ('9', 9)] {
        let mut app = english(fixture_view("card-pr.json"));
        app.card_view = Some(edited("card-pr.json", |card| {
            let template = card["actions"][0].clone();
            card["actions"] = (1..=buttons)
                .map(|number| {
                    let mut action = template.clone();
                    action["id"] = format!("button-{number}").into();
                    action["label"] = format!("Button {number}").into();
                    action.as_object_mut().unwrap().remove("option_id");
                    action
                })
                .collect();
        }));
        press(&mut app, KeyCode::Char(digit));
        let sent = app.posted.borrow().clone();
        assert_eq!(sent.len(), 1, "{digit}: {sent:?}");
        assert_eq!(sent[0].1["action_id"], format!("button-{digit}"), "{sent:?}");
    }
}

#[test]
fn keys_beyond_the_buttons_do_nothing_and_a_menu_swallows_the_rest() {
    let mut app = english(fixture_view("card-information.json"));
    for key in ['2', '3', 'm', '9'] {
        press(&mut app, KeyCode::Char(key));
    }
    assert!(app.posted.borrow().is_empty() && app.composer.is_none() && app.card_menu.is_none());
    let mut app = english(fixture_view("card-question.json"));
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Char('m'));
    assert!(app.posted.borrow().is_empty() && app.card_menu.is_none(), "a Decide with two buttons has no third and nothing behind More");
    let mut app = english(fixture_view("card-pr.json"));
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Char('j'));
    assert!(app.card_menu.is_some(), "a menu keeps keys that are not its own");
    press(&mut app, KeyCode::Char('0'));
    assert!(app.card_menu.is_some() && app.posted.borrow().is_empty());
}

#[test]
fn cards_read_in_spanish() {
    let mut app = card_screen(fixture_view("card-question.json"), crate::i18n::Lang::Es);
    let screen = rendered(&mut app, 120, 40);
    dump("detalle: Decidir", &screen);
    assert!(!screen.contains("Más…"), "nothing waits behind More: {screen}");
    let footer = draw::footer_keys(&app);
    assert!(footer.iter().any(|(key, label)| key == "r" && label == "responder"), "{footer:?}");
    assert!(footer.iter().any(|(key, label)| key == "⏎/o" && label == "abrir"), "{footer:?}");
    let mut app = card_screen(fixture_view("card-pr-es.json"), crate::i18n::Lang::Es);
    assert!(rendered(&mut app, 120, 40).contains(" m  Más… (2)"));
    press(&mut app, KeyCode::Char('m'));
    assert!(rendered(&mut app, 120, 40).contains("MÁS"));
}

fn header(screen: &str) -> &str {
    screen.lines().find(|line| line.contains("Bandeja")).unwrap_or_default()
}

fn rows_and_colours(app: &mut App) -> (Vec<String>, ratatui::style::Color) {
    let backend = ratatui::backend::TestBackend::new(160, 16);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let rows = (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>().trim_end().to_owned())
        .collect();
    (rows, buffer[(1, 0)].bg)
}

fn account_here() -> identity::Here {
    identity::Here {
        account_home: "/Users/someone".into(),
        home: "/Users/someone".into(),
        alondra_home: "/Users/someone/.alondra".into(),
        port: alondra_core::DEFAULT_PORT,
        sandbox_env: false,
    }
}

#[test]
fn a_sandbox_daemon_is_named_across_the_top_and_the_real_one_changes_nothing() {
    let domain = DomainState::new("screens");
    let live = LiveSnapshot::default();
    let mut app = app_on(Screen::Inbox, build(&domain, &live, "home"));
    let (before, _) = rows_and_colours(&mut app);

    app.here = Some(account_here());
    app.on_msg(Msg::DaemonIdentity(identity::Answer { home: "/Users/someone/.alondra".into(), sandbox: false }));
    let (real, _) = rows_and_colours(&mut app);
    assert_eq!(real, before);

    app.here = Some(identity::Here {
        home: "/tmp/s/tui-home".into(),
        alondra_home: "/tmp/s/mac-1".into(),
        port: 48211,
        sandbox_env: true,
        ..account_here()
    });
    app.on_msg(Msg::DaemonIdentity(identity::Answer { home: "/tmp/s/mac-1".into(), sandbox: true }));
    let (sandbox, colour) = rows_and_colours(&mut app);
    dump("sandbox daemon", &sandbox.join("\n"));
    assert_eq!(sandbox[0], " SANDBOX · /tmp/s/mac-1 · 127.0.0.1:48211");
    assert_eq!(colour, ratatui::style::Color::Yellow);
    assert_eq!(sandbox[1], before[0]);
    assert_eq!(sandbox.last(), before.last());
}

#[test]
fn the_real_daemon_reached_from_a_temporary_home_is_warned_in_red() {
    let domain = DomainState::new("screens");
    let live = LiveSnapshot::default();
    let mut app = app_on(Screen::Inbox, build(&domain, &live, "home"));
    app.here = Some(identity::Here { home: "/tmp/fake".into(), alondra_home: "/tmp/fake/.alondra".into(), ..account_here() });
    app.on_msg(Msg::DaemonIdentity(identity::Answer { home: "/Users/someone/.alondra".into(), sandbox: false }));
    let (rows, colour) = rows_and_colours(&mut app);
    dump("real daemon from a temporary home", &rows.join("\n"));
    assert!(rows[0].starts_with(" DAEMON REAL · ~/.alondra · 127.0.0.1:47800 — no es un sandbox"), "{}", rows[0]);
    assert_eq!(colour, ratatui::style::Color::Red);
}

#[test]
fn the_header_shows_every_mac_as_a_tab_with_how_it_is_reached() {
    let domain = DomainState::new("screens");
    let live = LiveSnapshot::default();
    let mut app = app_on(Screen::Inbox, build(&domain, &live, "home"));
    let alone = rendered(&mut app, 160, 16);
    assert!(header(&alone).starts_with(" Alondra  "), "{alone}");

    app.macs = vec![
        MacTab { name: "Este Mac".into(), status: MacStatus::Here, attention: 2, active: false },
        MacTab { name: "Studio".into(), status: MacStatus::Lan, attention: 0, active: true },
        MacTab { name: "Mini".into(), status: MacStatus::Tailscale, attention: 1, active: false },
        MacTab { name: "Air".into(), status: MacStatus::Offline, attention: 0, active: false },
    ];
    let backend = ratatui::backend::TestBackend::new(160, 16);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw::draw(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let row: String = (0..buffer.area.width).map(|x| buffer[(x, 0)].symbol()).collect();
    dump("header with four Macs", &row);
    assert!(
        row.starts_with(
            " Este Mac 2  Studio · En la red local  Mini · Por Tailscale 1  Air · Sin conexión  "
        ),
        "{row}"
    );
    assert!(!row.contains("Alondra"), "{row}");
    let studio = row.chars().position(|c| c == 'S').unwrap() as u16;
    let accent = ratatui::style::Color::LightYellow;
    assert_eq!(buffer[(studio, 0)].bg, accent, "{row}");
    assert_ne!(buffer[(1, 0)].bg, accent, "{row}");
}

#[test]
fn a_direct_mac_tab_says_whether_it_answers() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let here = App::new(tx.clone());
    assert_eq!(
        here.mac_tab(true),
        MacTab { name: "This Mac".into(), status: MacStatus::Here, attention: 0, active: true }
    );
    let mac: crate::direct_client::DirectMac = serde_json::from_value(json!({
        "mac_id": "mac-studio", "name": "Studio", "mac_signing_key": "k", "mac_agreement_key": "k",
        "device_id": "d", "signing_key": "k", "agreement_key": "k", "routes": ["192.168.1.20:47801"]
    }))
    .unwrap();
    let mut studio = App::with_target(
        tx,
        MacTarget::Direct(std::sync::Arc::new(crate::direct_client::Connection::new(mac))),
    );
    assert_eq!(studio.mac_tab(false).name, "Studio");
    assert_eq!(studio.mac_tab(false).status, MacStatus::Connecting);
    studio.on_msg(Msg::ViewHome(Err("refused".into())));
    assert_eq!(studio.mac_tab(false).status, MacStatus::Offline);
    use crate::direct_client::RouteKind;
    assert_eq!(MacStatus::direct(false, true, Some(RouteKind::Lan)), MacStatus::Lan);
    assert_eq!(MacStatus::direct(false, true, Some(RouteKind::Tailscale)), MacStatus::Tailscale);
    assert_eq!(MacStatus::direct(false, true, Some(RouteKind::Direct)), MacStatus::Direct);
    assert_eq!(MacStatus::direct(false, true, Some(RouteKind::Relay)), MacStatus::Relay);
    assert_eq!(MacStatus::direct(true, false, Some(RouteKind::Lan)), MacStatus::Offline);
}

fn waiting_device(id: &str, name: &str, expires_at_ms: i64) -> PendingDevice {
    PendingDevice { id: id.into(), name: name.into(), code: "482913".into(), expires_at_ms }
}

#[test]
fn a_device_asking_to_pair_shows_a_prompt_until_the_mac_stops_listing_it() {
    let domain = DomainState::new("screens");
    let live = LiveSnapshot::default();
    let mut app = app_on(Screen::Inbox, build(&domain, &live, "home"));
    let question =
        "¿Permitir que «iPad de Ana» use Alondra? Código 482913 — [p] Permitir · [d] Denegar";
    let later = now_millis() + 60_000;
    app.on_msg(Msg::PendingDevices(vec![
        waiting_device("p-1", "iPad de Ana", later),
        waiting_device("p-2", "Portátil", later),
    ]));
    let screen = rendered(&mut app, 120, 20);
    dump("pairing prompt", &screen);
    assert!(screen.contains(question), "{screen}");
    assert!(!screen.contains("Portátil"), "{screen}");

    app.on_msg(Msg::PendingDevices(vec![]));
    assert!(!rendered(&mut app, 120, 20).contains("Permitir"));

    app.on_msg(Msg::PendingDevices(vec![waiting_device(
        "p-3",
        "iPad de Ana",
        now_millis() - 1,
    )]));
    assert!(!rendered(&mut app, 120, 20).contains("Permitir"));
}

#[test]
fn a_blocked_task_action_says_why_instead_of_asking_first() {
    let (domain, live) = busy();
    let mut app = task_screen(&domain, &live, "r-3", "t-3");
    let mut view = serde_json::to_value(app.card_view.clone().unwrap()).unwrap();
    fn block(value: &mut serde_json::Value, key: &str) -> usize {
        match value {
            serde_json::Value::Object(map) if map.get("key").and_then(|k| k.as_str()) == Some(key) && map.contains_key("confirm") => {
                map.insert("enabled".into(), false.into());
                map.insert("disabled_reason".into(), "The task is already closing".into());
                1
            }
            serde_json::Value::Object(map) => map.values_mut().map(|value| block(value, key)).sum(),
            serde_json::Value::Array(items) => items.iter_mut().map(|value| block(value, key)).sum(),
            _ => 0,
        }
    }
    assert_eq!(block(&mut view, "x"), 1, "the task's cancel action, which asks first");
    app.card_view = Some(serde_json::from_value(view).unwrap());
    assert!(!draw::footer_keys(&app).iter().any(|(key, _)| key == "x"), "a blocked action is not offered");
    press(&mut app, KeyCode::Char('x'));
    assert!(app.modal.is_none(), "no question about something that cannot run");
    assert!(app.posted.borrow().is_empty());
    let screen = rendered(&mut app, 130, 30);
    assert!(screen.contains("The task is already closing"), "the detail says why: {screen}");
}

#[test]
fn l_on_the_tasks_never_changes_the_limit() {
    let (domain, mut live) = busy();
    live.pacing = Some(views::LivePacing { working: 3, limit: 6, fixed: None, auto: 6, most: 12, memory_free: Some(40), tight: false,
        queued: vec![], paused: vec![] });
    let mut app = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
    app.on_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
    assert!(app.card_menu.is_none(), "the limit is changed with `alondra config set max_working_nodes`");
    assert!(app.posted.borrow().is_empty());
}

const SIGN_IN_EN: &str = "Sign in to the Claude account Trabajo on the Mac";
const SIGN_IN_ES: &str = "Inicia sesión en la cuenta de Claude Trabajo en el Mac";
const SIGNED_IN_EN: &str = "I signed in";
const SIGNED_IN_ES: &str = "Ya he iniciado sesión";

fn sign_in_card(need: &str, signed_in: &str) -> views::View {
    let mut value = super::parity::view_fixture("card-question.json");
    value["title"] = need.into();
    let card = &mut value["sections"][0]["items"][0];
    card["need"] = need.into();
    card["title"] = need.into();
    card["actions"][0]["label"] = signed_in.into();
    serde_json::from_value(value).unwrap()
}

fn sign_in_inbox(lang: crate::i18n::Lang, need: &str) -> App {
    let mut home = super::parity::view_fixture("home-tui.json");
    home["sections"][0]["items"][0]["need"] = need.into();
    home["sections"][0]["items"][0]["title"] = need.into();
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    let mut app = App::new(tx);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(home).unwrap())));
    app.reconcile();
    crate::i18n::set_for_this_thread(lang);
    app.sel = Some(Key("a98d034b-249e-4487-a89b-57de38c26372".into()));
    app
}

fn joined(screen: &str) -> String {
    screen.lines().map(|line| line.trim_matches(|c: char| c == '▌' || c == '│' || c.is_whitespace())).collect::<Vec<_>>().join(" ")
}

#[test]
fn inbox_wraps_a_long_card_title_instead_of_cutting_it() {
    for (lang, need) in [(crate::i18n::Lang::En, SIGN_IN_EN), (crate::i18n::Lang::Es, SIGN_IN_ES)] {
        for width in [80, 120] {
            let mut app = sign_in_inbox(lang, need);
            let screen = rendered(&mut app, width, 24);
            dump(&format!("inbox {lang:?} {width}"), &screen);
            assert!(joined(&screen).contains(need), "{width} columns: {screen}");
        }
    }
}

#[test]
fn the_footer_shows_a_long_button_label_whole() {
    for (lang, need, signed_in) in [
        (crate::i18n::Lang::En, SIGN_IN_EN, SIGNED_IN_EN),
        (crate::i18n::Lang::Es, SIGN_IN_ES, SIGNED_IN_ES),
    ] {
        for width in [80, 120] {
            let mut app = sign_in_inbox(lang, need);
            let card = sign_in_card(need, signed_in);
            app.view_navigation.push(card.key.clone());
            app.card_view = Some(card);
            let screen = rendered(&mut app, width, 24);
            dump(&format!("card {lang:?} {width}"), &screen);
            let footer: Vec<&str> = screen.lines().skip_while(|line| !line.starts_with('╰')).skip(1).collect();
            assert!(footer.iter().any(|line| line.contains(&format!("1 {signed_in}"))), "{width} columns: {footer:?}\n{screen}");
            assert!(joined(&screen).contains(need), "{width} columns: {screen}");
        }
    }
}

#[test]
fn the_reader_border_names_the_card_whole_or_leaves_it_to_the_page() {
    for (lang, need, signed_in) in [
        (crate::i18n::Lang::En, SIGN_IN_EN, SIGNED_IN_EN),
        (crate::i18n::Lang::Es, SIGN_IN_ES, SIGNED_IN_ES),
    ] {
        for width in [80, 120] {
            let mut app = card_screen(sign_in_card(need, signed_in), lang);
            app.open_reader();
            let screen = rendered(&mut app, width, 24);
            dump(&format!("reader {lang:?} {width}"), &screen);
            let border = screen.lines().nth(1).unwrap();
            assert!(border.contains(need) || !border.contains(&need[..10]), "{width} columns: {border:?}");
            assert!(joined(&screen).contains(need), "{width} columns: {screen}");
        }
    }
}

const SHOT: &str = "/interactions/i-1/attachments/shot.png/content";
const CLIP: &str = "/interactions/i-1/attachments/clip.mp4/content";

fn card_with_attachments() -> views::View {
    edited("card-question.json", |card| {
        card["attachments"] = serde_json::json!([
            {"id": "shot.png", "name": "shot.png", "kind": "image", "size": 348_160, "digest": "a".repeat(64), "fetch": SHOT},
            {"id": "clip.mp4", "name": "clip.mp4", "kind": "video", "size": 13_002_342, "digest": "b".repeat(64), "fetch": CLIP},
        ]);
    })
}

fn reader_with_attachments(protocol: ratatui_image::picker::ProtocolType) -> App {
    let mut app = card_screen(card_with_attachments(), crate::i18n::Lang::Es);
    app.picker.as_mut().unwrap().set_protocol_type(protocol);
    app.open_reader();
    app.on_msg(Msg::File { route: SHOT.into(), result: Ok(files::FileBody::classify(files::png(64, 32))) });
    app
}

#[test]
fn a_terminal_with_a_graphics_protocol_draws_the_cards_image_under_its_caption() {
    let mut app = reader_with_attachments(ratatui_image::picker::ProtocolType::Kitty);
    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("\x1b_G"), "the image goes out as Kitty graphics: {screen:?}");
    assert!(screen.contains("Imagen 64×32 · 340 KB · v la abre en Vista Previa"), "{screen}");
    let reader = app.reader.as_ref().unwrap();
    assert!(reader.blocks.iter().any(|block| matches!(block.kind, md::Kind::Image { .. }) && block.targets.iter().any(|t| t.url == SHOT)));
}

#[test]
fn a_terminal_without_a_graphics_protocol_shows_the_image_as_a_line_that_opens_it() {
    let mut app = reader_with_attachments(ratatui_image::picker::ProtocolType::Halfblocks);
    let screen = rendered(&mut app, 120, 40);
    assert!(!screen.contains("\x1b_G") && !screen.contains('▀'), "nothing is drawn in its place: {screen}");
    let reader = app.reader.as_ref().unwrap();
    assert!(reader.blocks.iter().any(|block| block.plain() == "📷 shot.png"));
    assert!(!reader.blocks.iter().any(|block| matches!(block.kind, md::Kind::Image { .. })));
    let line = reader.blocks.iter().find(|block| block.targets.iter().any(|t| t.url == SHOT)).unwrap();
    assert_eq!(line.plain(), "Imagen 64×32 · 340 KB · v la abre en Vista Previa");
}

#[test]
fn a_video_is_one_line_with_its_kind_and_size_and_is_only_downloaded_when_opened() {
    let mut app = reader_with_attachments(ratatui_image::picker::ProtocolType::Kitty);
    assert!(app.loading.contains(SHOT) || app.files.contains_key(SHOT));
    assert!(!app.loading.contains(CLIP) && !app.files.contains_key(CLIP), "opening the card downloads no video");
    let screen = rendered(&mut app, 120, 40);
    assert!(screen.contains("Vídeo · 12.4 MB"), "{screen}");
    let reader = app.reader.as_mut().unwrap();
    assert!(reader.blocks.iter().any(|block| block.plain() == "🎬 clip.mp4"));
    let at = reader.blocks.iter().position(|block| block.targets.iter().any(|t| t.url == CLIP)).unwrap();
    assert_eq!(reader.blocks[at].plain(), "Vídeo · 12.4 MB · v lo baja y lo abre con su aplicación");
    reader.cursor = at;
    let spawned = app.spawned.borrow().len();
    app.on_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('v')));
    assert_eq!(app.spawned.borrow().len(), spawned + 1);
    assert_eq!(app.flash.as_ref().map(|flash| flash.text.as_str()), Some("Bajando «clip.mp4»…"));
}

#[test]
fn the_card_detail_lists_each_attachment_with_its_kind_and_size() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let drawn = view_render::document_lines(&card_with_attachments(), 100, false, false).iter().map(text).collect::<Vec<_>>().join("\n");
    assert!(drawn.contains("📷 shot.png · Imagen · 340 KB\n🎬 clip.mp4 · Vídeo · 12.4 MB\no abre la tarjeta para verlos y abrirlos"), "{drawn}");
}
