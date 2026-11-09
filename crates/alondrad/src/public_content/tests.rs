use super::*;
use alondra_core::domain::*;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
const NAME: &str = "example-private-project";
const CLEAN_DIFF: &str = "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+New documentation\n";
const PRIVATE_DIFF: &str = "diff --git a/docs/usage.md b/docs/usage.md\n--- a/docs/usage.md\n+++ b/docs/usage.md\n@@ -1 +1 @@\n-old\n+Uses EXAMPLE-PRIVATE-PROJECT for examples\n";

fn execute(state: &mut DomainState, actor: Actor, command: Command) {
    state
        .execute(
            &actor,
            &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
                command_id: format!("command-{}", state.revision).into(),
                expected_revision: state.revision,
                command,
            },
        )
        .unwrap();
}

fn state() -> DomainState {
    let mut state = DomainState::new("fixture");
    execute(
        &mut state,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "node".into(),
            name: "Node".into(),
            at: 0,
        },
    );
    execute(
        &mut state,
        Actor::User {
            user_id: "user".into(),
        },
        Command::CreateRequest {
            request_id: "request".into(),
            body: "Work".into(),
            auto_merge: false,
            maintenance: false,
            message: false,
        },
    );
    execute(
        &mut state,
        Actor::Orchestrator { generation: 0 },
        Command::CreateTask {
            plan_items: None,
            continues: None,
            task_id: "task".into(),
            request_id: "request".into(),
            title: "Work".into(),
            agent_id: Some("node".into()),
            review_prs: false,
            dependencies: Default::default(),
            stack: None,
            launch: Some(LaunchPlan::new("node", "/fixture")),
        },
    );
    execute(
        &mut state,
        Actor::Agent {
            agent_id: "node".into(),
        },
        Command::StartTask {
            task_id: "task".into(),
        },
    );
    register(&mut state, 7);
    state
}

fn register(state: &mut DomainState, number: u64) {
    execute(
        state,
        Actor::Agent {
            agent_id: "node".into(),
        },
        Command::RegisterPr {
            task_id: "task".into(),
            pr_id: format!("pr-{number}").into(),
            host: "github.com".into(),
            repository: "owner/public-project".into(),
            number,
            head: HEAD.into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
    );
}

fn delivery() -> Command {
    Command::PresentResult {
        task_id: "task".into(),
        interaction_id: "delivery".into(),
        body: "Done".into(),
        is_final: true,
        nothing_to_review: false,
        attachments: vec![],
        checklist: vec![],
        targets: vec![DeliveryTarget::PullRequest {
            pr_id: "pr-7".into(),
            head: String::new(),
        }],
        card: CardSpec::delivery("Review changes", &[], true),
    }
}

fn metadata() -> Value {
    json!({"data":{"repository":{"visibility":"PUBLIC","pullRequest":{
        "title":"Document usage", "body":"Updated examples", "headRefOid":HEAD,
        "baseRefOid":"a".repeat(40), "baseRefName":"main", "changedFiles":1,
    }}}})
}

struct Fixture {
    home: crate::test_support::Directory,
    gh: GhCli,
}

impl Fixture {
    fn new(pages: &[Value], diff: &str) -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(path_in(&home), format!("private_names = [\"{NAME}\"]\n")).unwrap();
        std::fs::write(home.join("diff"), diff).unwrap();
        let mut script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/calls'\ncase \"$1 $2\" in\n'api graphql')\ni=0\nif test -f '{0}/counter'; then i=$(cat '{0}/counter'); fi\ni=$((i+1))\nprintf '%s' \"$i\" > '{0}/counter'\ncase \"$i\" in\n",
            home.display()
        );
        for (index, page) in pages.iter().enumerate() {
            std::fs::write(home.join(format!("page-{index}")), page.to_string()).unwrap();
            script.push_str(&format!(
                "{}) cat '{}/page-{index}';;\n",
                index + 1,
                home.display()
            ));
        }
        script.push_str(&format!(
            "*) exit 1;;\nesac;;\n'pr diff') cat '{}/diff';;\n*) exit 1;;\nesac\n",
            home.display()
        ));
        let program = home.join("gh");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            home,
            gh: GhCli::at(program),
        }
    }

    async fn check(
        &self,
        state: &DomainState,
        command: &Command,
    ) -> Result<Option<Reviewed>, DomainError> {
        check(&self.home, state, command, &self.gh).await
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.home.join("calls")).unwrap_or_default()
    }
}

