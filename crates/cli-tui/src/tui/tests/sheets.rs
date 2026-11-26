//! The sheets of a spec in the terminal: lanes, tables, a drawn diagram, a mockup `o` opens, and
//! the scenarios a long spec folds.
use super::screens::{card_screen, rendered};
use super::*;
use alondra_view_model as views;
use serde_json::{json, Value};

const MOCKUP: &str = "/files?path=files/images/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.png";

fn anchor(id: &str, quote: &str) -> Value {
    json!({"id": id, "quote": quote})
}

fn scenario(id: &str, title: &str, collapsed: bool) -> Value {
    json!({"id": format!("billing.partial/{id}"), "title": title, "kind": "test", "collapsed": collapsed,
        "mark": {"change": "new", "label": "nuevo"},
        "anchor": anchor(id, &format!("billing.partial/{id} · {title}")),
        "lanes": [
            {"kind": "given", "label": "Partiendo de", "steps": [{"text": "a paid order of 30 EUR"}]},
            {"kind": "when", "label": "Cuando", "steps": [{"text": "10 EUR are refunded", "table": [["amount", "currency"], ["10", "EUR"]]}]},
            {"kind": "then", "label": "Entonces", "steps": [{"text": "the customer receives 10 EUR"}, {"text": "20 EUR are left to refund"}]},
        ],
        "parts": [{"id": "p", "kind": "mockup", "name": "refund-sheet.png", "title": "Mockup «Refund sheet»", "alt": "Mockup",
            "picture": {"url": MOCKUP, "width": 1170, "height": 2532, "bytes": 9}, "anchor": anchor("p", "billing.partial/part · Mockup «Refund sheet»")}],
    })
}

fn sheet() -> views::SpecSheet {
    serde_json::from_value(json!({"id": "spec:billing.partial", "spec": "billing.partial", "title": "Devoluciones parciales",
        "mark": {"change": "new", "label": "nueva"}, "explanation": "Un cliente recupera cualquier importe hasta lo que queda del pedido.",
        "decisions": {"label": "Se apoya en", "items": [{"title": "keep-ledger"}]},
        "anchor": anchor("spec", "billing.partial · Devoluciones parciales"),
        "parts": [{"id": "d", "kind": "diagram", "name": "flow.mmd", "title": "Diagrama «Flow»", "alt": "Diagrama",
            "code": "flowchart LR\n  Order --> Refund\n", "anchor": anchor("d", "billing.partial · Diagrama «Flow»")}],
        "scenarios": [scenario("part", "Part of an order", false), scenario("two", "Two refunds at the same time", true)],
        "unchanged": {"label": "12 escenarios sin cambios", "scenarios": [scenario("same", "Stays the same", true)]},
    })).unwrap()
}

fn screen_of(app: &mut App) -> String {
    rendered(app, 100, 80)
}

fn reader_on_sheet() -> App {
    let view: views::View = serde_json::from_value({
        let mut view = super::parity::view_fixture("card-question.json");
        view["sections"] = json!([{"id": "specs", "title": "Specs", "collapsed": false, "items": [Value::from(serde_json::to_value(views::Item::SpecSheet(Box::new(sheet()))).unwrap())]}]);
        view
    }).unwrap();
    let mut app = card_screen(view, crate::i18n::Lang::Es);
    app.open_reader();
    app
}

fn lines(app: &App, blocks: &[md::Block]) -> String {
    let _ = app;
    md::render(blocks, 90).lines.iter().map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect::<String>()).collect::<Vec<_>>().join("\n")
}

#[test]
fn a_spec_sheet_draws_its_frame_lanes_tables_diagram_and_mockup_and_folds_the_rest() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let app = App::new(std::sync::mpsc::sync_channel(1).0);
    let drawn = lines(&app, &sheet::spec_blocks(&sheet(), &Default::default()));
    for wanted in ["┌ Devoluciones parciales", "billing.partial · nueva ┐", "Se apoya en: “keep-ledger”", "▾ Part of an order", "test · nuevo",
        "Partiendo de  a paid order of 30 EUR", "Cuando        10 EUR are refunded", "Entonces      the customer receives 10 EUR", "              20 EUR are left to refund",
        "amount │ currency", "▣ Mockup «Refund sheet» 1170×2532 · o la abre", "▸ Two refunds at the same time", "· 12 escenarios sin cambios", "└──"] {
        assert!(drawn.contains(wanted), "lacks «{wanted}»:\n{drawn}");
    }
    assert!(!drawn.contains("Stays the same"), "the unchanged scenarios are one line:\n{drawn}");
    assert!(drawn.contains('▶') || drawn.contains('►'), "the Mermaid diagram is drawn, not shown as source:\n{drawn}");
    assert!(!drawn.contains("flowchart LR"), "{drawn}");
}

