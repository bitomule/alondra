//! Every key the key bar shows does what it says, and every key that does something is listed.
//! The bar, the help screen and the handler read one table (`bindings.rs`); these tests press each
//! key of each state and compare what happened with that table.

use super::bindings::{Act, Shown, same};
use super::screens::{app_on, build, busy, task_screen};
use super::*;

const QUESTION: &str = "a98d034b-249e-4487-a89b-57de38c26372";

fn view(name: &str) -> ViewDocument {
    serde_json::from_value(super::parity::view_fixture(name)).unwrap()
}

fn on(screen: Screen, home: &str) -> App {
    app_on(screen, view(home))
}

fn card(app: &mut App, row: &str, name: &str) {
    app.sel = Some(Key(row.into()));
    let document = view(name);
    app.on_msg(Msg::ViewCard { key: document.key.clone(), result: Ok(document) });
}

fn draw_text(app: &mut App) -> String {
    let backend = ratatui::backend::TestBackend::new(150, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw::draw(frame, app)).unwrap();
    terminal.backend().buffer().content().iter().map(|cell| cell.symbol()).collect()
}

/// What the user can tell changed after a key: the screen without a refusal's flash, what was sent
/// or asked of the daemon, and where the TUI stands.
fn observed(app: &mut App) -> String {
    let flash = app.flash.take();
    let screen = draw_text(app);
    app.flash = flash;
    format!(
        "{screen}|{}|{}|{:?}|{:?}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{}|{}|{:?}|{}|{:?}",
        app.posted.borrow().len(),
        app.spawned.borrow().len(),
        app.sel.as_ref().map(|key| &key.0),
        app.view_navigation,
        app.composer.is_some(),
        app.reader.is_some(),
        app.modal.is_some(),
        app.card_menu.is_some(),
        app.help,
        app.open_rows,
        app.open_sections,
        app.card_view.as_ref().map(|view| &view.key),
        app.cmdline.is_some(),
        app.search.is_some(),
        app.reader.as_ref().map(|reader| (reader.cursor, reader.scroll, reader.find.is_some(), reader.picking)),
        app.schedules_open,
        (app.detail_scroll, app.list_scroll, app.focus == Pane::Detail, app.screen == Screen::Inbox, app.screen == Screen::Tasks),
    )
}

/// Whether `key` changed something the user can tell, other than a flash saying it could not.
fn works(make: &dyn Fn() -> App, key: KeyEvent) -> bool {
    let mut app = make();
    let before = observed(&mut app);
    app.flash = None;
    let flow = app.on_key(key);
    let after = observed(&mut app);
    !matches!(flow, Flow::Go)
        || after != before
        || app.flash.as_ref().is_some_and(|flash| flash.kind != FlashKind::Error)
}

fn plain(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn universe() -> Vec<KeyEvent> {
    let mut keys: Vec<KeyEvent> = ('a'..='z')
        .chain('A'..='Z')
        .chain('0'..='9')
        .chain("/?:[]-+ .,;'\"!@#$%&*()=_<>|\\`~".chars())
        .map(|c| plain(KeyCode::Char(c)))
        .collect();
    keys.extend(
        [
            KeyCode::Enter, KeyCode::Esc, KeyCode::Tab, KeyCode::BackTab, KeyCode::Backspace, KeyCode::Left,
            KeyCode::Right, KeyCode::Up, KeyCode::Down, KeyCode::Home, KeyCode::End, KeyCode::PageUp,
            KeyCode::PageDown, KeyCode::Delete,
        ]
        .map(plain),
    );
    keys.extend(('a'..='z').filter(|c| *c != 'c').map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)));
    keys.push(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    keys
}

