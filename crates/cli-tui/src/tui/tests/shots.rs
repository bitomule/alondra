//! Pictures of the PR card flow and the usage indicator for review. Each state is drawn by the real terminal code into a
//! buffer and saved as an HTML page; `make tui-shots` turns the pages into PNGs.

use super::parity::view_fixture;
use super::*;
use alondra_domain_rules::Refusal;
use alondra_views::Lang as ViewLang;
use ratatui::style::{Color, Modifier};

const WIDTH: u16 = 120;
const HEIGHT: u16 = 34;
const CARD: &str = "hf-weekly-2026-09-30";

/// The card's own words as the server writes them in Spanish (`views/card.rs`).
const SPANISH: [(&str, &str); 7] = [
    ("Squash and merge", "Squash and merge"),
    ("Merge with a merge commit", "Hacer merge con merge commit"),
    ("Merging…", "Haciendo merge…"),
    ("Request changes", "Pedir cambios"),
    ("Close without merging", "Cerrar sin hacer merge"),
    ("Open on GitHub", "Abrir en GitHub"),
    ("Reply", "Responder"),
];

fn in_spanish(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            if let Some((_, spanish)) = SPANISH.iter().find(|(english, _)| english == text) {
                *text = (*spanish).to_owned();
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(in_spanish),
        serde_json::Value::Object(map) => map.values_mut().for_each(in_spanish),
        _ => {}
    }
}

