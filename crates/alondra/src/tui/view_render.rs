//! Text rendering for daemon-built views. The component switch lives here so new view
//! components have one deliberate TUI representation.

use alondra_core::views::{Action, Card, Icon, Item, Label, Row, Section, View};
use ratatui::{
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
};

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::LightYellow;

pub fn sections(view: &View) -> impl Iterator<Item = &Section> {
    view.sections
        .iter()
        .filter(|section| !section.id.eq("detail"))
}

pub fn rows(view: &View) -> Vec<(&Row, bool)> {
    let mut result = Vec::new();
    for section in &view.sections {
        for item in &section.items {
            match item {
                Item::Row(row) => {
                    result.push((row, false));
                    result.extend(row.children.iter().map(|child| (child, true)));
                }
                Item::Card(_) => {}
                Item::Markdown(_)
                | Item::ContentRef(_)
                | Item::Facts(_)
                | Item::Checks(_)
                | Item::Timeline(_)
                | Item::Form(_)
                | Item::Notice(_)
                | Item::Actions(_) => {}
            }
        }
    }
    result
}

pub fn row_lines(row: &Row, width: usize, selected: bool, nested: bool) -> Vec<Line<'static>> {
    let glyph = icon(row.icon);
    let prefix = if selected { "▌" } else { " " };
    let indent = if nested { "  ↳ " } else { "" };
    let mut first = vec![Span::styled(prefix, Style::new().fg(ACCENT))];
    first.push(Span::styled(
        indent.to_owned(),
        Style::new().fg(Color::Magenta),
    ));
    first.push(Span::styled(
        format!("{glyph} "),
        Style::new()
            .fg(icon_color(row.icon))
            .add_modifier(Modifier::BOLD),
    ));
    first.push(Span::styled(
        row.title.clone(),
        if selected {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    ));
    let first = fit(first, width);
    let mut lines = vec![first];
    let mut meta = Vec::new();
    if let Some(subtitle) = &row.subtitle {
        meta.push(subtitle.clone());
    }
    meta.extend(row.badges.iter().map(|badge| badge.text.clone()));
    if let Some(trailing) = &row.trailing {
        match trailing {
            alondra_core::views::Trailing::Count { value } => meta.push(value.to_string()),
            alondra_core::views::Trailing::Time { at, format } => meta.push(format_time(at, *format)),
            alondra_core::views::Trailing::Text { text } => meta.push(text.clone()),
        }
    }
    if !meta.is_empty() {
        let mut seen = std::collections::HashSet::new();
        meta.retain(|value| seen.insert(value.clone()));
        lines.push(fit(
            vec![
                Span::raw(if nested { "      " } else { "  " }),
                Span::styled(meta.join(" · "), Style::new().fg(DIM)),
            ],
            width,
        ));
    }
    lines
}

pub fn card_lines(card: &Card, width: usize, depth: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let indent = "  ".repeat(depth);
    if let Some(eyebrow) = &card.eyebrow {
        out.push(Line::from(Span::styled(
            format!("{indent}{}", eyebrow.text),
            label_style(eyebrow),
        )));
    }
    out.push(Line::from(Span::styled(
        format!("{indent}{} {}", icon(card.icon), card.title),
        Style::new().add_modifier(Modifier::BOLD),
    )));
    if let Some(subtitle) = &card.subtitle {
        out.push(Line::from(format!("{indent}{subtitle}").fg(DIM)));
    }
    for context in &card.context {
        out.push(Line::from(format!("{indent}{}", context.text).fg(DIM)));
    }
    for badge in &card.badges {
        out.push(Line::from(Span::styled(
            format!("{indent}{}", badge.text),
            label_style(badge),
        )));
    }
    if let Some(body) = &card.body {
        let rendered = super::md::render(&super::md::parse(&body.text), width);
        out.extend(rendered.lines);
    }
    for quote in &card.quotes {
        out.push(Line::from(Span::styled(
            format!("{indent}{}:", quote.label),
            Style::new().fg(DIM).add_modifier(Modifier::ITALIC),
        )));
        out.extend(
            quote
                .text
                .lines()
                .map(|line| Line::from(format!("{indent}  {line}"))),
        );
    }
    if let Some(facts) = &card.facts {
        for fact in &facts.items {
            out.push(Line::from(format!(
                "{indent}{}: {}",
                fact.label, fact.value
            )));
        }
    }
    if let Some(checks) = &card.checks {
        for check in &checks.items {
            out.push(Line::from(format!(
                "{indent}{} {}",
                check_state(check.state),
                check.name
            )));
        }
    }
    for child in &card.children {
        out.push(Line::default());
        out.extend(card_lines(child, width, depth + 1));
    }
    let _ = width;
    out
}

