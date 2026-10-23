//! How this Mac merges PRs: the one-press button on a PR card, the other ways under "More", and
//! the "PR merges" row in Settings.

use super::{
    Action, ActionOption, Icon, Input, Item, Lang, Presentation, RenderContext, Role, Style,
    action_id::Verb,
    common,
    text::{t, tf},
};
use crate::domain::{MergeMethod, PullRequest};

/// What the merge button says: GitHub's own words, so the card reads like the web.
fn button_label(method: MergeMethod, lang: Lang) -> &'static str {
    match method {
        MergeMethod::Squash => t!(lang, "Squash and merge", "Squash and merge"),
        MergeMethod::Merge => t!(lang, "Merge", "Hacer merge"),
        MergeMethod::Rebase => t!(lang, "Rebase and merge", "Rebase and merge"),
    }
}

fn noun(method: MergeMethod, lang: Lang) -> &'static str {
    match method {
        MergeMethod::Squash => t!(lang, "squash", "squash"),
        MergeMethod::Merge => t!(lang, "a merge commit", "merge commit"),
        MergeMethod::Rebase => t!(lang, "rebase", "rebase"),
    }
}

fn setting_label(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Squash => "Squash and merge",
        MergeMethod::Merge => "Merge",
        MergeMethod::Rebase => "Rebase and merge",
    }
}

fn setting_description(method: MergeMethod, lang: Lang) -> &'static str {
    match method {
        MergeMethod::Squash => t!(lang, "One commit on the base branch", "Un solo commit en la rama base"),
        MergeMethod::Merge => t!(lang, "Keeps every commit", "Conserva todos los commits"),
        MergeMethod::Rebase => t!(lang, "Commits replayed on the base branch", "Los commits se reaplican sobre la rama base"),
    }
}

/// The merge button for `pr` and, for "More", one button per other way its repository allows.
/// None while GitHub has not said how the repository merges.
pub(super) fn pr_actions(id: String, pr: &PullRequest, context: &RenderContext<'_>) -> Option<(Action, Vec<Action>)> {
    let lang = context.lang;
    let preferred = context.index.merge_method();
    let method = MergeMethod::for_pr(preferred, &pr.allowed_methods)?;
    let press = |method: MergeMethod, label: String| {
        let mut action = super::card::button(id.clone(), &label, Role::Advance);
        action.option_id = Some(method.id().into());
        action.stamp = Some(t!(lang, "Merging…", "Haciendo merge…").into());
        action
    };
    let mut merge = press(method, button_label(method, lang).into());
    if method != preferred {
        merge.description = Some(tf!(lang, "This repository does not allow {preferred}: it merges with {method}",
            "Este repositorio no permite {preferred}: se hará con {method}", preferred = noun(preferred, lang), method = noun(method, lang)));
    }
    let others = MergeMethod::ALL.into_iter()
        .filter(|other| *other != method && pr.allowed_methods.contains(other))
        .map(|other| {
            let mut action = press(other, tf!(lang, "Merge with {method}", "Hacer merge con {method}", method = noun(other, lang)));
            action.role = Some(Role::Neutral);
            action.style = Style::Secondary;
            action
        })
        .collect();
    Some((merge, others))
}

/// "PR merges": the way this Mac merges every PR, picked once in Settings.
pub(super) fn settings_row(context: &RenderContext<'_>) -> Option<Item> {
    let lang = context.lang;
    let current = context.index.merge_method();
    if !context.domain.set_merge_method_action(current).enabled {
        return None;
    }
    let mut action = common::action(Verb::SetMergeMethod, "merge_method", context.domain.revision,
        t!(lang, "PR merges", "Merge de las PR"), Style::Secondary)?;
    action.presentation = Presentation::Menu;
    action.stamp = Some(t!(lang, "Merge method changed", "Modo de merge cambiado").into());
    action.options = MergeMethod::ALL.into_iter().map(|method| ActionOption {
        id: method.id().into(),
        label: match method == current {
            true => tf!(lang, "{method} · now", "{method} · ahora", method = setting_label(method)),
            false => setting_label(method).into(),
        },
        description: Some(setting_description(method, lang).into()),
        style: Style::Secondary,
        input: Input::None,
        key: None,
    }).collect();
    let mut row = common::row("merge_method", "machine", Icon::Merge, action.label.clone(),
        Some(tf!(lang, "{method} · if the repository allows it", "{method} · si el repositorio lo permite", method = setting_label(current))));
    row.link = None;
    row.actions.push(action);
    Some(Item::Row(row))
}
