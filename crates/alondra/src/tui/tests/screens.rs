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
        dependencies: BTreeSet::new(),
        stack: None,
        documents: BTreeMap::new(),
        archived: false,
        model: None,
        delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: Default::default(), priority: Default::default(),
    }
}

/// Four nodes with one task each, three finished and one working, plus a scheduled task nobody
/// has started: the owner's fleet on 2026-10-03.
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
            projection: &domain.projection(),
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
fn fleet_lists_each_live_node_once_with_its_state_and_the_header_counts_them() {
    let (domain, live) = busy();
    let mut app = app_on(Screen::Fleet, build(&domain, &live, "fleet"));
    let screen = rendered(&mut app, 100, 24);
    dump("fleet", &screen);
    assert_eq!(screen.matches(ODD_STATE).count(), 1, "{screen}");
    assert!(!screen.contains(NIGHTLY) && !screen.contains("Terminado"), "a node whose task ended is not in Fleet: {screen}");
    assert!(screen.contains("Flota 1"), "the header counts the rows Fleet lists: {screen}");
    assert!(!screen.contains("↳"), "{screen}");
    assert!(!screen.contains("· 0"), "{screen}");
    assert!(screen.contains("Trabajando"), "{screen}");
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
fn the_tasks_list_is_three_blocks_with_the_chain_folded_and_names_a_new_request_by_its_first_sentence() {
    let mut app = app_on(Screen::Tasks, fixture_view("tasks-tui.json"));
    let screen = rendered(&mut app, 120, 30);
    dump("tasks (blocks)", &screen);
    let lines: Vec<&str> = screen.lines().collect();
    let at = |needle: &str| lines.iter().position(|line| line.contains(needle)).unwrap_or_else(|| panic!("{needle}: {screen}"));
    assert!(at("FOR YOU 1") < at("IN PROGRESS 4") && at("IN PROGRESS 4") < at("WAITING 3"), "{screen}");
    assert!(!screen.contains("ACTIVE"), "the blocks replace the Active title: {screen}");
    let head = at("Restore the native review experience");
    assert!(lines[head + 1..].iter().take(3).any(|line| line.contains("▸ +1 waiting for this")), "{screen}");
    assert!(!screen.contains("Decrement the badge on retraction"), "folded until opened: {screen}");
    assert!(!screen.contains("from: ") && !screen.contains("waits for Restore"), "{screen}");
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
fn a_task_in_review_without_a_card_is_not_for_you_in_tasks_nor_in_fleet() {
    let (mut domain, mut live) = busy();
    domain.tasks.get_mut(&"t-3".into()).unwrap().state = TaskState::ForYourReview;
    domain.agents.get_mut(&"a-3".into()).unwrap().phase = alondra_core::domain::NodePhase::Delivered { in_turn: false };
    live.nodes.iter_mut().for_each(|node| node.working = false);
    assert!(domain.projection().inbox.iter().all(|item| !item.pending), "no card waits");
    let mut tasks = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
    let screen = rendered(&mut tasks, 120, 30);
    dump("tasks (in review, no card)", &screen);
    assert!(!screen.contains("PARA TI") && !screen.contains("Para revisar"), "{screen}");
    assert!(screen.contains(ODD_STATE) && screen.contains("Entregada"), "{screen}");
    let mut fleet = app_on(Screen::Fleet, build(&domain, &live, "fleet"));
    let screen = rendered(&mut fleet, 120, 30);
    dump("fleet (in review, no card)", &screen);
    assert!(!screen.contains("Espera tu revisión"), "{screen}");
}

#[test]
fn finished_and_archived_start_folded_and_enter_on_the_header_opens_and_folds_them() {
    let (mut domain, live) = busy();
    domain.tasks.get_mut(&"t-weekly".into()).unwrap().state = TaskState::Done;
    let mut app = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
    let folded = rendered(&mut app, 100, 30);
    dump("tasks (finished folded)", &folded);
    assert!(folded.contains("▸ TERMINADAS"), "{folded}");
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
    assert!(open.contains("▾ TERMINADAS") && open.contains(WEEKLY), "{open}");

    press(&mut app, KeyCode::Char('o'));
    let again = rendered(&mut app, 100, 30);
    assert!(again.contains("▸ TERMINADAS") && !again.contains(WEEKLY), "{again}");
}

#[test]
fn a_chain_of_waiting_tasks_folds_under_its_first_task_and_enter_opens_it() {
    let mut domain = DomainState::new("screens");
    domain.revision = 1;
    let mut request = Request::new("r-onb".into(), "Instalación guiada y nueva CLI".into());
    request.title = Some("Instalación guiada y nueva CLI: plan por fases".into());
    domain.requests.insert("r-onb".into(), request);
    for (id, title, state, dependencies) in [
        ("t-2", "Textos y versión", TaskState::NotStarted, &[][..]),
        ("t-2b", "Versión en la TUI", TaskState::WaitingDependency, &["t-2"][..]),
        ("t-3", "Motor de comprobaciones", TaskState::WaitingDependency, &["t-2"][..]),
        ("t-4", "Asistente de instalación", TaskState::WaitingDependency, &["t-3"][..]),
        ("t-5", "Fórmula de Homebrew", TaskState::WaitingDependency, &["t-4"][..]),
        ("t-6", "API de ajustes", TaskState::WaitingDependency, &["t-2b"][..]),
        ("t-7", "Ajustes en las apps", TaskState::WaitingDependency, &["t-6"][..]),
        ("t-8", "API de la CLI", TaskState::WaitingDependency, &["t-4", "t-5", "t-7"][..]),
        ("t-9", "Documentación", TaskState::WaitingDependency, &["t-8"][..]),
    ] {
        let mut row = task(id, "r-onb", None, title, state);
        row.dependencies = dependencies.iter().map(|id| (*id).into()).collect();
        domain.tasks.insert(id.into(), row);
    }
    let live = LiveSnapshot::default();
    let mut app = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
    let folded = rendered(&mut app, 100, 30);
    dump("tasks (chain folded)", &folded);
    assert!(folded.contains("▸ +8 esperando a esta"), "{folded}");
    assert!(!folded.contains("Documentación") && !folded.contains("API de la CLI"), "{folded}");

    let fold = app.inbox_keys().iter().position(|key| key.0 == "chain:t-2").unwrap();
    app.move_inbox(fold as isize);
    press(&mut app, KeyCode::Enter);
    let open = rendered(&mut app, 100, 30);
    dump("tasks (chain open)", &open);
    assert!(open.contains("▾ +8 esperando a esta"), "{open}");
    let lines: Vec<&str> = open.lines().collect();
    let at = |needle: &str| lines.iter().position(|line| line.contains(needle)).unwrap_or_else(|| panic!("{needle}: {open}"));
    assert!(at("Versión en la TUI") < at("API de ajustes") && at("Fórmula de Homebrew") < at("API de la CLI")
        && at("API de la CLI") < at("Documentación"), "each after what it waits for: {open}");

    press(&mut app, KeyCode::Enter);
    let again = rendered(&mut app, 100, 30);
    assert!(again.contains("▸ +8 esperando a esta") && !again.contains("Documentación"), "{again}");
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
fn every_node_action_has_its_own_key() {
    let (domain, live) = busy();
    for key in ["node:a-3", "node:a-0"] {
        let actions = view_render::actions(&build(&domain, &live, key));
        assert!(!actions.is_empty(), "{key}");
        let keys: Vec<&str> = actions.iter().map(view_render::action_hint).collect();
        assert!(keys.iter().all(|hint| !hint.is_empty()), "{key}: {keys:?}");
        assert_eq!(keys.iter().collect::<BTreeSet<_>>().len(), keys.len(), "{key}: {keys:?}");
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
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("archive_done:"), "{sent:?}");
}

#[test]
fn fleet_footer_names_the_selected_nodes_real_keys() {
    let (domain, live) = busy();
    let mut app = app_on(Screen::Fleet, build(&domain, &live, "fleet"));
    app.sel = Some(Key("a-3".into()));
    app.on_msg(Msg::ViewCard {
        key: "node:a-3".into(),
        result: Ok(build(&domain, &live, "node:a-3")),
    });
    let footer = draw::footer_keys(&app);
    for (key, label) in [("r", "Mensaje"), ("x", "Cerrar nodo"), ("i", "Interrumpir")] {
        assert!(footer.iter().any(|(k, l)| k == key && l == label), "{key}: {footer:?}");
    }
    dump("fleet with node detail", &rendered(&mut app, 120, 30));
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
    run(Actor::Orchestrator { generation: 0 }, Command::CreateTask {
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
    assert!(welcome[0].contains("▌? Choose the welcome copy") && welcome[0].contains("Alondra"), "{welcome:?}");
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
    assert!(at("? DECIDE · Alondra") < at("Choose the welcome copy"));
    assert!(at("Choose the welcome copy") < at("The new empty state is ready"));
    assert!(at("The new empty state is ready") < at("Content"));
    assert!(at("Content") < at("Context"));
    assert!(at("Context") < at(" 1  ★ Ship the concise copy"));
    assert!(at(" 1  ★ Ship the concise copy") < at(" 2  Keep the warmer copy"));
    assert!(at(" 2  Keep the warmer copy") < at(" m  More… (1)"));
    assert!(at(" m  More… (1)") < at(" r  Reply"));
    let footer = draw::footer_keys(&app);
    assert_eq!(
        footer
            .iter()
            .map(|(key, _)| key.as_str())
            .filter(|key| matches!(*key, "1" | "2" | "3" | "m" | "r" | "⏎/o"))
            .collect::<Vec<_>>(),
        ["1", "2", "m", "r", "⏎/o"],
        "{footer:?}"
    );
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
    assert!(screen.contains("◆ AUTORIZAR"), "{screen}");
    assert!(screen.contains("Allow publishing Brew 0.2.36") && screen.contains("Restarts the daemon"), "{screen}");
    assert!(screen.contains("alondra update"), "the command is shown whole: {screen}");
    assert!(screen.contains(" 1  Permitir") && screen.contains(" 2  Denegar") && screen.contains(" r  Responder"), "{screen}");
}

#[test]
fn review_pr_detail_shows_what_blocks_it_and_offers_no_merge() {
    let mut app = english(blocked_pr());
    let screen = rendered(&mut app, 120, 46);
    dump("detail: Review PR, blocked", &screen);
    assert!(screen.contains("◎ REVIEW · PR #12"), "{screen}");
    assert!(screen.contains("▤ Diff · 4 files · +120 −30") && !screen.contains("0 B"), "{screen}");
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
    assert!(screen.contains("! PROBLEM") && screen.contains("The orchestrator run failed"), "{screen}");
    assert!(screen.contains(" 1  Retry") && screen.contains(" 2  Stop"), "{screen}");
}

#[test]
fn know_detail_only_offers_seen_and_the_reply() {
    let mut app = english(fixture_view("card-information.json"));
    let screen = rendered(&mut app, 120, 34);
    dump("detail: Know", &screen);
    assert!(screen.contains("· FYI") || screen.contains("· KNOW"), "{screen}");
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
fn clarify_is_a_reason_picker_then_an_optional_text() {
    let mut app = english(fixture_view("card-question.json"));
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Char('1'));
    let screen = rendered(&mut app, 120, 40);
    dump("detail: clarification reasons", &screen);
    assert!(screen.contains("REQUEST CLARIFICATION"), "{screen}");
    assert!(
        screen.contains(" 1  It is unclear")
            && screen.contains(" 2  It contradicts something")
            && screen.contains(" 3  It lacks context"),
        "{screen}"
    );
    press(&mut app, KeyCode::Char('2'));
    assert!(app.card_menu.is_none() && app.posted.borrow().is_empty(), "the text comes before it is sent");
    assert!(matches!(
        app.composer.as_ref().map(|composer| &composer.target),
        Some(Target::ViewAction { option_id: Some(reason), required: false, .. }) if reason == "contradicts"
    ));
    typed(&mut app, "The two copies disagree");
    press(&mut app, KeyCode::Enter);
    let sent = app.posted.borrow().clone();
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("clarify:"), "{sent:?}");
    assert_eq!(sent[0].1["option_id"], "contradicts");
    assert_eq!(sent[0].1["input"]["text"], "The two copies disagree");
}

#[test]
fn clarify_without_text_is_sent_with_just_the_reason() {
    let mut app = english(fixture_view("card-question.json"));
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Char('1'));
    press(&mut app, KeyCode::Char('1'));
    press(&mut app, KeyCode::Enter);
    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].1["option_id"], "unclear");
    assert_eq!(sent[0].1["input"], serde_json::json!({}));
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
fn what_is_written_in_the_cards_field_goes_with_the_option_pressed() {
    let mut app = english(fixture_view("card-question.json"));
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "Rotated both keys");
    press(&mut app, KeyCode::Esc);
    assert!(app.composer.is_none() && app.posted.borrow().is_empty(), "Esc keeps the words as a draft");
    assert!(app.flash.as_ref().is_some_and(|flash| flash.text.contains("press an option to send it with it")));
    app.flash = None;
    let screen = rendered(&mut app, 120, 60);
    dump("detail: a draft waiting for an option", &screen);
    assert!(screen.contains("«Rotated both keys» — goes with the option you press"), "{screen}");
    press(&mut app, KeyCode::Char('1'));
    let sent = app.posted.borrow().clone();
    assert!(app.composer.is_none(), "the option answers at once");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("ans:"), "{sent:?}");
    assert!(sent[0].1["option_id"].is_string(), "{sent:?}");
    assert_eq!(sent[0].1["input"]["text"], "Rotated both keys");
    assert!(app.drafts.is_empty(), "the words were sent once");
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
fn keys_beyond_the_buttons_do_nothing_and_a_menu_swallows_the_rest() {
    let mut app = english(fixture_view("card-information.json"));
    for key in ['2', '3', 'm', '9'] {
        press(&mut app, KeyCode::Char(key));
    }
    assert!(app.posted.borrow().is_empty() && app.composer.is_none() && app.card_menu.is_none());
    let mut app = english(fixture_view("card-question.json"));
    press(&mut app, KeyCode::Char('3'));
    assert!(app.posted.borrow().is_empty(), "a Decide with two buttons has no third");
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
    assert!(screen.contains(" m  Más… (1)"), "{screen}");
    let footer = draw::footer_keys(&app);
    assert!(footer.iter().any(|(key, label)| key == "r" && label == "responder"), "{footer:?}");
    assert!(footer.iter().any(|(key, label)| key == "⏎/o" && label == "abrir"), "{footer:?}");
    press(&mut app, KeyCode::Char('m'));
    assert!(rendered(&mut app, 120, 40).contains("MÁS"));
}

fn header(screen: &str) -> &str {
    screen.lines().find(|line| line.contains("Bandeja")).unwrap_or_default()
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
fn l_on_the_fleet_never_changes_the_limit() {
    let (domain, mut live) = busy();
    live.pacing = Some(views::LivePacing { working: 3, limit: 6, fixed: None, auto: 6, memory_free: Some(40), tight: false,
        queued: vec![], paused: vec![] });
    let mut app = app_on(Screen::Fleet, build(&domain, &live, "fleet"));
    app.on_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
    assert!(app.card_menu.is_none(), "the limit is changed with `alondra config set max_working_nodes`");
    assert!(app.posted.borrow().is_empty());
}

/// The golden Tasks list with a chain of two tasks waiting for the first, as the daemon folds it.
fn chain_of_two() -> views::View {
    let mut value = super::parity::view_fixture("tasks-tui.json");
    fn grow(rows: &mut serde_json::Value) {
        for row in rows.as_array_mut().into_iter().flatten() {
            if row["id"].as_str().is_some_and(|id| id.starts_with("chain:")) {
                let mut second = row["children"][0].clone();
                second["id"] = "t-badge-sync".into();
                second["link"]["key"] = "task:t-badge-sync".into();
                second["title"] = "Sync the badge across devices".into();
                row["children"].as_array_mut().unwrap().push(second);
                row["title"] = "+2 waiting for this".into();
            }
            if let Some(children) = row.get_mut("children") {
                grow(children);
            }
        }
    }
    for section in value["sections"].as_array_mut().unwrap() {
        grow(&mut section["items"]);
    }
    serde_json::from_value(value).unwrap()
}

#[test]
fn a_chain_starts_folded_and_its_key_unfolds_and_folds_it() {
    const MEMBERS: [&str; 2] = ["Decrement the badge on retraction", "Sync the badge across devices"];
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app_on(Screen::Tasks, chain_of_two());
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let folded = rendered(&mut app, 120, 34);
    dump("chain (folded)", &folded);
    assert!(folded.contains("▸ +2 waiting for this"), "{folded}");
    for member in MEMBERS {
        assert!(!folded.contains(member), "{member} is hidden while folded: {folded}");
    }
    assert!(!app.inbox_keys().iter().any(|key| key.0 == "t-badge-decrement" || key.0 == "t-badge-sync"),
        "a folded task cannot be selected");

    app.sel = app.inbox_keys().into_iter().find(|key| key.0.starts_with("chain:"));
    assert!(draw::footer_keys(&app).contains(&("o/⏎".into(), "unfold".into())), "{:?}", draw::footer_keys(&app));
    assert!(!draw::footer_keys(&app).iter().any(|(key, _)| key == "r" || key == "1"),
        "no other task's actions on the chain line: {:?}", draw::footer_keys(&app));
    press(&mut app, KeyCode::Char('o'));
    let open = rendered(&mut app, 120, 34);
    dump("chain (unfolded)", &open);
    assert!(open.contains("▾ +2 waiting for this"), "{open}");
    let lines: Vec<&str> = open.lines().collect();
    let indent = |needle: &str| lines.iter().find(|line| line.contains(needle)).and_then(|line| line.chars().position(|c| c == '↳'))
        .unwrap_or_else(|| panic!("{needle}: {open}"));
    for member in MEMBERS {
        assert!(indent(member) > indent("+2 waiting for this"), "{member} hangs under the chain line: {open}");
    }
    assert!(draw::footer_keys(&app).contains(&("o/⏎".into(), "fold".into())));
    press(&mut app, KeyCode::Char('o'));
    let again = rendered(&mut app, 120, 34);
    for member in MEMBERS {
        assert!(!again.contains(member), "{member} is hidden once folded again: {again}");
    }
}
