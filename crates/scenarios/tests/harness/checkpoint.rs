use alondra_scenarios::checkpoint::{Files, compare, normalize};
use serde_json::json;

#[test]
fn what_changes_from_run_to_run_is_masked_and_the_instants_are_kept() {
    let mut view = json!({"data_epoch": "abc", "sections": [{"version": "v1", "title": "Inbox", "at": "2026-11-15T09:00:25.000+00:00"}]});
    normalize(&mut view);
    assert_eq!(view, json!({"data_epoch": "<data_epoch>", "sections": [{"version": "<version>", "title": "Inbox", "at": "2026-11-15T09:00:25.000+00:00"}]}));
}

#[test]
fn the_ids_the_daemon_draws_and_the_revision_of_a_row_are_masked() {
    let mut view = json!({"id": "pause:sdk-acee27f00e56b821@19~t1", "next": "clear_completed:view@25", "mail": "ana@example.com"});
    normalize(&mut view);
    assert_eq!(view, json!({"id": "pause:<node>@<revision>~t1", "next": "clear_completed:view@<revision>", "mail": "ana@example.com"}));
}

#[test]
fn a_difference_names_the_file_and_the_lines() {
    let accepted = Files::from([("home.ios.es.json".to_owned(), "a\nb\n".to_owned())]);
    let found = Files::from([("home.ios.es.json".to_owned(), "a\nc\n".to_owned())]);
    let differences = compare(&accepted, &found);
    assert_eq!(differences["home.ios.es.json"], "line 2:\n- b\n+ c\n");
    assert!(compare(&accepted, &accepted).is_empty());
}