/// What one state shows that does not hold, or nothing. `typing` states send letters to a text box,
/// so only their shown keys are checked.
fn audit(state: &str, make: &dyn Fn() -> App, typing: bool) -> Vec<String> {
    let mut app = make();
    let _ = draw_text(&mut app);
    let table = app.bindings();
    let mut problems = Vec::new();
    if typing {
        for (hint, label) in draw::footer_keys(&app) {
            let key = match hint.as_str() {
                "⏎" => plain(KeyCode::Enter),
                "Esc" => plain(KeyCode::Esc),
                "Tab" => plain(KeyCode::Tab),
                _ => continue,
            };
            if !works(make, key) {
                problems.push(format!("{state}: shows «{hint} {label}» but it does nothing"));
            }
        }
        return problems;
    }
    for (index, binding) in table.0.iter().enumerate() {
        for key in &binding.keys {
            let reached = table.0.iter().position(|other| other.keys.iter().any(|k| same(k, key)));
            if reached != Some(index) {
                problems.push(format!("{state}: «{} {}» is shadowed by another entry", binding.hint, binding.label));
            }
        }
        let worked: Vec<bool> = binding.keys.iter().map(|key| works(make, *key)).collect();
        let ok = match binding.act {
            Act::Move => worked.iter().any(|worked| *worked),
            _ => worked.iter().all(|worked| *worked),
        };
        if !ok {
            problems.push(format!("{state}: shows «{} {}» but {:?} does nothing", binding.hint, binding.label,
                binding.keys.iter().zip(&worked).filter(|(_, worked)| !**worked).map(|(key, _)| key.code).collect::<Vec<_>>()));
        }
    }
    if !table.0.iter().any(|binding| binding.shown == Shown::Bar) {
        problems.push(format!("{state}: the key bar is empty"));
    }
    for key in universe() {
        if table.find(&key).is_none() && works(make, key) {
            problems.push(format!("{state}: {:?} {:?} works but no help lists it", key.code, key.modifiers));
        }
    }
    let bar = draw::footer_keys(&app);
    for binding in table.0.iter().filter(|binding| binding.shown == Shown::Bar) {
        if !bar.contains(&(binding.hint.clone(), binding.label.clone())) {
            problems.push(format!("{state}: «{} {}» is missing from the key bar", binding.hint, binding.label));
        }
    }
    app.help = true;
    let help: String = draw::help_lines(&app).iter().map(|line| format!("{line}\n")).collect();
    app.help = false;
    for binding in &table.0 {
        if !help.contains(&binding.label) {
            problems.push(format!("{state}: «{} {}» is missing from the help screen", binding.hint, binding.label));
        }
    }
    problems
}

