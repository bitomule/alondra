//! The Metrics screen drawn from the daemon's `metrics` view.

use super::*;

fn app() -> App {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    App::new(tx)
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn drawn(app: &mut App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    buffer.content().chunks(buffer.area.width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The fixture as the daemon sends it for `key`, with the row `shown` marked as the one shown.
fn view(key: &str, shown: &str) -> ViewDocument {
    let mut view = super::parity::metrics_fixture("metrics-ios.json");
    view["key"] = json!(key);
    let mark = json!([{"text": "Shown", "tone": "accent"}]);
    for row in view["sections"].as_array_mut().unwrap().iter_mut()
        .filter(|section| section["id"] == "period")
        .flat_map(|section| section["items"].as_array_mut().unwrap())
    {
        let here = row["id"] == shown;
        row["badges"] = if here { mark.clone() } else { json!([]) };
        if here {
            row.as_object_mut().unwrap().remove("link");
        } else {
            let period = row["id"].as_str().unwrap().trim_start_matches("period:").to_owned();
            row["link"] = json!({"kind": "view", "key": format!("metrics?period={period}&compare=previous")});
        }
    }
    serde_json::from_value(view).unwrap()
}

fn empty(key: &str) -> ViewDocument {
    let mut view = super::parity::metrics_fixture("metrics-ios.json");
    view["key"] = json!(key);
    view["sections"].as_array_mut().unwrap().retain(|section| section["id"] == "period" || section["id"] == "compare");
    view["notices"] = json!([{"text": "No task ended in this period", "tone": "neutral"}]);
    serde_json::from_value(view).unwrap()
}

fn opened() -> App {
    let mut app = app();
    app.screen = Screen::Tasks;
    press(&mut app, KeyCode::Char('M'));
    assert!(app.metrics.is_some(), "Shift+M opens Metrics");
    app.on_msg(Msg::ViewCard { key: "metrics".into(), result: Ok(view("metrics", "period:7d")) });
    app
}

fn line_of<'a>(screen: &'a str, text: &str) -> &'a str {
    screen.lines().find(|line| line.contains(text)).unwrap_or_else(|| panic!("no line with {text}:\n{screen}"))
}

/// The tabs drawn as the ones shown: one for the period and one for the comparison.
fn shown_tabs(app: &App) -> Vec<String> {
    let view = app.metrics.as_ref().unwrap().shown().unwrap().into_owned();
    super::metrics::lines(&view, usize::MAX, 120).lines.iter().flat_map(|line| &line.spans)
        .filter(|span| span.style.bg == Some(ratatui::style::Color::LightYellow))
        .map(|span| span.content.trim_matches(['[', ']']).to_owned()).collect()
}

fn texts(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
    lines.iter().map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect()).collect()
}

#[test]
fn metrics_draws_the_tabs_the_figures_by_section_the_groups_and_the_estimate_notice() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = opened();
    let screen = drawn(&mut app, 120, 80);
    for heading in ["Metrics", "Period", "Compare with", "Summary", "Cost", "Tokens and steps", "Time", "By model", "By project"] {
        assert!(screen.contains(heading), "{heading}:\n{screen}");
    }
    let period = line_of(&screen, "Last 7 days");
    assert!(period.contains("Period") && period.contains("Last 30 days") && period.contains("All time"), "the periods are tabs on one line:\n{screen}");
    assert_eq!(shown_tabs(&app), ["Last 7 days", "The period before"]);
    assert!(period.contains("[Last 7 days]  Last 30 days "), "the one shown reads so without colour:\n{screen}");
    assert!(line_of(&screen, "Summary").contains("Summary ────"), "a heading is ruled:\n{screen}");
    let notice = screen.lines().position(|line| line.contains("Provider's estimate")).unwrap();
    assert!(notice < screen.lines().position(|line| line.contains("Period")).unwrap(), "the notice leads:\n{screen}");
    assert!(screen.contains("1 task · 0% no rework · $6.00 per task"), "{screen}");
    assert!(screen.contains("Provider's estimate, not your bill · measured in 3 of 3 tasks"), "{screen}");
}