#[tokio::test]
async fn private_names_in_added_documentation_block_the_final_delivery() {
    let fixture = Fixture::new(&[metadata(), metadata()], PRIVATE_DIFF);
    let error = fixture
        .check(&state(), &delivery())
        .await
        .err()
        .expect("a private name must block delivery");
    assert_eq!(error.code, "private_names_in_public_content");
    assert_eq!(error.subject.as_deref(), Some("pr-7"));
    assert!(
        error.message.contains(NAME)
            && error.message.contains("docs/usage.md")
            && error.message.contains("diff line 6"),
        "{error}"
    );
}

#[tokio::test]
async fn title_description_and_private_repository_links_are_checked() {
    for (field, text, location) in [
        ("title", "EXAMPLE-PRIVATE-PROJECT", "PR title"),
        (
            "body",
            "See https://github.com/owner/example-private-project/pull/9",
            "PR description line 1",
        ),
    ] {
        let mut page = metadata();
        page["data"]["repository"]["pullRequest"][field] = json!(text);
        let fixture = Fixture::new(&[page.clone(), page], CLEAN_DIFF);
        let error = fixture.check(&state(), &delivery()).await.err().unwrap();
        assert_eq!(error.code, "private_names_in_public_content");
        assert!(error.message.contains(location), "{error}");
    }
}

#[tokio::test]
async fn clean_public_pr_passes_but_new_heads_or_additional_prs_invalidate_the_review() {
    let fixture = Fixture::new(&[metadata(), metadata()], CLEAN_DIFF);
    let mut state = state();
    let command = delivery();
    let reviewed = fixture.check(&state, &command).await.unwrap().unwrap();
    reviewed.verify(&state).unwrap();
    state.pull_requests.get_mut(&"pr-7".into()).unwrap().head = "b".repeat(40);
    assert_eq!(
        reviewed.verify(&state).unwrap_err().code,
        "public_content_review_changed"
    );
    state.pull_requests.get_mut(&"pr-7".into()).unwrap().head = HEAD.into();
    register(&mut state, 8);
    assert_eq!(
        reviewed.verify(&state).unwrap_err().code,
        "public_content_review_changed"
    );
    assert_eq!(
        command,
        delivery(),
        "review preserves command identity for idempotent retries"
    );
}

#[tokio::test]
async fn empty_configuration_and_private_repositories_do_not_read_a_diff() {
    for config in [None, Some(""), Some("private_names = []\n")] {
        let fixture = Fixture::new(&[], PRIVATE_DIFF);
        match config {
            Some(config) => std::fs::write(path_in(&fixture.home), config).unwrap(),
            None => std::fs::remove_file(path_in(&fixture.home)).unwrap(),
        }
        assert!(
            fixture
                .check(&state(), &delivery())
                .await
                .unwrap()
                .is_none()
        );
        assert!(fixture.calls().is_empty());
    }
    for visibility in ["PRIVATE", "INTERNAL"] {
        let mut page = metadata();
        page["data"]["repository"]["visibility"] = json!(visibility);
        page["data"]["repository"]["pullRequest"]["body"] = json!(NAME);
        let fixture = Fixture::new(&[page], PRIVATE_DIFF);
        fixture.check(&state(), &delivery()).await.unwrap();
        assert!(!fixture.calls().contains("pr diff"));
    }
}

#[tokio::test]
async fn all_implicit_open_prs_are_checked() {
    let mut private = metadata();
    private["data"]["repository"]["visibility"] = json!("PRIVATE");
    let fixture = Fixture::new(&[private, metadata(), metadata()], PRIVATE_DIFF);
    let mut state = state();
    register(&mut state, 8);
    let error = fixture.check(&state, &delivery()).await.err().unwrap();
    assert_eq!(error.subject.as_deref(), Some("pr-8"));
    assert!(fixture.calls().contains("number=8"));
}

