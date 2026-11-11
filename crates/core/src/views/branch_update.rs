//! How nodes bring main into their PR's branch: the "Update a PR's branch with main" row in Settings.

use super::{
    ActionOption, Icon, Input, Item, Presentation, RenderContext, Style, Lang,
    action_id::Verb,
    common,
    text::{t, tf},
};
use crate::domain::BranchUpdate;

fn setting_label(update: BranchUpdate) -> &'static str {
    match update {
        BranchUpdate::Rebase => "Rebase",
        BranchUpdate::Merge => "Merge",
    }
}

fn setting_description(update: BranchUpdate, lang: Lang) -> &'static str {
    match update {
        BranchUpdate::Rebase => t!(lang, "The PR's commits replayed on top of main, pushed with --force-with-lease",
            "Los commits de la PR se reaplican sobre main y se suben con --force-with-lease"),
        BranchUpdate::Merge => t!(lang, "A merge commit brings main into the branch", "Un commit de merge trae main a la rama"),
    }
}

/// "Update a PR's branch with main": how every node of this Mac brings main into its branch, picked once in Settings.
pub(super) fn settings_row(context: &RenderContext<'_>) -> Option<Item> {
    let lang = context.lang;
    let current = context.index.branch_update();
    let other = BranchUpdate::ALL.into_iter().find(|choice| *choice != current).unwrap_or(current);
    if !context.domain.set_branch_update_action(other).enabled {
        return None;
    }
    let mut action = common::action(Verb::SetBranchUpdate, "branch_update", context.domain.revision,
        t!(lang, "Update a PR's branch with main", "Actualizar la rama de un PR con main"), Style::Secondary)?;
    action.presentation = Presentation::Menu;
    action.stamp = Some(t!(lang, "Branch update changed", "Modo de actualizar la rama cambiado").into());
    action.options = BranchUpdate::ALL.into_iter().map(|update| ActionOption {
        id: update.id().into(),
        label: match update == current {
            true => tf!(lang, "{update} · now", "{update} · ahora", update = setting_label(update)),
            false => setting_label(update).into(),
        },
        description: Some(setting_description(update, lang).into()),
        style: Style::Secondary,
        input: Input::None,
        key: None,
    }).collect();
    let mut row = common::row("branch_update", "machine", Icon::Branch, action.label.clone(),
        Some(tf!(lang, "{update} · when the PR has conflicts", "{update} · cuando la PR tiene conflictos", update = setting_label(current))));
    row.link = None;
    row.actions.push(action);
    Some(Item::Row(row))
}