#[test]
fn every_figure_ends_in_one_column_with_what_it_is_set_beside_after_it() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = opened();
    let screen = drawn(&mut app, 120, 80);
    let end = |label: &str, value: &str| {
        let line = line_of(&screen, label);
        line.find(value).unwrap_or_else(|| panic!("no {value} beside {label}:\n{screen}")) + value.len()
    };
    let column = end("Tasks ended", "3");
    for (label, value) in [("Done with no rework", "67%"), ("Change rounds per task", "0.3"), ("Total ", "$8.30"),
        ("Per merged PR", "$2.50"), ("Input", "29k"), ("Cache read", "84.0 M"), ("Time per task", "1.5 h"), ("Queued, per task", "2 min")] {
        assert_eq!(end(label, value), column, "{label} ends its value in the column of the others:\n{screen}");
    }
    assert!(line_of(&screen, "Tasks ended").contains("3  before 2 (+50%)"), "{screen}");
}

#[test]
fn every_line_of_the_metrics_view_is_as_wide_as_the_panel() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    for (view, width) in [(view("metrics", "period:7d"), 117), (empty("metrics"), 97), (view("metrics", "period:7d"), 30)] {
        let rendered = super::metrics::lines(&view, 0, width);
        for (n, line) in texts(&rendered.lines).iter().enumerate() {
            assert_eq!(super::md::str_width(line), width, "line {n} of a {width}-wide panel: {line:?}");
        }
    }
    let mut app = opened();
    let screen = drawn(&mut app, 120, 80);
    let rows: Vec<&str> = screen.lines().skip_while(|row| !row.starts_with('╭')).skip(1).take_while(|row| !row.starts_with('╰')).collect();
    assert!(!rows.is_empty(), "{screen}");
    for row in rows {
        assert!(row.ends_with('│') && row.chars().count() == 120, "the panel's right border closes every row:\n{screen}");
    }
}

#[test]
fn a_better_value_is_green_a_worse_one_orange_and_the_rest_plain() {
    let shown: ViewDocument = view("metrics", "period:7d");
    let lines = super::metrics::lines(&shown, 0, 120).lines;
    let style = |text: &str| lines.iter().flat_map(|line| &line.spans)
        .find(|span| span.content == text).unwrap_or_else(|| panic!("no {text}")).style;
    assert_eq!(style("67%").fg, Some(ratatui::style::Color::LightGreen));
    assert_eq!(style(" (+33%)").fg, Some(ratatui::style::Color::LightGreen));
    assert_eq!(style("$3.75").fg, Some(ratatui::style::Color::Yellow));
    assert_eq!(style("$8.30").fg, None);
    assert!(style("$8.30").add_modifier.contains(ratatui::style::Modifier::BOLD));
    assert_eq!(style("before $5.00").fg, Some(ratatui::style::Color::DarkGray));
}

#[test]
fn a_narrow_screen_puts_each_value_under_its_label() {
    let shown: ViewDocument = view("metrics", "period:7d");
    let text = texts(&super::metrics::lines(&shown, 0, 30).lines);
    let label = text.iter().position(|line| line.trim() == "Done with no rework").unwrap_or_else(|| panic!("{text:#?}"));
    assert_eq!(text[label + 1].trim_end(), "    67%  before 50% (+33%)");
}