#[tokio::test]
async fn unknown_visibility_invalid_config_incomplete_diff_and_read_failures_refuse() {
    let mut unknown = metadata();
    unknown["data"]["repository"]["visibility"] = json!("UNKNOWN");
    let mut missing_file = metadata();
    missing_file["data"]["repository"]["pullRequest"]["changedFiles"] = json!(2);
    for pages in [
        vec![],
        vec![unknown],
        vec![missing_file],
        vec![json!({"errors":[{"message":"unavailable"}]})],
    ] {
        let fixture = Fixture::new(&pages, CLEAN_DIFF);
        assert_eq!(
            fixture
                .check(&state(), &delivery())
                .await
                .err()
                .unwrap()
                .code,
            "public_content_review_unavailable"
        );
    }
    for config in [
        "private_names = [\" \"]",
        "private_names = 42",
        "broken =",
        "unknown_key = true",
    ] {
        let fixture = Fixture::new(&[], CLEAN_DIFF);
        std::fs::write(path_in(&fixture.home), config).unwrap();
        assert_eq!(
            fixture
                .check(&state(), &delivery())
                .await
                .err()
                .unwrap()
                .code,
            "public_content_review_unavailable"
        );
        assert!(fixture.calls().is_empty());
    }
    let fixture = Fixture::new(&[metadata()], CLEAN_DIFF);
    std::fs::remove_file(fixture.home.join("diff")).unwrap();
    assert_eq!(
        fixture
            .check(&state(), &delivery())
            .await
            .err()
            .unwrap()
            .code,
        "public_content_review_unavailable"
    );
}

#[tokio::test]
async fn edits_during_the_review_and_stale_registered_heads_refuse() {
    for field in [
        "title",
        "body",
        "headRefOid",
        "baseRefOid",
        "baseRefName",
        "changedFiles",
    ] {
        let mut after = metadata();
        after["data"]["repository"]["pullRequest"][field] = if field == "changedFiles" {
            json!(2)
        } else {
            json!("b".repeat(40))
        };
        let fixture = Fixture::new(&[metadata(), after], CLEAN_DIFF);
        assert_eq!(
            fixture
                .check(&state(), &delivery())
                .await
                .err()
                .unwrap()
                .code,
            "public_content_review_changed",
            "{field}"
        );
    }
    let mut before = metadata();
    before["data"]["repository"]["pullRequest"]["headRefOid"] = json!("b".repeat(40));
    let fixture = Fixture::new(&[before], CLEAN_DIFF);
    assert_eq!(
        fixture
            .check(&state(), &delivery())
            .await
            .err()
            .unwrap()
            .code,
        "stale_head"
    );
}

#[tokio::test]
async fn truncated_diffs_never_pass() {
    let fixture = Fixture::new(&[metadata()], &"x".repeat(2 * 1024 * 1024 + 1));
    assert_eq!(
        fixture
            .check(&state(), &delivery())
            .await
            .err()
            .unwrap()
            .code,
        "public_content_review_unavailable"
    );
}

#[tokio::test]
async fn retry_of_a_recorded_delivery_and_progress_do_not_repeat_the_review() {
    let fixture = Fixture::new(&[], CLEAN_DIFF);
    let mut state = state();
    let command = delivery();
    execute(
        &mut state,
        Actor::Agent {
            agent_id: "node".into(),
        },
        command.clone(),
    );
    assert!(fixture.check(&state, &command).await.unwrap().is_none());
    let mut progress = delivery();
    let Command::PresentResult { is_final, .. } = &mut progress else {
        unreachable!()
    };
    *is_final = false;
    assert!(fixture.check(&state, &progress).await.unwrap().is_none());
    assert!(fixture.calls().is_empty());
}

#[test]
fn diff_scanning_checks_added_lines_and_quoted_paths_but_allows_removal() {
    assert_eq!(decoded_path(r#"b/\360\237\230\200.md"#).unwrap(), "b/😀.md");
    assert!(decoded_path(r#"b/\377.md"#).is_err());
    let names = vec![NAME.into()];
    let mut findings = Vec::new();
    inspect_diff(
        &names,
        &CLEAN_DIFF.replace("-old", &format!("-{NAME}")),
        1,
        &mut findings,
    )
    .unwrap();
    assert!(
        findings.is_empty(),
        "removing a private name must remain possible"
    );
    inspect_diff(
        &names,
        &CLEAN_DIFF.replace("+New documentation", &format!("+++{NAME}")),
        1,
        &mut findings,
    )
    .unwrap();
    assert_eq!(
        findings.len(),
        1,
        "an added line starting with ++ is still content"
    );
    findings.clear();
    inspect_diff(
        &names,
        &CLEAN_DIFF.replace("README.md", "example-private-project.md"),
        1,
        &mut findings,
    )
    .unwrap();
    assert!(!findings.is_empty());
    findings.clear();
    let diff = r#"diff --git "a/Projet-priv\303\251.md" "b/Projet-priv\303\251.md"
--- /dev/null
+++ "b/Projet-priv\303\251.md"
@@ -0,0 +1 @@
+Documentation
"#;
    inspect_diff(&["projet-privé".into()], diff, 1, &mut findings).unwrap();
    assert!(
        !findings.is_empty(),
        "Git octal path quoting must not hide a private name"
    );
}