pub fn document_lines(view: &View, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(title) = &view.title {
        lines.push(Line::from(Span::styled(
            title.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        )));
    }
    for notice in &view.notices {
        lines.push(Line::from(Span::styled(
            notice.text.clone(),
            label_style(&Label {
                text: String::new(),
                tone: notice.tone,
            }),
        )));
    }
    for section in &view.sections {
        if let Some(title) = &section.title {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(
                title.clone(),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
        }
        for item in &section.items {
            match item {
                Item::Card(card) => lines.extend(card_lines(card, width, 0)),
                Item::Row(row) => {
                    lines.extend(row_lines(row, width, false, row.depth > 0));
                    for child in &row.children {
                        lines.extend(row_lines(child, width, false, true));
                    }
                }
                Item::Markdown(markdown) => {
                    lines.extend(super::md::render(&super::md::parse(&markdown.text), width).lines)
                }
                Item::ContentRef(content) => lines.push(Line::from(vec![
                    Span::styled("▤ ", Style::new().fg(Color::Cyan)),
                    Span::raw(format!("{} · {} B", content.title, content.size)),
                ])),
                Item::Facts(facts) => {
                    for fact in &facts.items {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{}: ", fact.label), Style::new().fg(DIM)),
                            Span::raw(fact.value.clone()),
                        ]));
                    }
                }
                Item::Checks(checks) => {
                    for check in &checks.items {
                        lines.push(Line::from(format!(
                            "{} {}",
                            check_state(check.state),
                            check.name
                        )));
                    }
                }
                Item::Timeline(timeline) => {
                    for entry in &timeline.entries {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{}  ", format_time(&entry.at, alondra_core::views::TimeFormat::Absolute)), Style::new().fg(DIM)),
                            Span::raw(entry.title.clone()),
                        ]));
                        if let Some(text) = &entry.text {
                            lines.push(Line::from(format!("  {text}")));
                        }
                    }
                }
                Item::Form(form) => {
                    lines.push(Line::from(Span::styled(
                        form.id.clone(),
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                    )));
                    for field in &form.fields {
                        let value = match &field.value {
                            serde_json::Value::String(s) => s.clone(),
                            value => value.to_string(),
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!("{}: ", field.label), Style::new().fg(DIM)),
                            Span::raw(value),
                        ]));
                        if let Some(help) = &field.help {
                            lines.push(Line::from(format!("  {help}").fg(DIM)));
                        }
                    }
                    lines.push(Line::from(vec![
                        Span::styled("[e] ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                        Span::raw(format!("Edit fields and {}", form.submit.label)),
                    ]));
                }
                Item::Notice(notice) => lines.push(Line::from(Span::styled(
                    notice.text.clone(),
                    label_style(&Label {
                        text: String::new(),
                        tone: notice.tone,
                    }),
                ))),
                Item::Actions(_) => {}
            }
        }
    }
    lines
}

pub(super) fn action_line(action: &Action) -> String {
    let hint = action_hint(action);
    if hint.is_empty() {
        action.label.clone()
    } else {
        format!("[{hint}] {}", action.label)
    }
}