#[test]
fn picking_a_period_marks_it_at_once_waits_for_its_figures_and_esc_goes_back_then_out() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = opened();
    press(&mut app, KeyCode::Char('j'));
    let asked = app.spawned.borrow().len();
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.metrics.as_ref().unwrap().key, "metrics?period=30d&compare=previous");
    assert_eq!(app.spawned.borrow().len(), asked + 1, "the new key is asked for");

    let screen = drawn(&mut app, 120, 60);
    assert_eq!(shown_tabs(&app), ["Last 30 days", "The period before"], "the pick shows before the Mac answers");
    assert!(screen.contains("Loading…"), "{screen}");
    assert!(!screen.contains("Tasks ended"), "the old period's figures are not shown as the new one's:\n{screen}");
    assert_eq!(app.metrics.as_ref().unwrap().selected().map(|row| row.id), Some("period:30d".into()));

    app.on_msg(Msg::ViewCard { key: "metrics?period=30d&compare=previous".into(), result: Ok(view("metrics?period=30d&compare=previous", "period:30d")) });
    let screen = drawn(&mut app, 120, 60);
    assert!(screen.contains("Tasks ended"), "{screen}");
    assert!(!screen.contains("Loading…"), "{screen}");
    assert_eq!(app.metrics.as_ref().unwrap().selected().map(|row| row.id), Some("period:30d".into()));

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.metrics.as_ref().unwrap().key, "metrics");
    let screen = drawn(&mut app, 120, 60);
    assert_eq!(shown_tabs(&app), ["Last 7 days", "The period before"], "the key before shows at once, from what it had");
    assert!(screen.contains("Tasks ended"), "{screen}");
    press(&mut app, KeyCode::Esc);
    assert!(app.metrics.is_none());
}

#[test]
fn a_comparison_the_daemon_cannot_answer_says_why_in_place_of_its_figures() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = opened();
    let compare = app.metrics.as_ref().unwrap().rows().iter().position(|row| row.id == "compare:none").unwrap();
    for _ in 0..compare {
        press(&mut app, KeyCode::Char('j'));
    }
    press(&mut app, KeyCode::Enter);
    let key = app.metrics.as_ref().unwrap().key.clone();
    assert_eq!(key, "metrics?period=7d&compare=none");
    app.on_msg(Msg::ViewCard { key, result: Err("The Mac did not answer".into()) });
    let screen = drawn(&mut app, 120, 60);
    assert!(screen.contains("The Mac did not answer"), "{screen}");
    assert_eq!(shown_tabs(&app), ["Last 7 days", "Nothing"]);
}

#[test]
fn a_period_where_nothing_ended_keeps_its_selectors_and_says_so() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    press(&mut app, KeyCode::Char('M'));
    assert!(drawn(&mut app, 100, 30).contains("Loading…"));
    app.on_msg(Msg::ViewCard { key: "metrics".into(), result: Ok(empty("metrics")) });
    let screen = drawn(&mut app, 100, 30);
    assert!(screen.contains("Last 90 days"), "{screen}");
    assert!(screen.contains("No task ended in this period"), "{screen}");
    assert!(!screen.contains("Summary"), "{screen}");
}

#[test]
fn settings_lists_metrics_and_its_row_opens_the_screen_over_settings() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    press(&mut app, KeyCode::Char(','));
    let screen = drawn(&mut app, 100, 30);
    assert!(screen.contains("Cost, tokens and how tasks went"), "{screen}");
    let row = app.mcp_servers.as_ref().unwrap().rows().iter().position(|row| row.id == "metrics").unwrap();
    for _ in 0..row {
        press(&mut app, KeyCode::Char('j'));
    }
    press(&mut app, KeyCode::Enter);
    assert!(app.metrics.is_some(), "Enter on the row opens Metrics");
    press(&mut app, KeyCode::Esc);
    assert!(app.metrics.is_none());
    assert!(app.mcp_servers.is_some(), "Esc goes back to Settings");
}

#[test]
fn the_metrics_command_opens_the_screen() {
    let mut app = app();
    app.cmdline = Some(TextInput::with("metrics"));
    press(&mut app, KeyCode::Enter);
    assert!(app.metrics.is_some());
}

#[test]
fn the_shown_view_is_followed_for_changes() {
    let app = opened();
    let open = app.open_views.lock().unwrap().clone();
    assert!(open.contains_key("metrics"), "{open:?}");
}