fn app_with_the_pr_card(lang: crate::i18n::Lang) -> App {
    crate::i18n::set_for_this_thread(lang);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::home(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    app.sel = Some(Key(CARD.into()));
    the_card_arrives(&mut app, lang);
    app
}

fn the_card_arrives(app: &mut App, lang: crate::i18n::Lang) {
    let mut card = view_fixture("card-pr.json");
    card["key"] = format!("card:{CARD}").into();
    if lang == crate::i18n::Lang::Es {
        in_spanish(&mut card["sections"]);
    }
    app.on_msg(Msg::ViewCard { key: format!("card:{CARD}"), result: Ok(serde_json::from_value(card).unwrap()) });
}

fn merge_sent(lang: crate::i18n::Lang) -> (App, serde_json::Value) {
    let mut app = app_with_the_pr_card(lang);
    app.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    let request = app.posted.borrow().last().unwrap().1.clone();
    (app, request)
}

fn answered(lang: crate::i18n::Lang, outcome: Outcome) -> App {
    let (mut app, request) = merge_sent(lang);
    app.on_msg(Msg::ViewAction { request, result: Ok(ViewActionReply::Outcome(outcome)) });
    the_card_arrives(&mut app, lang);
    app
}

/// The PR card while its task still works on a message of the user; `press` asks for the merge.
fn merge_over_an_unanswered_message(lang: crate::i18n::Lang, press: bool) -> App {
    let mut app = app_with_the_pr_card(lang);
    let mut card = view_fixture(if lang == crate::i18n::Lang::Es { "card-merge-unanswered-es-tui.json" } else { "card-merge-unanswered-tui.json" });
    card["key"] = format!("card:{CARD}").into();
    app.on_msg(Msg::ViewCard { key: format!("card:{CARD}"), result: Ok(serde_json::from_value(card).unwrap()) });
    if press {
        app.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    }
    app
}

fn tasks(lang: crate::i18n::Lang) -> App {
    crate::i18n::set_for_this_thread(lang);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.on_msg(Msg::home(Ok(serde_json::from_value(view_fixture("tasks-tui.json")).unwrap())));
    app.reconcile();
    app
}

fn usage(lang: crate::i18n::Lang, sheet: bool) -> App {
    let mut app = tasks(lang);
    app.view_status = Some(super::parity::status_with(super::parity::usage_gauges(lang)));
    if sheet {
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    }
    app
}

/// A crash report the final check refused, offering to build the rest without the file that failed.
fn crash_report_refused(lang: crate::i18n::Lang) -> App {
    let mut app = tasks(lang);
    let es = lang == crate::i18n::Lang::Es;
    app.build_crash_report("other".into(), if es { "El nodo se paró tras dormir el Mac" } else { "The node stopped after the Mac slept" }.into());
    let built = serde_json::json!({
        "schema": "alondra.report/1",
        "id": "r-20261027-0a1b2c3d",
        "dir": "/Users/ana/.alondra/reports/r-20261027-0a1b2c3d",
        "compressed_bytes": 48_000,
        "manifest": {
            "window": {"hours": 72},
            "sections": [
                {"name": "versions.json", "bytes": 210, "sha256": "a"},
                {"name": "machine.json", "bytes": 1_204, "sha256": "b"},
                {"name": "daemon.log", "bytes": 180_000, "sha256": "c"},
            ],
        },
        "check": {
            "passed": false,
            "hits": [{"section": "machine.json", "line": 14, "what": if es { "un email, en el campo launchd_job.ana@example.com" } else { "an email, in the field launchd_job.ana@example.com" }}],
            "send_rest_without": ["machine.json"],
        },
        "repo": "owner/reports",
    });
    app.on_msg(Msg::Report(super::crash_report::Answer::Built(Ok(built))));
    app
}

#[test]
#[ignore = "writes the review pictures: make tui-shots"]
fn tui_shots() {
    let out = std::path::PathBuf::from(
        std::env::var("TEST_UNDECLARED_OUTPUTS_DIR").expect("run through Bazel: make tui-shots"),
    );
    for (lang, view_lang, name) in [
        (crate::i18n::Lang::Es, ViewLang::Es, "es"),
        (crate::i18n::Lang::En, ViewLang::En, "en"),
    ] {
        let es = lang == crate::i18n::Lang::Es;
        let changed = Outcome::changed(
            if es { "La PR #12 tiene commits nuevos" } else { "PR #12 has new commits" }.into(),
            if es {
                "Revisaste una versión anterior. Envíalo igualmente o abre la PR para ver qué cambió."
            } else {
                "You reviewed an older version. Send anyway, or open the PR to see what changed."
            }
            .into(),
            alondra_views::send_anyway(
                format!("merge:{CARD}@2~0123abcd"),
                if es { "Enviar igualmente" } else { "Send anyway" },
            ),
        );
        let shots = [
            ("1-pr-card", app_with_the_pr_card(lang)),
            ("2-merging", merge_sent(lang).0),
            ("3-refused", answered(lang, Outcome::refused(&Refusal::UpstreamPrOpen { number: 11 }, view_lang))),
            ("4-changed", answered(lang, changed)),
        ];
        let shots = shots.into_iter().chain([("5-tasks", tasks(lang)), ("6-usage", usage(lang, false)), ("7-usage-sheet", usage(lang, true)),
            ("10-merge-unanswered", merge_over_an_unanswered_message(lang, false)), ("11-merge-unanswered-asks", merge_over_an_unanswered_message(lang, true))]);
        for (shot, mut app) in shots {
            let html = page(&mut app);
            std::fs::write(out.join(format!("{shot}-{name}.html")), html).unwrap();
        }
        let mut uncheckable = app_with_the_pr_card(lang);
        let mut card = view_fixture(if es { "card-pr-uncheckable-es-tui.json" } else { "card-pr-uncheckable-tui.json" });
        card["key"] = format!("card:{CARD}").into();
        uncheckable.on_msg(Msg::ViewCard { key: format!("card:{CARD}"), result: Ok(serde_json::from_value(card).unwrap()) });
        let mut joined = answered(lang, Outcome::refused(&Refusal::PrSuperseded, view_lang));
        let mut not_checked = answered(lang, Outcome::refused(&Refusal::PrNotCheckedYet, view_lang));
        for (shot, app) in [("13-pr-uncheckable", &mut uncheckable), ("14-joined-copy-refused", &mut joined), ("15-not-checked-yet-refused", &mut not_checked)] {
            std::fs::write(out.join(format!("{shot}-{name}.html")), page(app)).unwrap();
            std::fs::write(out.join(format!("{shot}-light-{name}.html")), light_page(app)).unwrap();
        }
        let html = page(&mut crash_report_refused(lang));
        std::fs::write(out.join(format!("12-crash-report-refused-{name}.html")), html).unwrap();
        let review = alondra_domain_rules::Refusal::MergeBlocked { number: 12, blockers: vec![alondra_domain_rules::MergeBlocker::ReviewRequired] };
        let html = page(&mut merge_waits(lang, &review));
        std::fs::write(out.join(format!("13-merge-waits-for-review-{name}.html")), html).unwrap();
        let html = page(&mut merge_waits(lang, &alondra_domain_rules::Refusal::PrNotCheckedYet));
        std::fs::write(out.join(format!("14-merge-waits-for-identity-{name}.html")), html).unwrap();
        let html = page(&mut task_on_another_model(lang));
        std::fs::write(out.join(format!("9-task-model-{name}.html")), html).unwrap();
        let unknown = alondra_views::unknown_outcome(alondra_views::action_outcome::UnknownReason::Interrupted, view_lang);
        let html = page(&mut answered(lang, unknown));
        std::fs::write(out.join(format!("8-outcome-unknown-{name}.html")), html).unwrap();
    }
}

/// The PR card while GitHub requires a review before the PR can merge: no merge is offered and the card says why.
fn merge_waits(lang: crate::i18n::Lang, because: &alondra_domain_rules::Refusal) -> App {
    let mut app = app_with_the_pr_card(lang);
    let mut card = view_fixture("card-pr.json");
    card["key"] = format!("card:{CARD}").into();
    if lang == crate::i18n::Lang::Es {
        in_spanish(&mut card["sections"]);
    }
    let waits = alondra_views::refusal_text(because, if lang == crate::i18n::Lang::Es { ViewLang::Es } else { ViewLang::En });
    let item = &mut card["sections"][0]["items"][0];
    let merge = item["actions"].as_array_mut().unwrap().iter_mut().find(|action| action["id"].as_str().is_some_and(|id| id.starts_with("merge:"))).unwrap();
    merge["enabled"] = false.into();
    merge["disabled_reason"] = waits.into();
    if *because == alondra_domain_rules::Refusal::PrNotCheckedYet {
        item["more"] = serde_json::json!([]);
    }
    app.on_msg(Msg::ViewCard { key: format!("card:{CARD}"), result: Ok(serde_json::from_value(card).unwrap()) });
    app
}

fn task_on_another_model(lang: crate::i18n::Lang) -> App {
    let mut app = tasks(lang);
    let key = "task:t-651c38484cd2f34e74d5107ba05f7f15";
    app.view_navigation.push(key.into());
    let fixture = if lang == crate::i18n::Lang::Es { "task-model-es-tui.json" } else { "task-model-tui.json" };
    app.on_msg(Msg::ViewCard { key: key.into(), result: Ok(serde_json::from_value(view_fixture(fixture)).unwrap()) });
    app
}

fn page(app: &mut App) -> String {
    themed_page(app, "#16181d", "#d7dae0")
}

/// The same screen in a terminal with a light profile: what the TUI leaves to the terminal's own
/// colours turns light, and its named colours stay.
fn light_page(app: &mut App) -> String {
    themed_page(app, "#fafafa", "#24292e")
}

fn themed_page(app: &mut App, background: &str, foreground: &str) -> String {
    let backend = ratatui::backend::TestBackend::new(WIDTH, HEIGHT);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut rows = String::new();
    for y in 0..buffer.area.height {
        let mut run: Option<(String, String)> = None;
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            let style = css(cell.fg, cell.bg, cell.modifier);
            let symbol = escape(cell.symbol());
            match &mut run {
                Some((current, text)) if *current == style => text.push_str(&symbol),
                _ => {
                    if let Some((style, text)) = run.take() {
                        rows.push_str(&format!("<span style=\"{style}\">{text}</span>"));
                    }
                    run = Some((style, symbol));
                }
            }
        }
        if let Some((style, text)) = run {
            rows.push_str(&format!("<span style=\"{style}\">{text}</span>"));
        }
        rows.push('\n');
    }
    format!(
        "<!doctype html><meta charset=\"utf-8\"><style>body{{margin:0;background:{background}}}\
         pre{{margin:0;padding:12px;font:15px/1.25 Menlo,monospace;color:{foreground};background:{background}}}</style>\
         <pre>{rows}</pre>"
    )
}

