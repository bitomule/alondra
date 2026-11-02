//! Pictures of the PR card flow and the usage indicator for review. Each state is drawn by the real terminal code into a
//! buffer and saved as an HTML page; `make tui-shots` turns the pages into PNGs.

use super::parity::view_fixture;
use super::*;
use alondra_core::domain::Refusal;
use alondra_core::views::Lang as ViewLang;
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
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
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

fn tasks(lang: crate::i18n::Lang) -> App {
    crate::i18n::set_for_this_thread(lang);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("tasks-tui.json")).unwrap())));
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
            alondra_core::views::send_anyway(
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
        let shots = shots.into_iter().chain([("5-tasks", tasks(lang)), ("6-usage", usage(lang, false)), ("7-usage-sheet", usage(lang, true))]);
        for (shot, mut app) in shots {
            let html = page(&mut app);
            std::fs::write(out.join(format!("{shot}-{name}.html")), html).unwrap();
        }
    }
}

fn page(app: &mut App) -> String {
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
        "<!doctype html><meta charset=\"utf-8\"><style>body{{margin:0;background:#16181d}}\
         pre{{margin:0;padding:12px;font:15px/1.25 Menlo,monospace;color:#d7dae0;background:#16181d}}</style>\
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

