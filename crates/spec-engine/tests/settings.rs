use alondra_spec_engine::{Code, FolderSettings, ReportStatus, Settings, SpecsSettings, read_settings};

use crate::support::{billing, codes, line_of, padded, project};

fn fails_with(text: &str, code: Code) -> alondra_spec_engine::Problem {
    let problems = read_settings(Some(text.as_bytes())).expect_err("the settings read and they should not");
    assert_eq!(codes(&problems), [code], "{problems:#?}");
    assert_eq!(problems[0].path, "alondra.toml");
    problems[0].clone()
}

// spec: spec-engine.settings/absent-file-turns-everything-off
#[test]
fn a_project_without_settings_has_everything_disabled() {
    let settings = read_settings(None).unwrap();

    assert!(!settings.specs.enabled);
    assert!(!settings.decisions.enabled);
    assert!(!settings.reference.enabled);
}

// spec: spec-engine.settings/defaults-apply
#[test]
fn a_file_that_only_enables_specs_takes_every_default() {
    let settings = read_settings(Some(b"format = 1\n\n[specs]\nenabled = true\n")).unwrap();

    assert_eq!(
        settings,
        Settings {
            specs: SpecsSettings { enabled: true, committed: true, dir: "specs".to_string() },
            decisions: FolderSettings { enabled: false, dir: "docs/decisions".to_string() },
            reference: FolderSettings { enabled: false, dir: "docs/reference".to_string() },
        }
    );
}

// spec: spec-engine.settings/reads-every-setting
#[test]
fn every_value_written_in_the_file_is_read() {
    let text = "format = 1

[specs]
enabled = true
committed = false
dir = \"product/specs\"

[decisions]
enabled = true
dir = \"adr\"

[reference]
enabled = true
dir = \"handbook\"
";
    let settings = read_settings(Some(text.as_bytes())).unwrap();

    assert_eq!(
        settings,
        Settings {
            specs: SpecsSettings { enabled: true, committed: false, dir: "product/specs".to_string() },
            decisions: FolderSettings { enabled: true, dir: "adr".to_string() },
            reference: FolderSettings { enabled: true, dir: "handbook".to_string() },
        }
    );
}

// spec: spec-engine.settings/unknown-key-is-a-problem
#[test]
fn an_unknown_key_is_refused_at_its_line() {
    let text = "format = 1\n\n[specs]\nenable = true\n";
    let problem = fails_with(text, Code::InvalidSettings);

    assert_eq!(problem.line, line_of(text, "enable = true"));
    assert_eq!(problem.code.as_str(), "invalid_settings");
}

// spec: spec-engine.settings/broken-toml-is-a-problem
#[test]
fn a_file_that_is_not_toml_is_refused() {
    fails_with("format = 1\n\n[specs\nenabled = true\n", Code::InvalidSettings);
}

// spec: spec-engine.settings/folder-outside-repository-is-a-problem
#[test]
fn a_folder_outside_the_repository_is_refused_at_its_line() {
    for folder in ["../specs", "/etc/specs", "specs/../.."] {
        let text = format!("format = 1\n\n[specs]\nenabled = true\ndir = \"{folder}\"\n");
        let problems = read_settings(Some(text.as_bytes())).expect_err(folder);
        assert_eq!(codes(&problems), [Code::PathOutsideRepository], "{folder}: {problems:#?}");
        assert_eq!(problems[0].path, "alondra.toml", "{folder}");
        assert_eq!(problems[0].line, line_of(&text, "dir ="), "{folder}");
    }
    let text = "format = 1\n\n[specs]\nenabled = true\ndir = \"specs\"\n\n[decisions]\nenabled = true\ndir = \"../adr\"\n";
    let problems = read_settings(Some(text.as_bytes())).unwrap_err();
    assert_eq!(codes(&problems), [Code::PathOutsideRepository], "{problems:#?}");
    assert_eq!(problems[0].line, line_of(text, "dir = \"../adr\""));
}

// spec: spec-engine.settings/oversized-settings-are-a-problem
#[test]
fn settings_beyond_the_size_limit_are_refused() {
    let text = "format = 1\n\n[specs]\nenabled = true\n";
    assert!(read_settings(Some(padded(text, '#', 65_536).as_bytes())).unwrap().specs.enabled);

    let problem = fails_with(&padded(text, '#', 65_537), Code::TooLarge);

    assert_eq!(problem.code.as_str(), "too_large");
}

// spec: spec-engine.settings/unsupported-format-is-a-problem
#[test]
fn a_format_newer_than_the_engine_is_refused() {
    let problem = fails_with("format = 2\n\n[specs]\nenabled = true\n", Code::UnsupportedFormat);

    assert_eq!(problem.code.as_str(), "unsupported_format");
}

// spec: spec-engine.settings/disabled-project-loads-empty
#[test]
fn a_project_without_settings_reports_clean_and_empty() {
    let mut tree = billing();
    tree.remove("alondra.toml");

    let report = project(&tree).report(None);

    assert_eq!(report.status, ReportStatus::Clean);
    assert!(report.items.is_empty(), "{:#?}", report.items);
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
}

// spec: spec-engine.settings/markers-without-specs-when-not-committed
#[test]
fn a_marker_without_its_spec_is_no_problem_when_specs_are_not_committed() {
    let mut tree = billing();
    tree.put("alondra.toml", "format = 1\n\n[specs]\nenabled = true\ncommitted = false\n");
    tree.remove("specs/billing/refunds.feature");
    tree.apply(&project(&tree).suite_passed("billing", "unit").unwrap());

    let loaded = project(&tree);
    let report = loaded.report(None);

    let declared: Vec<(&str, &str)> = loaded.markers().iter().map(|marker| (marker.path.as_str(), marker.scenario.as_str())).collect();
    assert!(declared.contains(&("crates/billing/tests/full.rs", "billing.refunds/full")), "{declared:#?}");
    assert_eq!(report.status, ReportStatus::Clean, "{:#?}", report.items);
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
}