fn escape(symbol: &str) -> String {
    symbol.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn css(fg: Color, bg: Color, modifier: Modifier) -> String {
    let (mut fg, mut bg) = (hex(fg), hex(bg));
    if modifier.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
        fg = fg.or(Some("#16181d".into()));
        bg = bg.or(Some("#d7dae0".into()));
    }
    let mut style = String::new();
    if let Some(fg) = fg {
        style.push_str(&format!("color:{fg};"));
    }
    if let Some(bg) = bg {
        style.push_str(&format!("background:{bg};"));
    }
    if modifier.contains(Modifier::BOLD) {
        style.push_str("font-weight:bold;");
    }
    if modifier.contains(Modifier::DIM) {
        style.push_str("opacity:.6;");
    }
    if modifier.contains(Modifier::ITALIC) {
        style.push_str("font-style:italic;");
    }
    if modifier.contains(Modifier::UNDERLINED) {
        style.push_str("text-decoration:underline;");
    }
    style
}

fn hex(color: Color) -> Option<String> {
    let named = match color {
        Color::Reset => return None,
        Color::Black => "#1d1f21",
        Color::Red => "#e06c75",
        Color::Green => "#98c379",
        Color::Yellow => "#e5c07b",
        Color::Blue => "#61afef",
        Color::Magenta => "#c678dd",
        Color::Cyan => "#56b6c2",
        Color::Gray => "#abb2bf",
        Color::DarkGray => "#5c6370",
        Color::LightRed => "#f28b82",
        Color::LightGreen => "#b5e08f",
        Color::LightYellow => "#f2d58c",
        Color::LightBlue => "#8cc4f5",
        Color::LightMagenta => "#dba6eb",
        Color::LightCyan => "#8fd6df",
        Color::White => "#f0f0f0",
        Color::Rgb(r, g, b) => return Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Indexed(index) => return Some(indexed(index)),
    };
    Some(named.into())
}

fn indexed(index: u8) -> String {
    match index {
        16..=231 => {
            let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            let index = index - 16;
            format!("#{:02x}{:02x}{:02x}", level(index / 36), level(index / 6 % 6), level(index % 6))
        }
        232..=255 => {
            let gray = 8 + (index - 232) * 10;
            format!("#{gray:02x}{gray:02x}{gray:02x}")
        }
        _ => "#abb2bf".into(),
    }
}

