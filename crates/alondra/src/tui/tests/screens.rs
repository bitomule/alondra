//! The four list screens and the task actions, rendered from views the daemon's own builder makes
//! for the terminal, the way the owner sees them.

use super::*;
use alondra_core::domain_v2::{Agent, DomainState, Request, Task, TaskState};
use alondra_core::views::{self, Client, Lang, LiveNode, LiveSnapshot, RenderContext, Transport};
use std::collections::{BTreeMap, BTreeSet};

const NIGHTLY: &str = "Por qué se atascó la tarea nocturna";
const ODD_STATE: &str = "Estado raro del nodo Parcel";
const WEEKLY: &str = "Parcel · mejora semanal";

fn task_screen(domain: &DomainState, live: &LiveSnapshot, request: &str, task: &str) -> App {
    let mut app = app_on(Screen::Tasks, build(domain, live, "tasks"));
    app.sel = Some(Key(request.into()));
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

fn minutes_ago(minutes: i64) -> String {
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
    }
}

/// Four nodes with one task each, three finished and one working, plus a scheduled task nobody
/// has started: the owner's fleet on 2026-10-03.
fn busy() -> (DomainState, LiveSnapshot) {
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
            Agent { id: agent.as_str().into(), name: title.into(), connected: true },
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

fn build(domain: &DomainState, live: &LiveSnapshot, key: &str) -> views::View {
    views::build(
        key,
        RenderContext {
            domain,
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

fn app_on(screen: Screen, home: views::View) -> App {
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
fn fleet_lists_each_node_once_with_its_state_and_no_zero_counters() {
    let (domain, live) = busy();
    let mut app = app_on(Screen::Fleet, build(&domain, &live, "fleet"));
    let screen = rendered(&mut app, 100, 24);
    dump("fleet", &screen);
    for title in [NIGHTLY, ODD_STATE] {
        assert_eq!(screen.matches(title).count(), 1, "{title}: {screen}");
    }
    assert!(!screen.contains("↳"), "{screen}");
    assert!(!screen.contains("· 0"), "{screen}");
    assert!(screen.contains("Terminado") && screen.contains("Trabajando"), "{screen}");
}

#[test]
fn tasks_list_one_row_per_single_task_request_with_people_times() {
    let (domain, live) = busy();
    let mut app = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
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
                footer.iter().any(|(shown, _)| *shown == draw::footer_key(key)),
                "{task}: {key} missing from {footer:?}"
            );
        }
        if task == "t-0" {
            dump("task actions (finished task)", &rendered(&mut app, 130, 30));
        }
    }
}

#[test]
fn every_node_and_orchestrator_action_has_its_own_key() {
    let (domain, live) = busy();
    for key in ["node:a-3", "node:a-0", "node:orchestrator"] {
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
