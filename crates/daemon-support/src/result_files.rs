//! A report or plan a node delivers outlives its session: the session's folders, its temporary
//! one included, go when the task closes, so the whole final result is kept in Alondra's files
//! folder and its card says where.
use alondra_core::domain::{Command, DeliveryTarget, DomainError};
use std::path::{Path, PathBuf};

const RESULTS: &str = "files/results";

/// Keeps the whole final result of a delivery without a pull request (its report, the files it
/// cites and its plan) and names the copy on its card. Returns the file it wrote, to remove if
/// the delivery is refused.
pub fn keep(home: &Path, command: &mut Command) -> Result<Option<PathBuf>, DomainError> {
    let Command::PresentResult { task_id, interaction_id, body, targets, is_final: true, nothing_to_review: false, card, .. } = command else {
        return Ok(None);
    };
    card.kept_at = None;
    if targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { .. })) {
        return Ok(None);
    }
    let file = home.join(RESULTS).join(segment(&task_id.0)).join(format!("{}.md", segment(&interaction_id.0)));
    if file.exists() {
        card.kept_at = Some(file.display().to_string());
        return Ok(None);
    }
    let report = card.content.as_ref().map(|content| content.body.trim()).filter(|text| !text.is_empty()).unwrap_or(body.trim());
    let mut text = format!("# {}\n\n{report}\n", card.need.trim());
    if let Some(plan) = card.plan.as_deref().map(str::trim).filter(|plan| !plan.is_empty()) {
        text.push_str(&format!("\n---\n\n{plan}\n"));
    }
    let written = file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&file, text));
    if let Err(error) = written {
        return Err(DomainError::new("storage_error", format!("could not keep the result in {}: {error}", file.display())));
    }
    card.kept_at = Some(file.display().to_string());
    Ok(Some(file))
}

/// An id as one safe file name: never a path that leaves its folder.
fn segment(id: &str) -> String {
    let name: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    if name.trim_matches('.').is_empty() { format!("_{name}") } else { name }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{CardKind, CardSpec, ContentKind};

    fn delivery(targets: Vec<DeliveryTarget>, card: CardSpec) -> Command {
        Command::PresentResult {
            attachments: vec![], task_id: "t-research".into(), interaction_id: "result-1".into(), body: "Report".into(),
            targets, is_final: true, nothing_to_review: false, checklist: vec![], card,
        }
    }

    fn kept_at(command: &Command) -> Option<String> {
        let Command::PresentResult { card, .. } = command else { unreachable!() };
        card.kept_at.clone()
    }

    #[test]
    fn a_final_report_whose_file_lives_in_a_temporary_folder_is_kept_whole_after_that_folder_goes() {
        let home = crate::test_support::Directory::new();
        let scratch = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*scratch).unwrap();
        let cited = scratch.join("redraft-index.md");
        std::fs::write(&cited, "## Phases\n\n1. Index the library").unwrap();
        let body = format!("Full report below.\n\n## {}\n\n{}", cited.display(), std::fs::read_to_string(&cited).unwrap());
        let card = CardSpec::new(CardKind::Review, "Read the report on indexing the whole library")
            .content(ContentKind::Report, body)
            .plan("- [ ] Index the library in the background");
        let mut command = delivery(vec![DeliveryTarget::Text { text: "report".into() }], card);
        let written = keep(home.as_ref(), &mut command).unwrap().expect("a final report is kept");
        drop(scratch);
        assert!(!cited.exists(), "the temporary folder is gone");
        let kept = std::fs::read_to_string(&written).unwrap();
        assert!(written.starts_with(home.join("files/results/t-research")), "{}", written.display());
        assert!(kept.contains("1. Index the library"), "the cited file's content survives: {kept}");
        assert!(kept.contains("- [ ] Index the library in the background"), "the plan survives: {kept}");
        assert_eq!(kept_at(&command), Some(written.display().to_string()), "the card says where it is kept");
    }

    #[test]
    fn a_pull_request_progress_or_silent_result_is_not_copied() {
        let home = crate::test_support::Directory::new();
        let card = || CardSpec::new(CardKind::Review, "Review the fix").content(ContentKind::Report, "Done");
        let mut pr = delivery(vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "a".repeat(40) }], card());
        assert_eq!(keep(home.as_ref(), &mut pr).unwrap(), None);
        let mut progress = delivery(vec![], card());
        if let Command::PresentResult { is_final, .. } = &mut progress { *is_final = false; }
        assert_eq!(keep(home.as_ref(), &mut progress).unwrap(), None);
        assert!(!home.join("files").exists(), "nothing written");
    }

    #[test]
    fn a_node_cannot_name_where_its_result_is_kept_or_write_outside_the_results_folder() {
        let home = crate::test_support::Directory::new();
        let mut card = CardSpec::new(CardKind::Review, "Read it").content(ContentKind::Report, "Report");
        card.kept_at = Some("/etc/passwd".into());
        let mut command = delivery(vec![], card);
        if let Command::PresentResult { task_id, interaction_id, .. } = &mut command {
            *task_id = "../../escape".into();
            *interaction_id = "..".into();
        }
        let written = keep(home.as_ref(), &mut command).unwrap().unwrap();
        assert!(written.starts_with(home.join("files/results")), "{}", written.display());
        assert_eq!(kept_at(&command), Some(written.display().to_string()));
    }
}