#[test]
fn a_narrow_terminal_keeps_every_line_inside_its_width() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    for width in [40, 60] {
        let rendered = md::render(&sheet::spec_blocks(&sheet(), &Default::default()), width);
        for line in &rendered.lines {
            let used: usize = line.spans.iter().map(|span| md::str_width(&span.content)).sum();
            assert!(used <= width, "{used} > {width}: {line:?}");
        }
    }
}

fn press(app: &mut App, key: char) {
    app.on_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char(key)));
}

fn on_block(app: &mut App, starts: &str) {
    let reader = app.reader.as_mut().unwrap();
    reader.cursor = reader.blocks.iter().position(|block| block.plain().starts_with(starts) || block.quote().starts_with(starts)).unwrap_or_else(|| panic!("no block {starts}"));
}

#[test]
fn in_the_reader_z_unfolds_a_scenario_and_o_opens_a_mockup() {
    let mut app = reader_on_sheet();
    assert!(screen_of(&mut app).contains("▸ Two refunds at the same time"));
    on_block(&mut app, "billing.partial/two");
    press(&mut app, 'z');
    assert!(screen_of(&mut app).contains("▾ Two refunds at the same time"));
    on_block(&mut app, "12 escenarios sin cambios");
    press(&mut app, 'z');
    assert!(screen_of(&mut app).contains("Stays the same"));
    on_block(&mut app, "billing.partial/part · Mockup");
    let spawned = app.spawned.borrow().len();
    press(&mut app, 'o');
    assert!(app.reader.is_some(), "o on a mockup opens it, it does not leave the reader");
    assert_eq!(app.spawned.borrow().len(), spawned + 1);
    on_block(&mut app, "billing.partial · Devoluciones parciales");
    press(&mut app, 'o');
    assert!(app.reader.is_none(), "anywhere else o still leaves the reader");
}

#[test]
fn a_comment_stays_on_its_part_when_the_scenarios_fold_around_it() {
    let mut app = reader_on_sheet();
    on_block(&mut app, "billing.partial/part · Mockup");
    let (key, block, quote) = {
        let reader = app.reader.as_ref().unwrap();
        (reader.key.clone(), reader.cursor, reader.blocks[reader.cursor].quote())
    };
    app.reviews.entry(key.clone()).or_default().comments.push(Comment { block, quote: quote.clone(), text: "Show what is left".into() });
    on_block(&mut app, "billing.partial/part · Part of an order");
    press(&mut app, 'z');
    press(&mut app, 'z');
    let comment = &app.reviews[&key].comments[0];
    assert_eq!(app.reader.as_ref().unwrap().blocks[comment.block].quote(), quote);
}

#[test]
fn a_button_of_the_reader_that_opens_a_screen_leaves_the_reader_for_it() {
    let mut app = reader_on_sheet();
    let view = app.card_view.as_mut().unwrap();
    let views::Item::Card(card) = &mut serde_json::from_value::<views::Item>(super::parity::view_fixture("card-question.json")["sections"][0]["items"][0].clone()).unwrap() else { panic!() };
    let mut edit = card.actions[0].clone();
    edit.on_client().id = String::new();
    edit.on_client().link = Some(views::Link::View { key: "task_specs:t-1".into() });
    card.actions = vec![edit];
    view.sections.insert(0, views::Section { id: "card".into(), title: None, count: None, collapsed: false, items: vec![views::Item::Card(card.clone())], card_ids: vec![], card_waits: vec![], more: None, style: None, actions: vec![] });
    app.open_reader();
    press(&mut app, '1');
    assert!(app.reader.is_none(), "the editor the button opens is on screen, not under the document it left");
    assert_eq!(app.view_navigation.last().map(String::as_str), Some("task_specs:t-1"));
}