type State = (&'static str, bool, Box<dyn Fn() -> App>);

const README: &str = "Intro without links.\n\nRead [the guide](https://x.y/guide).\n\n<a href=\"https://ci.x/run\"><img alt=\"build\" src=\"https://img.x/b.svg\"></a> ![shot](docs/shot.png) [notes](file:///etc/hosts)\n";

fn reader_on(parts: Vec<Part>, block_with: usize) -> App {
    let mut app = on(Screen::Inbox, "home-tui.json");
    app.reader = Some(Reader {
        key: Key("result:readme".into()),
        agent: String::new(),
        task_id: None,
        title: "README".into(),
        kind: "Result".into(),
        glyph: "✓",
        color: ratatui::style::Color::Green,
        parts,
        blocks: vec![],
        cursor: 0,
        scroll: 0,
        ranges: vec![],
        view_h: 0,
        total: 0,
        find: None,
        picking: false,
    });
    app.rebuild_reader();
    let reader = app.reader.as_mut().unwrap();
    reader.cursor = reader.blocks.iter().position(|block| block.targets.len() == block_with).unwrap();
    app
}

fn readme(block_with: usize) -> App {
    reader_on(vec![Part::Text(README.into())], block_with)
}

fn reader_on_question(comment: bool) -> App {
    let mut app = on(Screen::Inbox, "home-tui.json");
    card(&mut app, QUESTION, "card-document-question-es-tui.json");
    app.open_reader();
    if comment {
        let commentable = app.reader.as_ref().unwrap().blocks.iter().position(|block| block.commentable()).unwrap();
        app.reader.as_mut().unwrap().cursor = commentable;
        app.on_key(plain(KeyCode::Char('c')));
        for c in "Split it".chars() {
            app.on_key(plain(KeyCode::Char(c)));
        }
        app.on_key(plain(KeyCode::Enter));
    }
    app
}

fn tasks() -> App {
    let (domain, live) = busy();
    task_screen(&domain, &live, "r-3", "t-3")
}

fn states() -> Vec<State> {
    vec![
        ("inbox, question", false, Box::new(|| {
            let mut app = on(Screen::Inbox, "home-tui.json");
            card(&mut app, QUESTION, "card-document-question-es-tui.json");
            app
        })),
        ("inbox, question, detail focused", false, Box::new(|| {
            let mut app = on(Screen::Inbox, "home-tui.json");
            card(&mut app, QUESTION, "card-document-question-es-tui.json");
            app.focus = Pane::Detail;
            app
        })),
        ("inbox, its More menu", false, Box::new(|| {
            let mut app = on(Screen::Inbox, "home-tui.json");
            card(&mut app, QUESTION, "card-document-question-es-tui.json");
            app.on_key(plain(KeyCode::Char('m')));
            app
        })),
        ("inbox, a notice", false, Box::new(|| {
            let mut app = on(Screen::Inbox, "home-tui.json");
            card(&mut app, "cc2d034b-249e-4487-a89b-57de38c26372", "card-know-command-es-tui.json");
            app
        })),
        ("inbox, a delivery with a PR", false, Box::new(|| {
            let mut app = on(Screen::Inbox, "home-tui.json");
            card(&mut app, "hf-weekly-2026-09-30", "card-delivery.json");
            app
        })),
        ("inbox, empty", false, Box::new(|| {
            let domain = alondra_core::domain::DomainState::new("keys");
            app_on(Screen::Inbox, build(&domain, &Default::default(), "home"))
        })),
        ("inbox, loading", false, Box::new(|| {
            let (tx, _rx) = std::sync::mpsc::sync_channel(64);
            crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
            App::new(tx)
        })),
        ("tasks, a working task", false, Box::new(tasks)),
        ("tasks, the Finished header", false, Box::new(|| {
            let (domain, live) = busy();
            let mut app = app_on(Screen::Tasks, build(&domain, &live, "tasks"));
            app.sel = Some(Key("section:finished".into()));
            app
        })),
        ("tasks, a folded chain", false, Box::new(|| {
            let mut app = on(Screen::Tasks, "tasks-tui.json");
            app.sel = app.inbox_keys().into_iter().find(|key| key.0.starts_with("chain:"));
            app
        })),
        ("tasks, an unfolded chain", false, Box::new(|| {
            let mut app = on(Screen::Tasks, "tasks-tui.json");
            app.sel = app.inbox_keys().into_iter().find(|key| key.0.starts_with("chain:"));
            app.on_key(plain(KeyCode::Char('o')));
            app
        })),
        ("tasks, scheduled", false, Box::new(|| {
            let mut app = on(Screen::Tasks, "tasks-tui.json");
            app.on_key(plain(KeyCode::Char('s')));
            app.on_msg(Msg::ViewHome(Ok(view("schedules-ios.json"))));
            app.reconcile();
            app
        })),
        ("tasks, searching", false, Box::new(|| {
            let mut app = on(Screen::Tasks, "tasks-tui.json");
            app.on_key(plain(KeyCode::Char('/')));
            app.on_key(plain(KeyCode::Char('x')));
            app.on_key(plain(KeyCode::Enter));
            app
        })),
        ("reader, a document question", false, Box::new(|| reader_on_question(false))),
        ("reader, a commented block", false, Box::new(|| reader_on_question(true))),
        ("reader, searching", false, Box::new(|| {
            let mut app = reader_on_question(false);
            app.on_key(plain(KeyCode::Char('/')));
            for c in "store".chars() {
                app.on_key(plain(KeyCode::Char(c)));
            }
            app.on_key(plain(KeyCode::Enter));
            app
        })),
        ("reader, a block with no link", false, Box::new(|| readme(0))),
        ("reader, a block that opens one link", false, Box::new(|| readme(1))),
        ("reader, a block that opens several things", false, Box::new(|| readme(4))),
        ("reader, picking what to open", false, Box::new(|| {
            let mut app = readme(4);
            app.on_key(plain(KeyCode::Char('v')));
            app
        })),
        ("reader, the card is gone", false, Box::new(|| {
            let mut app = reader_on_question(true);
            app.card_view = None;
            app
        })),
        ("a confirmation", false, Box::new(|| {
            let mut app = tasks();
            app.modal = Some(Modal {
                title: "Close it?".into(),
                body: String::new(),
                button: "Close".into(),
                destructive: true,
                then: ModalThen::Action {
                    action: Box::new(serde_json::from_value(serde_json::json!({"id":"close:x@1","label":"Close",
                        "style":"destructive","presentation":"button","input":{"kind":"none"},"attachments":false,
                        "enabled":true})).unwrap()),
                    option_id: None,
                    selected: Key("t-3".into()),
                    epoch: "e".into(),
                },
            });
            app
        })),
        ("the composer", true, Box::new(|| {
            let mut app = tasks();
            app.on_key(plain(KeyCode::Char('n')));
            app.on_msg(Msg::FormView { view: "new_request".into(), project: None, result: Ok(view("new-request-ios.json")) });
            for c in "Do it".chars() {
                app.on_key(plain(KeyCode::Char(c)));
            }
            app
        })),
    ]
}

#[test]
fn every_key_the_bar_shows_works_and_every_working_key_is_listed() {
    let mut problems = Vec::new();
    for (state, typing, make) in states() {
        let mut app = make();
        let _ = draw_text(&mut app);
        println!("== {state}: {:?}", draw::footer_keys(&app));
        problems.extend(audit(state, make.as_ref(), typing));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_reader_on_a_document_question_offers_each_key_once_and_its_digits_answer() {
    let mut app = reader_on_question(false);
    let _ = draw_text(&mut app);
    let bar = draw::footer_keys(&app);
    for (hint, _) in &bar {
        assert_eq!(bar.iter().filter(|(other, _)| other == hint).count(), 1, "{hint} twice: {bar:?}");
    }
    let labels: Vec<&str> = bar.iter().map(|(_, label)| label.as_str()).collect();
    for absent in ["Pedir aclaración", "No se entiende", "Se contradice", "Le falta contexto"] {
        assert!(!labels.contains(&absent), "{absent}: {bar:?}");
    }
    assert!(bar.contains(&("c".into(), "comentar".into())), "{bar:?}");
    assert!(bar.contains(&("3".into(), "Pedir cambios".into())), "{bar:?}");
    app.on_key(plain(KeyCode::Char('1')));
    assert_eq!(app.posted.borrow().last().map(|(_, body)| body["option_id"].clone()),
        Some(serde_json::json!("clear")), "1 answers the option the bar names");
}

fn opened() -> Vec<String> {
    OPENED.with(|opened| std::mem::take(&mut *opened.borrow_mut()))
}

#[test]
fn v_offers_and_opens_only_what_the_selected_block_opens() {
    opened();
    let mut app = readme(0);
    let _ = draw_text(&mut app);
    assert!(!draw::footer_keys(&app).iter().any(|(hint, _)| hint == "v"));

    let mut app = readme(1);
    let _ = draw_text(&mut app);
    assert!(draw::footer_keys(&app).contains(&("v".into(), "abrir".into())));
    app.on_key(plain(KeyCode::Char('v')));
    assert_eq!(opened(), vec!["https://x.y/guide"]);

    app.on_key(plain(KeyCode::Char('j')));
    app.on_key(plain(KeyCode::Char('v')));
    assert!(app.reader.as_ref().unwrap().picking);
    assert!(app.flash.is_none(), "the flash of the last link would cover the list");
    assert!(opened().is_empty());
    let screen = draw_text(&mut app);
    for entry in ["1 ↗ build", "2 📷  build", "3 📷  shot", "4 ↗ notes"] {
        assert!(screen.contains(entry), "{entry} not in the list:\n{screen}");
    }
    assert_eq!(draw::footer_keys(&app).first(), Some(&("1-4".into(), "abrir".into())));
    app.on_key(plain(KeyCode::Char('2')));
    assert_eq!(opened(), vec!["https://img.x/b.svg"]);
    assert!(!app.reader.as_ref().unwrap().picking);

    for (digit, why) in [('3', "Mac que escribió"), ('4', "enlaces web y de correo")] {
        app.on_key(plain(KeyCode::Char('v')));
        app.on_key(plain(KeyCode::Char(digit)));
        assert!(opened().is_empty(), "{digit} opened something");
        let flash = app.flash.take().unwrap();
        assert!(flash.kind == FlashKind::Error && flash.text.contains(why), "{digit}: {}", flash.text);
    }
}

#[test]
fn v_on_a_stored_file_fetches_it_to_open() {
    let parts = vec![Part::File { name: "build.zip".into(), route: "/files/build.zip".into() }];
    let mut app = reader_on(parts, 0);
    app.files.insert("/files/build.zip".into(), Ok(files::FileBody::Binary { bytes: 2048 }));
    app.rebuild_reader();
    let reader = app.reader.as_mut().unwrap();
    reader.cursor = reader.blocks.iter().position(|block| !block.targets.is_empty()).unwrap();
    let spawned = app.spawned.borrow().len();
    app.on_key(plain(KeyCode::Char('v')));
    assert_eq!(app.spawned.borrow().len(), spawned + 1);
    assert!(app.flash.as_ref().is_some_and(|flash| flash.text.contains("build.zip")));
}