pub fn format_time(at: &str, format: alondra_core::views::TimeFormat) -> String {
    let Some(millis) = parse_utc(at) else {
        return at.to_owned();
    };
    let Some(offset) = local_utc_offset(millis) else {
        return at.to_owned();
    };
    format_time_at(at, millis, offset, format)
}

fn format_time_at(
    at: &str,
    millis: i64,
    utc_offset_seconds: i64,
    format: alondra_core::views::TimeFormat,
) -> String {
    let Some((date, time)) = date_time_at_offset(millis, utc_offset_seconds) else {
        return at.to_owned();
    };
    match format {
        alondra_core::views::TimeFormat::Relative => super::draw::ago(millis),
        alondra_core::views::TimeFormat::Absolute => format!("{date} {time}"),
        alondra_core::views::TimeFormat::Time => time,
    }
}

fn parse_utc(value: &str) -> Option<i64> {
    let value = value.strip_suffix('Z')?;
    let (date, clock) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let (year, month, day) = (
        date_parts.next()?.parse::<i64>().ok()?,
        date_parts.next()?.parse::<i64>().ok()?,
        date_parts.next()?.parse::<i64>().ok()?,
    );
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let clock = clock.split('.').next()?;
    let mut time_parts = clock.split(':');
    let (hour, minute, second) = (
        time_parts.next()?.parse::<i64>().ok()?,
        time_parts.next()?.parse::<i64>().ok()?,
        time_parts.next()?.parse::<i64>().ok()?,
    );
    if time_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let year_adj = year - i64::from(month <= 2);
    let era = year_adj.div_euclid(400);
    let year_of_era = year_adj - era * 400;
    let month_adj = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_adj + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let fraction = value
        .split_once('.')
        .and_then(|(_, fraction)| fraction.parse::<u16>().ok())
        .unwrap_or(0);
    let millis = ((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000)
        + i64::from(fraction);
    Some(millis)
}

fn local_utc_offset(millis: i64) -> Option<i64> {
    let seconds = millis.div_euclid(1_000);
    let timestamp = libc::time_t::try_from(seconds).ok()?;
    // SAFETY: localtime_r writes to the supplied `tm` and does not retain either pointer.
    let tm = unsafe {
        let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
        libc::localtime_r(&timestamp, tm.as_mut_ptr()).as_ref()?;
        tm.assume_init()
    };
    Some(i64::from(tm.tm_gmtoff))
}

fn date_time_at_offset(millis: i64, utc_offset_seconds: i64) -> Option<(String, String)> {
    let seconds = millis.div_euclid(1_000) + utc_offset_seconds;
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_adj = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_adj + 2) / 5 + 1;
    let month = if month_adj < 10 { month_adj + 3 } else { month_adj - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Some((
        format!("{year:04}-{month:02}-{day:02}"),
        format!("{:02}:{:02}", second_of_day / 3_600, second_of_day % 3_600 / 60),
    ))
}

pub fn actions(view: &View) -> Vec<Action> {
    fn row_actions(row: &Row, out: &mut Vec<Action>) {
        out.extend(row.actions.iter().cloned());
        for child in &row.children {
            row_actions(child, out);
        }
    }
    let mut out = Vec::new();
    for section in sections(view) {
        for item in &section.items {
            match item {
                Item::Card(card) => out.extend(card.actions.iter().cloned()),
                Item::Row(row) => row_actions(row, &mut out),
                Item::Actions(bar) => out.extend(bar.actions.iter().cloned()),
                Item::Form(_) => {}
                Item::Notice(notice) => out.extend(notice.action.iter().cloned()),
                _ => {}
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|action| seen.insert(action.id.clone()));
    out
}

pub fn detail_card(view: &View) -> Option<&Card> {
    view.sections
        .iter()
        .find(|section| section.id == "detail" || section.id == "card")?
        .items
        .iter()
        .find_map(|item| match item {
            Item::Card(card) => Some(card),
            _ => None,
        })
}

pub fn action_hint(action: &Action) -> &str {
    action.key.as_deref().unwrap_or_else(|| {
        use alondra_core::views::action_id::{ActionId, Verb};
        match ActionId::parse(&action.id).map(|id| id.verb) {
            Ok(Verb::Merge) => "m",
            Ok(Verb::Conflicts) => "1",
            Ok(Verb::Withdraw) => "x",
            _ => "",
        }
    })
}

pub fn icon(icon: Icon) -> &'static str {
    match icon {
        Icon::Question => "?",
        Icon::Action => "⚡",
        Icon::Document => "▤",
        Icon::PullRequest => "⑂",
        Icon::Notice => "·",
        Icon::Delivery => "⇢",
        Icon::Task => "□",
        Icon::Request => "＋",
        Icon::Node => "●",
        Icon::Orchestrator => "◆",
        Icon::Mission => "◇",
        Icon::Project => "▰",
        Icon::Schedule => "◷",
        Icon::Attachment => "📎",
        Icon::Check => "✓",
        Icon::Warning => "!",
        Icon::Merge => "⑂",
        Icon::Branch => "⑂",
        Icon::History => "◷",
        Icon::Machine => "⌘",
    }
}

pub fn icon_color(icon: Icon) -> Color {
    match icon {
        Icon::Question => ACCENT,
        Icon::PullRequest | Icon::Merge => Color::Magenta,
        Icon::Warning => Color::LightRed,
        _ => Color::Cyan,
    }
}

fn label_style(label: &Label) -> Style {
    Style::new().fg(match label.tone {
        alondra_core::views::Tone::Neutral => DIM,
        alondra_core::views::Tone::Info => Color::Cyan,
        alondra_core::views::Tone::Accent => ACCENT,
        alondra_core::views::Tone::Success => Color::LightGreen,
        alondra_core::views::Tone::Warning | alondra_core::views::Tone::YourTurn | alondra_core::views::Tone::Unclear => Color::Yellow,
        alondra_core::views::Tone::Critical => Color::LightRed,
    })
}

fn check_state(state: alondra_core::views::CheckState) -> &'static str {
    match state {
        alondra_core::views::CheckState::Pass => "✓",
        alondra_core::views::CheckState::Fail => "✗",
        alondra_core::views::CheckState::Pending => "…",
    }
}

fn fit(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let line = Line::from(spans);
    let mut result = Vec::new();
    let mut remaining = width;
    for span in line.spans {
        if remaining == 0 {
            break;
        }
        let text = span.content.to_string();
        let take = super::md::truncate(&text, remaining);
        remaining = remaining.saturating_sub(super::md::str_width(&take));
        result.push(Span::styled(take, span.style));
    }
    Line::from(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::views::{Input, Presentation, Style};

    #[test]
    fn view_times_are_formatted_for_people_instead_of_showing_wire_timestamps() {
        use alondra_core::views::TimeFormat;
        let at = "2026-10-01T23:55:00.000Z";
        let instant = parse_utc(at).unwrap();
        assert_eq!(format_time_at(at, instant, 2 * 3_600, TimeFormat::Absolute), "2026-10-02 01:55");
        assert_eq!(format_time_at(at, instant, 2 * 3_600, TimeFormat::Time), "01:55");
        assert_eq!(format_time_at(at, instant, -(5 * 3_600 + 30 * 60), TimeFormat::Absolute), "2026-10-01 18:25");
        assert_eq!(format_time_at(at, instant, 0, TimeFormat::Time), "23:55");
        assert_eq!(format_time("not-a-date", TimeFormat::Absolute), "not-a-date");
    }

    #[test]
    fn duplicate_row_state_metadata_is_shown_once() {
        let row: Row = serde_json::from_value(serde_json::json!({
            "id": "task", "icon": "task", "title": "Ship", "depth": 0,
            "subtitle": "working", "badges": [{"text": "working", "tone": "info"}],
            "struck": false
        })).unwrap();
        let rendered = row_lines(&row, 80, false, false);
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[1].to_string().matches("working").count(), 1);
    }

    #[test]
    fn actions_without_keyboard_bindings_do_not_render_empty_brackets() {
        let action = Action {
            id: "message".into(), label: "Message".into(), style: Style::Primary,
            presentation: Presentation::Button, options: vec![], input: Input::None,
            attachments: false, comments: None, confirm: None, enabled: true,
            disabled_reason: None, stamp: None, key: None, link: None,
        };
        assert_eq!(action_line(&action), "Message");
        let withdraw = Action {
            id: "withdraw:request-1@1".into(),
            label: "Cancel this request".into(),
            ..action
        };
        assert_eq!(action_hint(&withdraw), "x");
        assert_eq!(action_line(&withdraw), "[x] Cancel this request");
    }

    #[test]
    fn duplicate_attach_actions_with_legacy_empty_ids_are_shown_once() {
        let attach = serde_json::json!({
            "id":"", "label":"Attach to node", "style":"plain",
            "presentation":"button", "input":{"kind":"none"},
            "attachments":false, "enabled":true, "key":"a"
        });
        let view: View = serde_json::from_value(serde_json::json!({
            "protocol":1, "data_epoch":"test", "view":"card", "key":"card:pr",
            "client":"tui", "version":"v1", "cursor":"c",
            "status":{"version":"v1", "summary":"", "badge":0, "counts":[],
                "gauges":[], "orchestrator":{"text":"idle", "tone":"neutral"}},
            "sections":[{"id":"card", "collapsed":false, "items":[{
                "type":"card", "id":"pr", "icon":"pull_request", "title":"PR",
                "detail":"included", "actions":[attach.clone(), attach]
            }]}]
        })).unwrap();
        let actions = actions(&view);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].label, "Attach to node");
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;
    use alondra_core::views::{Input, Presentation, Style};

    fn action(id: &str, label: &str) -> Action {
        Action {
            id: id.into(), label: label.into(), style: Style::Plain,
            presentation: Presentation::Button, options: vec![], input: Input::None,
            attachments: false, comments: None, confirm: None, enabled: true,
            disabled_reason: None, stamp: None, key: None, link: None,
        }
    }

    fn bar(actions: Vec<Action>) -> View {
        serde_json::from_value(serde_json::json!({
            "protocol":1, "data_epoch":"test", "view":"card", "key":"card:pr",
            "client":"tui", "version":"v1", "cursor":"c",
            "status":{"version":"v1", "summary":"", "badge":0, "counts":[],
                "gauges":[], "orchestrator":{"text":"idle", "tone":"neutral"}},
            "sections":[{"id":"card", "collapsed":false, "items":[{
                "type":"actions", "id":"bar", "actions": actions
            }]}]
        })).unwrap()
    }

    #[test]
    fn hints_follow_the_action_verb_whatever_the_label_says() {
        assert_eq!(action_hint(&action("merge:pr-1@1", "Fusionar")), "m");
        assert_eq!(action_hint(&action("conflicts:pr-1@1", "Resolver")), "1");
        assert_eq!(action_hint(&action("withdraw:request-1@1", "Retirar")), "x");
        assert_eq!(action_hint(&action("reply:i-1@1", "Withdraw the merge button")), "");
    }

    #[test]
    fn distinct_actions_sharing_a_label_are_all_shown() {
        let view = bar(vec![
            action("cancel:task-a@1", "Cancel"),
            action("cancel:task-b@1", "Cancel"),
            action("cancel:task-a@1", "Cancel"),
        ]);
        let ids: Vec<String> = actions(&view).into_iter().map(|action| action.id).collect();
        assert_eq!(ids, ["cancel:task-a@1", "cancel:task-b@1"]);
    }
}
