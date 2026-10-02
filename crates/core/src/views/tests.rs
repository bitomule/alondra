use super::*;
use crate::domain_v2::{
    Agent, Author, Document, DocumentRevision, DomainState, Interaction, InteractionPayload,
    InteractionSubject, PrState, PullRequest, QuestionOption, Recipient, Request, Scope, Task,
    TaskState,
};
use std::collections::{BTreeMap, BTreeSet};

fn fixture() -> DomainState {
    let mut state = DomainState::new("test-epoch");
    state.requests.insert(
        "r-1".into(),
        Request::new("r-1".into(), "Build a feature".into()),
    );
    state.agents.insert(
        "a-1".into(),
        Agent {
            id: "a-1".into(),
            name: "Node One".into(),
            connected: true,
        },
    );
    state.tasks.insert(
        "t-1".into(),
        Task {
            id: "t-1".into(),
            request_id: "r-1".into(),
            agent_id: None,
            title: "Build a feature".into(),
            state: TaskState::NeedsYou,
            revision: 1,
            started: true,
            review_prs: false,
            final_presentation: Some("i-3".into()),
            pr_id: None,
            dependencies: BTreeSet::new(),
            stack: None,
            documents: BTreeMap::new(),
            archived: false,
        },
    );
    for (index, kind) in ["question", "information", "delivery"].iter().enumerate() {
        let id = format!("i-{}", index + 1);
        let payload = match *kind {
            "question" => InteractionPayload::Question {
                options: vec![],
                answer: None,
            },
            "information" => InteractionPayload::Information {
                read_by: BTreeSet::new(),
            },
            _ => InteractionPayload::Delivery {
                targets: vec![],
                is_final: true,
                acceptance: None,
                feedback: vec![],
            },
        };
        state.interactions.insert(
            id.clone().into(),
            Interaction {
                attachments: vec![],
                id: id.into(),
                scope: Scope::Task {
                    task_id: "t-1".into(),
                },
                author: if index == 0 {
                    Author::Agent {
                        agent_id: "a-1".into(),
                    }
                } else {
                    Author::System
                },
                recipient: Recipient::User,
                body: format!("Body {index}"),
                blocking: true,
                subject: None,
                payload,
                revision: 1,
                order: index as u64 + 1,
                withdrawn: false,
            },
        );
    }
    state.interactions.insert(
        "i-4".into(),
        Interaction {
            attachments: vec![],
            id: "i-4".into(),
            scope: Scope::Orchestrator { request_id: None },
            author: Author::Orchestrator { generation: 1 },
            recipient: Recipient::User,
            body: "Internal".into(),
            blocking: true,
            subject: None,
            payload: InteractionPayload::Question {
                options: vec![],
                answer: None,
            },
            revision: 1,
            order: 4,
            withdrawn: false,
        },
    );
    state
}
fn render<'a>(
    domain: &'a DomainState,
    live: &'a LiveSnapshot,
    client: Client,
    lang: Lang,
    key: &str,
    time: &'a str,
) -> View {
    build(
        key,
        RenderContext {
            domain,
            live,
            client,
            lang,
            transport: Transport::Loopback,
            cursor: "cursor-1",
            rendered_at: time,
        },
    )
    .unwrap()
}
#[test]
fn action_ids_round_trip() {
    use action_id::{ActionId, Verb};
    for verb in Verb::ALL {
        for extra in [None, Some("abc123".into())] {
            let id = ActionId::new(verb, "i-42", 117, extra).unwrap();
            assert_eq!(ActionId::parse(&id.to_string()).unwrap(), id);
            assert!(id.to_string().len() <= 96);
        }
    }
    let merge = ActionId::new(Verb::Merge, "i-42", 117, Some("e5f6a7b8-i-42".into())).unwrap();
    assert_eq!(ActionId::parse(&merge.to_string()).unwrap(), merge);
    assert!(ActionId::parse("ans:i-42@117~").is_err());
    assert!(ActionId::parse("ans:i-42@117~abc:def").is_err());
}
#[test]
fn home_ios_sends_only_ordered_ids() {
    let state = fixture();
    let live = LiveSnapshot::default();
    let view = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "home",
        "2026-09-30T10:00:00Z",
    );
    assert!(view.sections[0].items.is_empty());
    assert_eq!(view.sections[0].card_ids, ["i-1", "i-2", "i-3"]);
}

#[test]
fn a_huge_inbox_stays_under_the_budget_and_reports_more() {
    let mut state = fixture();
    for index in 0..2_000 {
        let id = format!("q-{index:04}");
        state.interactions.insert(
            id.clone().into(),
            Interaction {
                attachments: vec![],
                id: id.into(),
                scope: Scope::Task {
                    task_id: "t-1".into(),
                },
                author: Author::System,
                recipient: Recipient::User,
                body: "x".repeat(8 * 1024),
                blocking: true,
                subject: None,
                payload: InteractionPayload::Question {
                    options: vec![],
                    answer: None,
                },
                revision: 1,
                order: index + 10,
                withdrawn: false,
            },
        );
    }
    let view = render(
        &state,
        &LiveSnapshot::default(),
        Client::Ios,
        Lang::En,
        "home",
        "2026-09-30T10:00:00Z",
    );
    let queue = &view.sections[0];
    assert!(queue.items.is_empty());
    assert_eq!(queue.card_ids.first().unwrap(), "i-1");
    assert_eq!(queue.card_ids.last().unwrap(), "q-1999");
    let bytes = serde_json::to_vec(&view).unwrap();
    assert!(bytes.len() < 744 * 1024, "home used {} bytes", bytes.len());
    assert_eq!(queue.card_ids.len(), 2_003, "every remaining card is reported by id");
}
#[test]
fn focus_puts_the_id_first() {
    let state = fixture();
    let live = LiveSnapshot::default();
    let view = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "home?focus=i-3",
        "2026-09-30T10:00:00Z",
    );
    assert!(view.sections[0].items.is_empty());
    assert_eq!(view.sections[0].card_ids[0], "i-3");
}
#[test]
fn version_ignores_time() {
    let state = fixture();
    let live = LiveSnapshot::default();
    let a = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "home",
        "2026-09-30T10:00:00Z",
    );
    let b = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "home",
        "2026-09-30T11:00:00Z",
    );
    assert_eq!(a.version, b.version);
}
#[test]
fn orchestrator_items_are_not_in_home() {
    let state = fixture();
    let live = LiveSnapshot::default();
    let view = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "home",
        "2026-09-30T10:00:00Z",
    );
    assert!(!view.sections[0].card_ids.contains(&"i-4".to_owned()));
    assert_eq!(view.status.badge, view.sections[0].card_ids.len() as u32);
    assert_eq!(
        view.status.summary,
        "1 question · 2 to read · 0 nodes working"
    );
}
#[test]
fn pull_request_contains_its_question() {
    let mut state = fixture();
    state.pull_requests.insert(
        "pr-1".into(),
        PullRequest {
            id: "pr-1".into(),
            task_id: "t-1".into(),
            host: "github".into(),
            repository: "example/repo".into(),
            number: 12,
            head: "abcdef12".into(),
            base: "main".into(),
            state: PrState::Open,
            merged_head: None,
            allowed_methods: vec![],
            review: None,
        },
    );
    state.interactions.insert(
        "i-5".into(),
        Interaction {
            attachments: vec![],
            id: "i-5".into(),
            scope: Scope::Task {
                task_id: "t-1".into(),
            },
            author: Author::System,
            recipient: Recipient::User,
            body: "Review the PR".into(),
            blocking: true,
            subject: Some(InteractionSubject::PullRequest {
                pr_id: "pr-1".into(),
            }),
            payload: InteractionPayload::Question {
                options: vec![],
                answer: None,
            },
            revision: 1,
            order: 5,
            withdrawn: false,
        },
    );
    let live = LiveSnapshot::default();
    let view = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "home",
        "2026-09-30T10:00:00Z",
    );
    assert!(view.sections[0].card_ids.contains(&"pr-1".to_owned()));
    assert!(!view.sections[0].card_ids.contains(&"i-5".to_owned()));
    assert!(view.sections[0].items.is_empty());
    let detail = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "card:pr-1",
        "2026-09-30T10:00:00Z",
    );
    let Item::Card(pr) = &detail.sections[0].items[0] else {
        panic!("PR detail must be card")
    };
    assert_eq!(pr.children[0].id, "i-5");
}

fn pr_fixture() -> DomainState {
    let mut state = fixture();
    state.pull_requests.insert(
        "pr-1".into(),
        PullRequest {
            id: "pr-1".into(),
            task_id: "t-1".into(),
            host: "github".into(),
            repository: "example/repo".into(),
            number: 12,
            head: "abcdef12".into(),
            base: "main".into(),
            state: PrState::Open,
            merged_head: None,
            allowed_methods: vec![],
            review: None,
        },
    );
    state.interactions.insert(
        "i-5".into(),
        Interaction {
            attachments: vec![],
            id: "i-5".into(),
            scope: Scope::Task {
                task_id: "t-1".into(),
            },
            author: Author::System,
            recipient: Recipient::User,
            body: "Review the PR".into(),
            blocking: true,
            subject: Some(InteractionSubject::PullRequest {
                pr_id: "pr-1".into(),
            }),
            payload: InteractionPayload::Question {
                options: vec![],
                answer: None,
            },
            revision: 1,
            order: 5,
            withdrawn: false,
        },
    );
    state.interactions.insert(
        "i-6".into(),
        Interaction {
            attachments: vec![],
            id: "i-6".into(),
            scope: Scope::Task {
                task_id: "t-1".into(),
            },
            author: Author::Agent {
                agent_id: "a-1".into(),
            },
            recipient: Recipient::User,
            body: "The PR checks passed".into(),
            blocking: false,
            subject: Some(InteractionSubject::PullRequest {
                pr_id: "pr-1".into(),
            }),
            payload: InteractionPayload::Information {
                read_by: BTreeSet::new(),
            },
            revision: 1,
            order: 6,
            withdrawn: false,
        },
    );
    state
}

fn golden_fixture() -> DomainState {
    let mut state = pr_fixture();
    let request_id = "r-9f2156b5a8894e53901f0af576dff6a2";
    let task_id = "t-651c38484cd2f34e74d5107ba05f7f15";
    let interaction_ids = [
        ("i-1", "a98d034b-249e-4487-a89b-57de38c26372"),
        ("i-2", "f12d034b-249e-4487-a89b-57de38c26372"),
        ("i-3", "hf-weekly-2026-09-30"),
        ("i-5", "8fa4034b-249e-4487-a89b-57de38c26372"),
        ("i-6", "cc2d034b-249e-4487-a89b-57de38c26372"),
    ];
    let agent_id = "d887a34b-249e-4487-a89b-57de38c26372";
    let pr_id = "github.com/example/repo#12";
    if let Some(request) = state.requests.get_mut(&"r-1".into()) {
        request.title = Some("Polish the Alondra beta".into());
        request.body = "Prepare the next Alondra beta: restore the established review experience, verify the simulator build, and keep the release notes concise.".into();
    }
    if let Some(task) = state.tasks.get_mut(&"t-1".into()) {
        task.title = "Restore the native review experience".into();
    }
    if let Some(agent) = state.agents.get_mut(&"a-1".into()) {
        agent.name = "Alondra · iOS".into();
    }
    if let Some(interaction) = state.interactions.get_mut(&"i-1".into()) {
        interaction.body = "The new empty state is ready. Which copy should we ship in the beta?".into();
        interaction.payload = InteractionPayload::Question {
            options: vec![
                QuestionOption {
                    id: "clear".into(),
                    label: "Ship the concise copy".into(),
                    description: Some("Use a short title and one explanatory sentence.".into()),
                    inputs: vec![],
                },
                QuestionOption {
                    id: "warm".into(),
                    label: "Keep the warmer copy".into(),
                    description: Some("Preserve the more conversational explanation.".into()),
                    inputs: vec![],
                },
            ],
            answer: None,
        };
    }
    if let Some(interaction) = state.interactions.get_mut(&"i-2".into()) {
        interaction.body = "The iPhone simulator pass completed successfully. Swipe navigation, inline replies, and attachment previews are ready for review.".into();
    }
    if let Some(interaction) = state.interactions.get_mut(&"i-3".into()) {
        interaction.body = "## Beta candidate ready\n\n- Restored the native review layouts\n- Kept swipe navigation between cards\n- Verified the ARM simulator build\n\nThe candidate is ready for your final approval.".into();
    }
    if let Some(interaction) = state.interactions.get_mut(&"i-5".into()) {
        interaction.body = "The pull request is ready. Should the compact schedule editor ship with this beta?".into();
    }
    if let Some(interaction) = state.interactions.get_mut(&"i-6".into()) {
        interaction.body = "All required checks passed and the branch is up to date with main.".into();
    }
    let old_interactions = std::mem::take(&mut state.interactions);
    state.interactions = old_interactions.into_values().map(|mut interaction| {
        if let Some((_, replacement)) = interaction_ids.iter().find(|(old, _)| *old == interaction.id.0) {
            interaction.id = (*replacement).into();
        }
        if let Author::Agent { agent_id: id } = &mut interaction.author {
            if id.0 == "a-1" { *id = agent_id.into(); }
        }
        if let Some(InteractionSubject::PullRequest { pr_id: id }) = &mut interaction.subject {
            if id.0 == "pr-1" { *id = pr_id.into(); }
        }
        (interaction.id.clone(), interaction)
    }).collect();
    let old_prs = std::mem::take(&mut state.pull_requests);
    state.pull_requests = old_prs.into_values().map(|mut pr| {
        if pr.id.0 == "pr-1" { pr.id = pr_id.into(); }
        (pr.id.clone(), pr)
    }).collect();
    let old_agents = std::mem::take(&mut state.agents);
    state.agents = old_agents.into_values().map(|mut agent| {
        if agent.id.0 == "a-1" { agent.id = agent_id.into(); }
        (agent.id.clone(), agent)
    }).collect();
    let mut request = state.requests.remove(&"r-1".into()).unwrap();
    request.id = request_id.into();
    state.requests.insert(request.id.clone(), request);
    let mut task = state.tasks.remove(&"t-1".into()).unwrap();
    task.id = task_id.into();
    task.request_id = request_id.into();
    state.tasks.insert(task.id.clone(), task);
    for interaction in state.interactions.values_mut() {
        if let Scope::Task { task_id: id } = &mut interaction.scope {
            if id.0 == "t-1" { *id = task_id.into(); }
        }
    }
    for pr in state.pull_requests.values_mut() {
        if pr.task_id.0 == "t-1" { pr.task_id = task_id.into(); }
    }
    if let Some(task) = state.tasks.get_mut(&task_id.into()) {
        task.final_presentation = Some("hf-weekly-2026-09-30".into());
    }
    state
}

fn golden_live() -> LiveSnapshot {
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode {
        agent_id: "d887a34b-249e-4487-a89b-57de38c26372".into(),
        short_id: Some("alondra-ios".into()),
        cwd: Some("/demo/Demo Garden/ios".into()),
        name: "Alondra · iOS".into(),
        project: Some("Demo Garden".into()),
        state_key: Some("waiting_answer".into()),
        state_label: Some("Waiting for you".into()),
        summary: Some("Reviewing the native beta experience".into()),
        needs: Some("A decision on the welcome copy".into()),
        background: vec![],
        background_badge: None,
        background_suspect: false,
        working: false,
        waiting: true,
        last_message: Some("The simulator pass completed successfully.".into()),
        state_since: None,
        last_message_at: Some("2026-09-30T09:58:00Z".into()),
    });
    live.pull_requests.push(LivePullRequest {
        id: "github.com/example/repo#12".into(),
        title: Some("Add the compact schedule editor".into()),
        body: Some("Replaces the long schedule form with a compact editor.\n\n- Repeat and time on one row\n- Launch options in a sheet".into()),
        head_ref: Some("feature/compact-schedule-editor".into()),
        base_ref: Some("main".into()),
        checks: vec![
            LiveCheck { name: "Build".into(), state: CheckState::Pass },
            LiveCheck { name: "Unit tests".into(), state: CheckState::Pass },
            LiveCheck { name: "Lint".into(), state: CheckState::Pending },
        ],
        checks_passed: Some(true),
        additions: 120,
        deletions: 30,
        changed_files: 4,
        url: Some("https://github.com/example/repo/pull/12".into()),
        ..Default::default()
    });
    live.activity.insert("d887a34b-249e-4487-a89b-57de38c26372".into(), vec![LiveActivity {
        agent_id: Some("worker-1".into()),
        agent_type: Some("general-purpose".into()),
        task: Some("Check the Spanish strings".into()),
        tool: Some("Read".into()),
        summary: Some("Reading the localization catalog".into()),
        since: "2026-09-30T09:55:00Z".into(),
    }]);
    live.task_history.insert("t-651c38484cd2f34e74d5107ba05f7f15".into(), vec![
        LiveHistoryStep { at: "2026-09-30T08:00:00Z".into(), state: "running".into(), summary: "Started".into() },
        LiveHistoryStep { at: "2026-09-30T09:30:00Z".into(), state: "needs_you".into(), summary: "Waiting for a decision".into() },
    ]);
    live.projects.push(crate::options::ProjectEntry {
        name: "Demo Garden".into(),
        path: "/demo/Demo Garden".into(),
        last_used: Some(1_759_226_280_000),
        nodes: 1,
    });
    live.options = crate::options::OptionsFile::default().catalog(None);
    live.gauges = vec![
        Gauge { id: "memory".into(), label: "Memory".into(), value: 0.38,
            text: "6.1 GB of 16 GB".into(), detail: None, level: Level::Calm },
        Gauge { id: "disk".into(), label: "Disk available".into(), value: 0.72,
            text: "120 GB".into(), detail: None, level: Level::Calm },
    ];
    live
}

#[test]
fn real_pr_id_opens_detail_diff_and_actions() {
    let mut state = pr_fixture();
    let id = "github.com/bitomule/alondra#154";
    let mut pr = state.pull_requests.remove(&"pr-1".into()).unwrap();
    pr.id = id.into();
    state.pull_requests.insert(id.into(), pr);
    for interaction in state.interactions.values_mut() {
        if let Some(InteractionSubject::PullRequest { pr_id }) = &mut interaction.subject {
            *pr_id = id.into();
        }
    }
    let live = LiveSnapshot::default();
    let card = render(&state, &live, Client::Ios, Lang::En, &format!("card:{id}"), "");
    let Item::Card(card) = &card.sections[0].items[0] else { panic!("PR card") };
    assert!(card.actions.iter().any(|action| action.label == "Ask about the PR" &&
        action_id::ActionId::parse(&action.id).is_ok()));
    assert!(push_actions(&state, id, Lang::En).unwrap().iter().any(|action|
        action.id.starts_with("ask_pr:github.com/bitomule/alondra#154@")));
    let detail = render(&state, &live, Client::Mac, Lang::En, &format!("pull_request:{id}"), "");
    assert!(detail.sections.iter().flat_map(|section| &section.items).any(|item|
        matches!(item, Item::ContentRef(content) if content.fetch ==
            "/pull-requests/github.com%2Fbitomule%2Falondra%23154/diff")));
    let tui = render(&state, &live, Client::Tui, Lang::En, &format!("card:{id}"), "");
    let Item::Card(tui_card) = &tui.sections[0].items[0] else { panic!("PR card") };
    assert!(tui_card.actions.iter().any(|action| action.key.as_deref() == Some("d") &&
        matches!(&action.link, Some(Link::Local { local: LocalLink::ReviewClone { pr_id, pr: 12, .. } }) if pr_id == id)));
}

#[test]
fn invalid_generated_pr_action_id_is_omitted_while_the_card_renders() {
    let mut state = golden_fixture();
    let pr_id = "github.com/example/repo#12";
    state.pull_requests.get_mut(&pr_id.into()).unwrap().head = "bad/head123".into();
    let mut live = LiveSnapshot::default();
    live.pull_requests.push(crate::views::live::LivePullRequest {
        id: pr_id.into(), title: None, body: None, head_ref: None, base_ref: None,
        checks_passed: Some(true), conflicts: true, stacked_on: None, url: None,
        ..Default::default()
    });
    let view = render(&state, &live, Client::Ios, Lang::En, &format!("card:{pr_id}"), "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("the card remains visible") };
    assert!(card.actions.iter().any(|action| action.label == "Ask about the PR"));
    assert!(!card.actions.iter().any(|action| action.label == "Request changes"));
    assert!(card.actions.iter().any(|action| action.label == "Resolve conflicts"),
        "a valid action after the invalid action should still render");
    let result = build(&format!("card:{pr_id}"), RenderContext {
        domain: &state,
        live: &live,
        client: Client::Ios,
        lang: Lang::En,
        transport: Transport::Loopback,
        cursor: "test",
        rendered_at: "",
    });
    assert!(result.is_ok());
}

#[test]
fn agent_uuid_keeps_message_and_close_actions() {
    let mut state = fixture();
    let id = "a98d034b-249e-4487-a89b-57de38c26372";
    let mut agent = state.agents.remove(&"a-1".into()).unwrap();
    agent.id = id.into();
    state.agents.insert(id.into(), agent);
    let live = LiveSnapshot::default();
    let detail = render(&state, &live, Client::Tui, Lang::En, &format!("node:{id}"), "");
    let actions = detail.sections.iter().flat_map(|section| &section.items).find_map(|item|
        if let Item::Actions(bar) = item { Some(&bar.actions) } else { None }).unwrap();
    assert!(actions.iter().any(|action| action.id.starts_with(&format!("msg:{id}@"))));
    assert!(actions.iter().any(|action| action.id.starts_with(&format!("close:{id}@"))));
}
fn assert_golden(filename: &str, actual: &View, expected: &str) {
    let actual = serde_json::to_value(actual).unwrap();
    let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
    assert_eq!(actual, expected, "{filename}");
}

#[test]
fn view_keys_are_addressable_and_bound_to_epoch() {
    let mut state = fixture();
    state.agents.get_mut(&"a-1".into()).unwrap().connected = true;
    let live = LiveSnapshot::default();
    for (key, client) in [
        ("sidebar", Client::Mac),
        ("tasks", Client::Ios),
        ("task:t-1", Client::Ios),
        ("request:r-1", Client::Mac),
        ("fleet", Client::Tui),
        ("node:a-1", Client::Ios),
        ("schedules", Client::Mac),
        ("schedule:new", Client::Ios),
        ("new_request", Client::Ios),
        ("issue_picker", Client::Tui),
        ("search?q=Node", Client::Mac),
        ("machine", Client::Ios),
    ] {
        let view = render(&state, &live, client, Lang::En, key, "2026-09-30T10:00:00Z");
        assert_eq!(view.key, key);
        assert_eq!(view.data_epoch, "test-epoch");
        assert!(!view.version.is_empty());
    }
    assert_eq!(
        build(
            "sidebar",
            RenderContext {
                domain: &state,
                live: &live,
                client: Client::Ios,
                lang: Lang::En,
                transport: Transport::Loopback,
                cursor: "cursor-1",
                rendered_at: "2026-09-30T10:00:00Z"
            }
        ),
        Err(BuildError::UnsupportedClient)
    );
}

#[test]
fn search_view_counts_found_agents_and_reports_empty_results() {
    let state = fixture();
    let live = LiveSnapshot::default();
    let found = render(&state, &live, Client::Tui, Lang::En, "search?q=Node", "");
    assert_eq!(found.sections[0].title.as_deref(), Some("1 result found"));
    let missing = render(&state, &live, Client::Tui, Lang::En, "search?q=NoSuchNode", "");
    assert_eq!(missing.empty.as_ref().map(|notice| notice.text.as_str()), Some("No results found"));
}

#[test]
fn document_detail_exposes_the_revision_content_route() {
    let mut state = fixture();
    state.documents.insert("document-1".into(), Document {
        id: "document-1".into(), title: "Design notes".into(), current_revision: "revision-1".into(),
    });
    state.document_revisions.insert("revision-1".into(), DocumentRevision {
        id: "revision-1".into(), document_id: "document-1".into(), number: 1,
        content_digest: "a".repeat(64), content_reference: "stored/design".into(),
    });
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "document:revision-1", "");
    assert_eq!(view.title.as_deref(), Some("Design notes"));
    let content = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::ContentRef(content) = item { Some(content) } else { None }
    }).expect("document content remains fetchable by revision");
    assert_eq!(content.fetch, "/document-revisions/revision-1/content");
    assert!(content.commentable);
}

#[test]
fn document_review_card_exposes_revision_content_and_review_actions() {
    let mut state = fixture();
    state.documents.insert("document-1".into(), Document {
        id: "document-1".into(), title: "Design notes".into(), current_revision: "revision-1".into(),
    });
    state.document_revisions.insert("revision-1".into(), DocumentRevision {
        id: "revision-1".into(), document_id: "document-1".into(), number: 1,
        content_digest: "b".repeat(64), content_reference: "stored/design".into(),
    });
    let interaction = state.interactions.get_mut(&"i-3".into()).unwrap();
    if let InteractionPayload::Delivery { targets, .. } = &mut interaction.payload {
        *targets = vec![crate::domain_v2::DeliveryTarget::DocumentRevision {
            document_id: "document-1".into(), revision_id: "revision-1".into(),
        }];
    } else { panic!("delivery fixture") }
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:revision-1", "");
    let content = view.sections.iter().flat_map(|section| &section.items).find_map(|item|
        if let Item::ContentRef(content) = item { Some(content) } else { None })
        .expect("document review card exposes the revision content");
    assert_eq!(content.kind, ContentKind::Document);
    assert_eq!(content.fetch, "/document-revisions/revision-1/content");
    let digest = "b".repeat(64);
    assert_eq!(content.digest.as_deref(), Some(digest.as_str()));
    assert!(content.commentable);
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("review card") };
    assert!(card.actions.iter().any(|action| action.label == "Approve"
        && action.style == Style::Primary
        && action.stamp.as_deref() == Some("Approved")), "{:?}", card.actions);
    assert!(card.actions.iter().any(|action| action.label == "Request changes"));
}

fn plan_question_fixture() -> DomainState {
    let mut state = fixture();
    state.documents.insert("plan".into(), Document {
        id: "plan".into(), title: "Plan: one task per node".into(), current_revision: "plan-r1".into(),
    });
    state.document_revisions.insert("plan-r1".into(), DocumentRevision {
        id: "plan-r1".into(), document_id: "plan".into(), number: 1,
        content_digest: "c".repeat(64), content_reference: "stored/plan".into(),
    });
    state.tasks.get_mut(&"t-1".into()).unwrap().documents.insert("plan".into(), "plan".into());
    let question = state.interactions.get_mut(&"i-1".into()).unwrap();
    question.payload = InteractionPayload::Question {
        options: vec![QuestionOption { id: "separate".into(), label: "Approve; node messaging in its own PR".into(),
            description: None, inputs: vec![] }],
        answer: None,
    };
    question.subject = Some(InteractionSubject::DocumentReview { document_id: "plan".into(), revision_id: "plan-r1".into() });
    state
}

fn document_content(view: &View) -> Option<&ContentRef> {
    view.sections.iter().flat_map(|section| &section.items).find_map(|item|
        if let Item::ContentRef(content) = item { Some(content) } else { None })
}

#[test]
fn a_question_about_a_plan_opens_the_plan_for_review_with_its_options_and_request_changes() {
    let state = plan_question_fixture();
    for key in ["card:plan-r1", "card:i-1"] {
        let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, key, "");
        let content = document_content(&view).unwrap_or_else(|| panic!("{key} shows the plan"));
        assert_eq!((content.kind, content.fetch.as_str(), content.commentable),
            (ContentKind::Document, "/document-revisions/plan-r1/content", true), "{key}");
        let card = card_of(&view);
        let answer = card.actions.iter().find(|action| action.id.starts_with("ans:i-1@")).expect("the node's options");
        assert_eq!(answer.options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(), ["separate"]);
        let changes = card.actions.iter().find(|action| action.id.starts_with("changes:i-1@")).expect("request changes");
        assert_eq!((changes.label.as_str(), changes.stamp.as_deref(), changes.enabled), ("Pedir cambios", Some("Cambios solicitados"), true));
        assert_eq!(changes.comments, Some(CommentSpec { kind: CommentKind::Blocks }));
        assert!(changes.key.is_none());
    }
    let root = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:plan-r1", "");
    let card = card_of(&root);
    assert_eq!((card.icon, card.eyebrow.as_ref().unwrap().text.as_str(), card.title.as_str()),
        (Icon::Document, "Document", "Plan: one task per node"));
    assert!(card.children.iter().all(|child| child.actions.is_empty()), "the review card carries the answer");
}

#[test]
fn a_question_linked_to_no_document_keeps_its_plain_answer() {
    let mut state = plan_question_fixture();
    state.interactions.get_mut(&"i-1".into()).unwrap().subject = None;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert!(document_content(&view).is_none());
    let card = card_of(&view);
    assert_eq!(card.icon, Icon::Question);
    assert!(card.actions.iter().all(|action| !action.id.starts_with("changes:")), "{:?}", card.actions);
}

fn delivery_card_actions(state: &DomainState, lang: Lang, key: &str) -> Vec<Action> {
    let view = render(state, &LiveSnapshot::default(), Client::Ios, lang, key, "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("delivery card") };
    card.actions.clone()
}

fn reply_to_node(actions: &[Action]) -> &Action {
    let reply = actions
        .iter()
        .find(|action| action.id.starts_with("reply:"))
        .expect("every delivery can be answered to its node");
    assert!(reply.comments.is_none());
    reply
}

fn attach_document(state: &mut DomainState) {
    state.documents.insert("document-1".into(), Document {
        id: "document-1".into(), title: "Design notes".into(), current_revision: "revision-1".into(),
    });
    state.document_revisions.insert("revision-1".into(), DocumentRevision {
        id: "revision-1".into(), document_id: "document-1".into(), number: 1,
        content_digest: "b".repeat(64), content_reference: "stored/design".into(),
    });
    if let InteractionPayload::Delivery { targets, .. } = &mut state.interactions.get_mut(&"i-3".into()).unwrap().payload {
        *targets = vec![crate::domain_v2::DeliveryTarget::DocumentRevision {
            document_id: "document-1".into(), revision_id: "revision-1".into(),
        }];
    }
}

#[test]
fn plain_delivery_offers_accept_and_a_reply_to_the_node_only() {
    let mut state = fixture();
    state.interactions.get_mut(&"i-3".into()).unwrap().author = Author::Agent { agent_id: "a-1".into() };
    let actions = delivery_card_actions(&state, Lang::En, "card:i-3");
    assert!(actions.iter().any(|action| action.label == "Accept" && action.style == Style::Primary));
    assert!(actions.iter().all(|action| !action.id.starts_with("changes:")), "{actions:?}");
    let reply = reply_to_node(&actions);
    assert_eq!(reply.label, "Send");
    assert!(matches!(&reply.input, Input::Text { placeholder, required: true, .. } if placeholder == "Reply to Node One…"));
    assert_eq!(reply.stamp.as_deref(), Some("Sent"));
}

#[test]
fn delivery_reply_placeholder_is_localized_and_falls_back_to_this_node() {
    let state = fixture();
    let spanish = delivery_card_actions(&state, Lang::Es, "card:i-3");
    let reply = reply_to_node(&spanish);
    assert_eq!(reply.label, "Enviar");
    assert!(matches!(&reply.input, Input::Text { placeholder, .. } if placeholder == "Responder a este nodo…"));
    let english = delivery_card_actions(&state, Lang::En, "card:i-3");
    assert!(matches!(&reply_to_node(&english).input, Input::Text { placeholder, .. } if placeholder == "Reply to this node…"));
}

#[test]
fn delivery_with_a_document_offers_the_review_with_comments_beside_the_reply() {
    let mut state = fixture();
    attach_document(&mut state);
    let actions = delivery_card_actions(&state, Lang::En, "card:revision-1");
    assert!(actions.iter().any(|action| action.label == "Approve" && action.style == Style::Primary));
    let changes = actions.iter().find(|action| action.id.starts_with("changes:")).expect("review feedback");
    assert!(changes.comments.is_some());
    reply_to_node(&actions);
}

#[test]
fn delivery_carrying_a_claude_artifact_offers_request_changes_beside_the_reply() {
    let mut state = fixture();
    state.interactions.get_mut(&"i-3".into()).unwrap().body =
        "Plan ready: https://claude.ai/artifact/abc123".into();
    let actions = delivery_card_actions(&state, Lang::En, "card:i-3");
    let changes = actions.iter().find(|action| action.label == "Request changes").expect("request changes");
    assert!(changes.comments.is_some());
    assert!(matches!(&changes.input, Input::Text { placeholder, .. } if placeholder == "Describe the changes"));
    assert!(actions.iter().any(|action| action.label == "Accept" && action.style == Style::Primary));
    reply_to_node(&actions);
}

#[test]
fn pull_request_card_exposes_the_diff_and_read_actions_stay_primary() {
    let state = golden_fixture();
    let pr_id = "github.com/example/repo#12";
    let view = render(
        &state,
        &LiveSnapshot::default(),
        Client::Ios,
        Lang::En,
        &format!("card:{pr_id}"),
        "",
    );
    let diff = view
        .sections
        .iter()
        .flat_map(|section| &section.items)
        .find_map(|item| match item {
            Item::ContentRef(content) if content.kind == ContentKind::Diff => Some(content),
            _ => None,
        })
        .expect("PR card exposes the diff used by the review surface");
    assert_eq!(
        diff.fetch,
        "/pull-requests/github.com%2Fexample%2Frepo%2312/diff"
    );
    assert!(diff.commentable);

    let information = render(
        &state,
        &LiveSnapshot::default(),
        Client::Ios,
        Lang::En,
        "card:f12d034b-249e-4487-a89b-57de38c26372",
        "",
    );
    let Item::Card(card) = &information.sections[0].items[0] else {
        panic!("information card")
    };
    let read = card
        .actions
        .iter()
        .find(|action| action.label == "Mark as read")
        .expect("information card keeps its read action");
    assert_eq!(read.style, Style::Primary);
}

#[test]
fn question_card_exposes_free_text_answer_alongside_options() {
    let view = render(&fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("question card") };
    let answer = card.actions.iter().find(|action| action.id.starts_with("ans:")).expect("answer action");
    assert_eq!(answer.presentation, Presentation::Options);
    assert!(matches!(&answer.input, Input::Text { required: false, multiline: true, .. }));
}

#[test]
fn pull_request_detail_uses_live_title_body_and_branch_names() {
    let mut state = fixture();
    state.pull_requests.insert("pr-1".into(), PullRequest {
        id: "pr-1".into(), task_id: "t-1".into(), host: "github.com".into(),
        repository: "owner/repo".into(), number: 154, head: "0123456789abcdef".into(),
        base: "main".into(), state: PrState::Open, merged_head: None,
        allowed_methods: vec![], review: None,
    });
    let mut live = LiveSnapshot::default();
    live.pull_requests.push(LivePullRequest {
        id: "pr-1".into(), title: Some("Readable task editor".into()),
        body: Some("Why this change matters".into()), head_ref: Some("task/editor".into()),
        base_ref: Some("main".into()), checks_passed: None, conflicts: false,
        stacked_on: None, url: None,
        ..Default::default()
    });
    let view = render(&state, &live, Client::Tui, Lang::En, "pull_request:pr-1", "");
    assert_eq!(view.title.as_deref(), Some("Readable task editor"));
    let facts = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Facts(facts) = item { Some(&facts.items) } else { None }
    }).unwrap();
    assert!(facts.iter().any(|fact| fact.label == "Head" && fact.value == "task/editor"));
    assert!(facts.iter().all(|fact| !fact.value.contains("0123456789abcdef")));
    assert!(view.sections.iter().any(|section| section.items.iter().any(|item|
        matches!(item, Item::Markdown(markdown) if markdown.text == "Why this change matters"))));
}

#[test]
fn task_detail_keeps_dependencies_options_and_human_request_text() {
    let mut state = fixture();
    state.requests.get_mut(&"r-1".into()).unwrap().title = None;
    let mut dependency = state.tasks[&"t-1".into()].clone();
    dependency.id = "t-2".into();
    dependency.title = "Prepare API".into();
    dependency.dependencies.clear();
    state.tasks.insert(dependency.id.clone(), dependency);
    state.tasks.get_mut(&"t-1".into()).unwrap().dependencies.insert("t-2".into());
    let mut live = LiveSnapshot::default();
    live.task_options.insert("t-1".into(), LiveTaskOptions {
        project: Some("Alondra".into()), effort: Some("high".into()),
    });
    let view = render(&state, &live, Client::Tui, Lang::En, "task:t-1", "");
    let request = view.sections.iter().find(|section| section.id == "header").unwrap()
        .items.iter().find_map(|item| if let Item::Facts(facts) = item { Some(&facts.items) } else { None }).unwrap()
        .iter().find(|fact| fact.label == "Request").unwrap();
    assert_eq!(request.value, "Build a feature");
    assert!(view.sections.iter().any(|section| section.id == "dependencies"));
    let options = view.sections.iter().find(|section| section.id == "options").unwrap();
    let facts = options.items.iter().find_map(|item| if let Item::Facts(facts) = item { Some(&facts.items) } else { None }).unwrap();
    assert!(facts.iter().any(|fact| fact.label == "Project" && fact.value == "Alondra"));
    assert!(facts.iter().any(|fact| fact.label == "Effort" && fact.value == "High"));
}

#[test]
fn fleet_node_rows_keep_their_assigned_task_rows() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "fleet", "");
    let node = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Row(row) = item && row.id == "a-1" { Some(row) } else { None }
    }).unwrap();
    assert_eq!(node.id, "a-1");
    assert!(node.children.iter().any(|task| task.id == "t-1" && task.title == "Build a feature"));
}

#[test]
fn a_fleet_node_shows_the_one_task_it_is_on_not_the_ones_it_finished() {
    let mut state = fixture();
    let mut earlier = state.tasks[&"t-1".into()].clone();
    earlier.id = "t-0".into();
    earlier.title = "Earlier work".into();
    earlier.state = TaskState::Done;
    state.tasks.insert(earlier.id.clone(), earlier);
    for task in state.tasks.values_mut() {
        task.agent_id = Some("a-1".into());
    }
    let node_row = |state: &DomainState| {
        let view = render(state, &LiveSnapshot::default(), Client::Tui, Lang::En, "fleet", "");
        view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
            if let Item::Row(row) = item && row.id == "a-1" { Some(row.children.iter().map(|task| task.id.clone()).collect::<Vec<_>>()) } else { None }
        }).unwrap()
    };
    assert_eq!(node_row(&state), ["t-1"]);
    let detail = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "node:a-1", "");
    assert!(header_facts(&detail).iter().any(|fact| fact.label == "Task" && fact.value == "Build a feature"));
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    let idle = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "node:a-1", "");
    assert!(header_facts(&idle).iter().all(|fact| fact.label != "Task"), "a node between tasks is on none");
    state.tasks.get_mut(&"t-1".into()).unwrap().revision = 2;
    assert_eq!(node_row(&state), ["t-1"], "between tasks a node shows the last one it had");
}

#[test]
fn schedule_editor_includes_selected_projects_launch_options() {
    let state = fixture();
    let mut live = LiveSnapshot::default();
    live.project_options.insert("App".into(), vec![crate::options::Spec {
        id: "run_checks".into(), label: "Run checks".into(),
        kind: crate::options::Kind::Toggle, values: vec![],
        default: crate::options::Choice::Toggle(true), builtin: false,
    }]);
    let view = render(&state, &live, Client::Tui, Lang::En,
        "schedule:new?project=App", "");
    let Item::Form(form) = &view.sections[0].items[0] else { panic!("schedule form") };
    let option = form.fields.iter().find(|field| field.id == "run_checks").unwrap();
    assert_eq!(option.value, serde_json::json!(true));
}

#[test]
fn new_schedule_keeps_deterministic_weekday_and_unset_start_date_defaults() {
    let view = render(&fixture(), &LiveSnapshot::default(), Client::Tui, Lang::En, "schedule:new", "");
    let form = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Form(form) = item { Some(form) } else { None }
    }).unwrap();
    assert_eq!(form.fields.iter().find(|field| field.id == "weekday").unwrap().value, serde_json::json!(1));
    assert_eq!(form.fields.iter().find(|field| field.id == "start_at").unwrap().value, serde_json::Value::Null);
}

#[test]
fn tasks_hide_zero_scheduled_count_and_archive_action_until_completed_tasks_exist() {
    let mut state = fixture();
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "tasks", "");
    let scheduled = view.sections.iter().find(|section| section.id == "scheduled").unwrap();
    assert_eq!(scheduled.count, None);
    assert!(!view.sections.iter().flat_map(|section| &section.items).any(|item|
        matches!(item, Item::Actions(actions) if actions.actions.iter().any(|action| action.id.starts_with("archive_done:")))));

    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "tasks", "");
    assert!(view.sections.iter().flat_map(|section| &section.items).any(|item|
        matches!(item, Item::Actions(actions) if actions.actions.iter().any(|action| action.id.starts_with("archive_done:")))));
}

#[test]
fn a_request_row_links_to_its_most_recently_changed_task() {
    let mut state = fixture();
    let mut later = state.tasks[&"t-1".into()].clone();
    later.id = "t-2".into();
    later.revision = 7;
    state.tasks.insert("t-2".into(), later);
    let mut earlier = state.tasks[&"t-1".into()].clone();
    earlier.id = "t-0".into();
    earlier.revision = 3;
    state.tasks.insert("t-0".into(), earlier);
    state.requests.insert("r-2".into(), Request::new("r-2".into(), "Still splitting".into()));

    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "tasks", "");
    let link = |id: &str| view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
        Item::Row(row) if row.id == id => Some(row.link.clone()),
        _ => None,
    }).unwrap();
    assert_eq!(link("r-1"), Some(Link::View { key: "task:t-2".into() }));
    assert_eq!(link("r-2"), None);
}

#[test]
fn only_the_iphone_gets_no_link_to_the_request_view() {
    let state = fixture();
    let request_fact_link = |client| {
        let view = render(&state, &LiveSnapshot::default(), client, Lang::En, "task:t-1", "");
        view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
            Item::Facts(facts) => facts.items.iter().find(|fact| fact.label == "Request").map(|fact| fact.link.clone()),
            _ => None,
        }).unwrap()
    };
    assert_eq!(request_fact_link(Client::Ios), None);
    assert_eq!(request_fact_link(Client::Tui), Some(Link::View { key: "request:r-1".into() }));
    assert_eq!(request_fact_link(Client::Mac), Some(Link::View { key: "request:r-1".into() }));
}

#[test]
fn schedule_editor_resets_old_options_when_project_changes() {
    let mut state = fixture();
    let schedule: crate::domain_v2::Schedule = serde_json::from_value(serde_json::json!({
        "id":"schedule-1", "title":"Morning", "text":"Review work",
        "project":"Old", "options":{"run_checks":false},
        "disallowed_tools":[], "frequency":"daily", "hour":9, "minute":0,
        "weekday":null, "interval":null, "start_at":null, "enabled":true,
        "next_run":null, "revision":1, "deleted_at":null
    })).unwrap();
    state.schedule.schedules.insert(schedule.id.clone(), schedule);
    let mut live = LiveSnapshot::default();
    live.project_options.insert("New".into(), vec![crate::options::Spec {
        id: "run_checks".into(), label: "Run checks".into(),
        kind: crate::options::Kind::Toggle, values: vec![],
        default: crate::options::Choice::Toggle(true), builtin: false,
    }]);
    let view = render(&state, &live, Client::Tui, Lang::En,
        "schedule:schedule-1?project=New", "");
    let Item::Form(form) = &view.sections[0].items[0] else { panic!("schedule form") };
    let option = form.fields.iter().find(|field| field.id == "run_checks").unwrap();
    assert_eq!(option.value, serde_json::json!(true));
}

#[test]
fn task_message_keeps_task_context_and_node_attach_uses_live_short_id() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let task = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En,
        "task:t-1", "2026-09-30T10:00:00Z");
    let message = task.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        let Item::Actions(bar) = item else { return None };
        bar.actions.iter().find(|action| action.id.starts_with("msg:"))
    }).expect("task message action");
    assert_eq!(message.id, format!("msg:a-1@{}~t-1", state.revision));

    let mut live = LiveSnapshot::default();
    live.nodes.push(live::LiveNode {
        agent_id: "a-1".into(), short_id: Some("short-a-1".into()), cwd: None,
        name: "Node One".into(), project: None,
        state_key: None,
        state_label: None, summary: None, needs: None, background: vec![],
        background_badge: None, background_suspect: false, working: true,
        waiting: false, state_since: None, last_message: None, last_message_at: None,
    });
    let node = render(&state, &live, Client::Tui, Lang::En,
        "node:a-1", "2026-09-30T10:00:00Z");
    assert!(node.sections.iter().flat_map(|section| &section.items).any(|item| {
        matches!(item, Item::Row(row) if matches!(&row.link,
            Some(Link::Local { local: LocalLink::Attach { session } }) if session == "short-a-1"))
    }));
    let task = render(&state, &live, Client::Tui, Lang::En,
        "task:t-1", "2026-09-30T10:00:00Z");
    assert!(task.sections.iter().flat_map(|section| &section.items).any(|item| {
        matches!(item, Item::Row(row) if matches!(&row.link,
            Some(Link::Local { local: LocalLink::Attach { session } }) if session == "short-a-1"))
    }));
    let phone_task = render(&state, &live, Client::Ios, Lang::En,
        "task:t-1", "2026-09-30T10:00:00Z");
    assert!(!phone_task.sections.iter().flat_map(|section| &section.items).any(|item| {
        matches!(item, Item::Row(row) if matches!(&row.link, Some(Link::Local { .. })))
    }));
    let card = render(&state, &live, Client::Tui, Lang::En,
        "card:i-1", "2026-09-30T10:00:00Z");
    assert!(card.sections.iter().flat_map(|section| &section.items).any(|item| {
        matches!(item, Item::Card(card) if card.actions.iter().any(|action|
            matches!(&action.link,
                Some(Link::Local { local: LocalLink::Attach { session } }) if session == "short-a-1")))
    }));
}

#[test]
fn query_keys_decode_percent_encoding_and_reject_malformed_values() {
    let state = fixture();
    let live = LiveSnapshot::default();
    let query = render(
        &state,
        &live,
        Client::Mac,
        Lang::En,
        "search?q=Node+One",
        "2026-09-30T10:00:00Z",
    );
    assert_eq!(query.sections[0].items.len(), 1);
    assert_eq!(
        common::key_param("new_request?project=The%20App", "new_request", "project"),
        Ok(Some("The App".into()))
    );
    for key in [
        "search?q=%",
        "search?q=%FF",
        "search?q=x&refresh=1",
        "search?q=%00",
    ] {
        assert!(common::key_param(key, "search", "q").is_err(), "{key}");
    }
}

#[test]
fn home_ios_matches_golden() {
    let state = golden_fixture();
    let live = golden_live();
    let actual = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "home",
        "2026-09-30T10:00:00Z",
    );
    assert_golden(
        "home-ios.json",
        &actual,
        include_str!("../../../../fixtures/views/home-ios.json"),
    );
}

#[test]
fn home_tui_matches_golden() {
    let state = golden_fixture();
    let live = golden_live();
    let actual = render(
        &state,
        &live,
        Client::Tui,
        Lang::En,
        "home",
        "2026-09-30T10:00:00Z",
    );
    assert_golden(
        "home-tui.json",
        &actual,
        include_str!("../../../../fixtures/views/home-tui.json"),
    );
}

#[test]
fn card_variants_match_goldens() {
    let state = golden_fixture();
    let live = golden_live();
    for (filename, key, expected) in [
        (
            "card-question.json",
            "card:a98d034b-249e-4487-a89b-57de38c26372",
            include_str!("../../../../fixtures/views/card-question.json"),
        ),
        (
            "card-information.json",
            "card:f12d034b-249e-4487-a89b-57de38c26372",
            include_str!("../../../../fixtures/views/card-information.json"),
        ),
        (
            "card-pr.json",
            "card:github.com/example/repo#12",
            include_str!("../../../../fixtures/views/card-pr.json"),
        ),
        (
            "card-delivery.json",
            "card:hf-weekly-2026-09-30",
            include_str!("../../../../fixtures/views/card-delivery.json"),
        ),
    ] {
        let actual = render(
            &state,
            &live,
            Client::Ios,
            Lang::En,
            key,
            "2026-09-30T10:00:00Z",
        );
        assert_golden(filename, &actual, expected);
    }
}

#[test]
fn home_ios_es_matches_golden() {
    let state = golden_fixture();
    let live = golden_live();
    let actual = render(
        &state,
        &live,
        Client::Ios,
        Lang::Es,
        "home",
        "2026-09-30T10:00:00Z",
    );
    assert_golden(
        "home-ios-es.json",
        &actual,
        include_str!("../../../../fixtures/views/home-ios-es.json"),
    );
}

#[test]
#[ignore = "explicit fixture generation after Bazel authorization"]
fn generate_goldens() {
    let dir = std::env::var("ALONDRA_GOLDEN_OUT").expect("ALONDRA_GOLDEN_OUT required");
    let state = golden_fixture();
    let live = golden_live();
    let cases = [
        ("home-ios.json", "home", Client::Ios, Lang::En),
        ("home-tui.json", "home", Client::Tui, Lang::En),
        ("card-question.json", "card:a98d034b-249e-4487-a89b-57de38c26372", Client::Ios, Lang::En),
        ("card-information.json", "card:f12d034b-249e-4487-a89b-57de38c26372", Client::Ios, Lang::En),
        ("card-pr.json", "card:github.com/example/repo#12", Client::Ios, Lang::En),
        ("card-delivery.json", "card:hf-weekly-2026-09-30", Client::Ios, Lang::En),
        ("home-ios-es.json", "home", Client::Ios, Lang::Es),
        ("tasks-ios.json", "tasks", Client::Ios, Lang::En),
        ("fleet-ios.json", "fleet", Client::Ios, Lang::En),
        ("new-request-ios.json", "new_request", Client::Ios, Lang::En),
        ("node-ios.json", "node:d887a34b-249e-4487-a89b-57de38c26372", Client::Ios, Lang::En),
        ("task-ios.json", "task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Ios, Lang::En),
        ("request-ios.json", "request:r-9f2156b5a8894e53901f0af576dff6a2", Client::Ios, Lang::En),
        ("pull-request-ios.json", "pull_request:github.com/example/repo#12", Client::Ios, Lang::En),
        ("machine-ios.json", "machine", Client::Ios, Lang::En),
        ("schedules-ios.json", "schedules", Client::Ios, Lang::En),
        ("schedule-new-ios.json", "schedule:new", Client::Ios, Lang::En),
    ];
    for (filename, key, client, lang) in cases {
        let view = render(&state, &live, client, lang, key, "2026-09-30T10:00:00Z");
        std::fs::write(
            format!("{dir}/{filename}"),
            format!("{}\n", serde_json::to_string_pretty(&view).unwrap()),
        )
        .unwrap();
    }
}
#[test]
fn archived_interaction_detail_remains_addressable() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().archived = true;
    let live = LiveSnapshot::default();
    let view = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "card:i-1",
        "2026-09-30T10:00:00Z",
    );
    assert!(matches!(&view.sections[0].items[0], Item::Card(card) if card.id == "i-1"));
}
#[test]
fn node_quote_filters_raw_tool_calls() {
    let state = fixture();
    let mut live = LiveSnapshot::default();
    live.nodes.push(live::LiveNode {
        project: None,
        state_label: None,
        summary: None,
        needs: None,
        background: vec![],
        background_badge: None,
        background_suspect: false,
        agent_id: "a-1".into(),
        short_id: Some("a-1".into()),
        cwd: None,
        name: "Node One".into(),
        state_key: None,
        working: true,
        waiting: false,
        last_message: Some(" mcp__tools__call ".into()),
        state_since: None,
        last_message_at: None,
    });
    let hidden = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "card:i-1",
        "2026-09-30T10:00:00Z",
    );
    let Item::Card(card) = &hidden.sections[0].items[0] else {
        panic!("card")
    };
    assert!(card.quotes.is_empty());
    live.nodes[0].last_message = Some("The tests passed".into());
    let visible = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "card:i-1",
        "2026-09-30T10:00:00Z",
    );
    let Item::Card(card) = &visible.sections[0].items[0] else {
        panic!("card")
    };
    assert_eq!(card.quotes[0].text, "The tests passed");
}

#[test]
fn task_board_nests_a_dependent_under_its_parent() {
    let mut state = fixture();
    let mut child = state.tasks.get(&"t-1".into()).unwrap().clone();
    child.id = "t-2".into();
    child.title = "Follow-up".into();
    child.dependencies.insert("t-1".into());
    state.tasks.insert(child.id.clone(), child);
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "tasks", "2026-09-30T10:00:00Z");
    let active = view.sections.iter().find(|section| section.id == "active").unwrap();
    let Item::Row(request) = &active.items[0] else { panic!("request row") };
    assert_eq!(request.children[0].id, "t-1");
    assert_eq!(request.children[0].children[0].id, "t-2");
    assert_eq!(request.children[0].children[0].depth, 2);
}

#[test]
fn ambiguous_orchestrator_run_exposes_both_choices_with_confirmation() {
    let state = fixture();
    let live = LiveSnapshot { orchestrator: Some(live::LiveOrchestrator {
        working: false, summary: Some("An execution needs a decision".into()),
        queue_depth: 2, active_run_id: Some("run-1".into()), can_retry: true, can_cancel: false,
    }), ..Default::default() };
    let view = render(&state, &live, Client::Ios, Lang::En, "node:orchestrator", "2026-09-30T10:00:00Z");
    let actions: Vec<_> = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| {
        if let Item::Actions(bar) = item { Some(&bar.actions) } else { None }
    }).flatten().collect();
    assert!(actions.iter().any(|action| action.id.starts_with("orch_retry:") && action.confirm.is_some()));
    assert!(actions.iter().any(|action| action.id.starts_with("orch_ack:") && action.confirm.is_some()));
    assert!(!actions.iter().any(|action| action.id.starts_with("orch_stop:")));
}
#[test]
fn spanish_builder_text_differs() {
    let state = fixture();
    let live = LiveSnapshot::default();
    let en = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "home",
        "2026-09-30T10:00:00Z",
    );
    let es = render(
        &state,
        &live,
        Client::Ios,
        Lang::Es,
        "home",
        "2026-09-30T10:00:00Z",
    );
    assert_ne!(en.status.summary, es.status.summary);
    assert_ne!(en.sections[0].title, es.sections[0].title);
}

#[test]
fn more_context_action_allows_a_blank_note() {
    let state = fixture();
    let view = render(
        &state,
        &LiveSnapshot::default(),
        Client::Tui,
        Lang::En,
        "card:i-1",
        "",
    );
    let action = view
        .sections
        .iter()
        .flat_map(|section| &section.items)
        .find_map(|item| match item {
            Item::Card(card) => card.actions.iter().find(|action| {
                action_id::ActionId::parse(&action.id)
                    .is_ok_and(|id| id.verb == action_id::Verb::Ctx)
            }),
            _ => None,
        })
        .expect("the question must offer More Context");
    assert!(matches!(
        action.input,
        Input::Text {
            required: false,
            ..
        }
    ));
}

#[test]
fn stacked_pr_merge_names_upstream_number_and_keeps_canonical_link() {
    use crate::domain_v2::{DeliveryTarget, MergeMethod};
    let mut state = pr_fixture();
    state.pull_requests.get_mut(&"pr-1".into()).unwrap().base = "upstream-head".into();
    state
        .pull_requests
        .get_mut(&"pr-1".into())
        .unwrap()
        .allowed_methods = vec![MergeMethod::Squash];
    let mut upstream = state.pull_requests[&"pr-1".into()].clone();
    upstream.id = "github.com/bitomule/alondra#153".into();
    upstream.number = 153;
    upstream.head = "upstream-head".into();
    upstream.base = "main".into();
    state.pull_requests.insert(upstream.id.clone(), upstream);
    if let InteractionPayload::Delivery { targets, .. } =
        &mut state.interactions.get_mut(&"i-3".into()).unwrap().payload
    {
        targets.push(DeliveryTarget::PullRequest {
            pr_id: "pr-1".into(),
            head: "abcdef12".into(),
        });
    }
    let mut live = LiveSnapshot::default();
    live.pull_requests.push(LivePullRequest {
        id: "pr-1".into(),
        title: Some("Add the schedule editor".into()),
        body: None,
        head_ref: None,
        base_ref: None,
        checks_passed: None,
        conflicts: false,
        stacked_on: Some("github.com/bitomule/alondra#153".into()),
        url: None,
        ..Default::default()
    });
    let view = render(&state, &live, Client::Ios, Lang::En, "card:pr-1", "");
    let Item::Card(card) = &view.sections[0].items[0] else {
        panic!("PR card")
    };
    let merge = card
        .actions
        .iter()
        .find(|action| {
            action_id::ActionId::parse(&action.id).is_ok_and(|id| id.verb == action_id::Verb::Merge)
        })
        .expect("merge action");
    assert!(!merge.enabled);
    let confirm = merge.confirm.as_ref().expect("merge confirmation");
    assert_eq!(
        (confirm.title.as_str(), confirm.message.as_str(), confirm.button.as_str()),
        ("Merge PR #12?", "Add the schedule editor", "Merge")
    );
    let spanish = render(&state, &live, Client::Ios, Lang::Es, "card:pr-1", "");
    let Item::Card(spanish_card) = &spanish.sections[0].items[0] else { panic!("PR card") };
    let spanish_confirm = spanish_card.actions.iter().find_map(|action| action.confirm.as_ref()).expect("confirmation");
    assert_eq!(spanish_confirm.title, "¿Fusionar la PR #12?");
    assert_eq!(merge.disabled_reason.as_deref(), Some("Merge #153 first"));
    assert!(card.facts.as_ref().is_some_and(|facts| {
        facts.items.iter().any(|fact| {
            fact.value == "#153"
                && matches!(&fact.link, Some(Link::View { key })
            if key == "pull_request:github.com/bitomule/alondra#153")
        })
    }));
}

#[test]
fn built_in_request_options_keep_ids_and_translate_spanish_labels() {
    let state = fixture();
    let mut live = LiveSnapshot::default();
    live.options = crate::options::OptionsFile::default().catalog(None);
    let view = render(&state, &live, Client::Ios, Lang::Es, "new_request", "");
    let fields = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Form(form) = item { Some(&form.fields) } else { None }
    }).expect("request form");
    let pr = fields.iter().find(|field| field.id == "pr").expect("PR handling");
    assert_eq!(pr.label, "PRs");
    let FieldKind::Choice { items } = &pr.kind else { panic!("PR choices") };
    assert_eq!(items.iter().map(|item| (item.id.as_str(), item.label.as_str())).collect::<Vec<_>>(),
        [("review", "Revisar cada una"), ("auto_merge", "Merge automático")]);
    let effort = fields.iter().find(|field| field.id == "effort").expect("effort");
    assert_eq!(effort.label, "Esfuerzo");
    let FieldKind::Choice { items } = &effort.kind else { panic!("effort choices") };
    assert!(items.iter().any(|item| item.id == "light" && item.label == "Ligero"));
    assert_eq!(effort.value, serde_json::json!("auto"));
    let english = render(&state, &live, Client::Ios, Lang::En, "new_request", "");
    let english_fields = english.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Form(form) = item { Some(&form.fields) } else { None }
    }).unwrap();
    let english_pr = english_fields.iter().find(|field| field.id == "pr").unwrap();
    assert_eq!(english_pr.label, "PRs");
    let FieldKind::Choice { items } = &english_pr.kind else { panic!("PR choices") };
    assert_eq!(items.iter().map(|item| item.label.as_str()).collect::<Vec<_>>(),
        ["Review each one", "Auto-merge"]);
    let schedule = render(&state, &live, Client::Ios, Lang::Es, "schedule:new", "");
    let Item::Form(form) = &schedule.sections[0].items[0] else { panic!("schedule form") };
    for (id, expected) in [("repeat", "daily"), ("time", "09:00"), ("pr", "review"), ("effort", "auto")] {
        assert_eq!(form.fields.iter().find(|field| field.id == id).unwrap().value,
            serde_json::json!(expected), "{id}");
    }
}

#[test]
fn node_last_message_keeps_its_time_next_to_the_message() {
    let state = fixture();
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode {
        agent_id: "a-1".into(), short_id: None, cwd: None, name: "Node One".into(),
        project: None, state_key: None, state_label: None, summary: None, needs: None,
        background: vec![], background_badge: None, background_suspect: false,
        working: false, waiting: false,
        last_message: Some("Ready for review".into()),
        state_since: None,
        last_message_at: Some("2026-09-30T09:58:00Z".into()),
    });
    let view = render(&state, &live, Client::Ios, Lang::En, "node:a-1", "");
    let section = view.sections.iter().find(|section| section.id == "last_message").unwrap();
    assert!(section.items.iter().any(|item| matches!(item, Item::Markdown(markdown) if markdown.text == "Ready for review")));
    assert!(section.items.iter().any(|item| matches!(item,
        Item::Row(row) if matches!(&row.trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:58:00Z"))));
    let fleet = render(&state, &live, Client::Ios, Lang::En, "fleet", "");
    let node = fleet.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Row(row) = item && row.id == "a-1" { Some(row) } else { None }
    }).unwrap();
    assert!(node.children.iter().any(|child| child.id == "last_message"
        && child.title == "Ready for review"
        && matches!(&child.trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:58:00Z")));
    let search = render(&state, &live, Client::Ios, Lang::En, "search?q=Node", "");
    let Item::Row(result) = &search.sections[0].items[0] else { panic!("search result") };
    assert!(result.children.iter().any(|child| child.id == "last_message"));
}

#[test]
fn task_detail_says_which_question_it_is_waiting_on() {
    let state = fixture();
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "task:t-1", "2026-09-30T10:00:00Z");
    let now = view.sections.iter().find(|section| section.id == "now").expect("now section");
    let Item::Row(row) = &now.items[0] else { panic!("now row") };
    assert!(row.title.starts_with("question: "), "{}", row.title);
    assert!(matches!(&row.link, Some(Link::View { key }) if key.starts_with("card:")));
    let spanish = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "task:t-1", "2026-09-30T10:00:00Z");
    let Item::Row(row) = &spanish.sections.iter().find(|section| section.id == "now").unwrap().items[0] else { panic!("now row") };
    assert!(row.title.starts_with("pregunta: "), "{}", row.title);
}

#[test]
fn task_detail_has_no_now_line_while_the_node_just_works() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "task:t-1", "2026-09-30T10:00:00Z");
    assert!(view.sections.iter().all(|section| section.id != "now"));
}

#[test]
fn node_detail_names_the_occupancy_and_since_when() {
    let state = fixture();
    let mut live = LiveSnapshot::default();
    live.nodes.push(live::LiveNode {
        project: None,
        state_label: Some("working".into()),
        summary: None,
        needs: None,
        background: vec![],
        background_badge: None,
        background_suspect: false,
        agent_id: "a-1".into(),
        short_id: Some("a-1".into()),
        cwd: None,
        name: "Node One".into(),
        state_key: Some("free".into()),
        working: false,
        waiting: false,
        state_since: Some("2026-09-30T09:55:00Z".into()),
        last_message: None,
        last_message_at: None,
    });
    let view = render(&state, &live, Client::Ios, Lang::Es, "node:a-1", "2026-09-30T10:00:00Z");
    let Item::Facts(facts) = &view.sections.iter().find(|section| section.id == "header").unwrap().items[0] else {
        panic!("header facts")
    };
    assert_eq!(facts.items[0].value, "Libre");
    let since = view.sections.iter().find(|section| section.id == "since").expect("since section");
    let Item::Row(row) = &since.items[0] else { panic!("since row") };
    assert!(matches!(&row.trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:55:00Z"));
}

#[test]
fn node_detail_without_live_data_keeps_the_connection_state() {
    let state = fixture();
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "node:a-1", "2026-09-30T10:00:00Z");
    let Item::Facts(facts) = &view.sections.iter().find(|section| section.id == "header").unwrap().items[0] else {
        panic!("header facts")
    };
    assert_eq!(facts.items[0].value, "Connected");
    assert!(view.sections.iter().all(|section| section.id != "since"));
}

#[test]
fn information_reply_accepts_block_comments() {
    let state = fixture();
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-2", "2026-09-30T10:00:00Z");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("card") };
    let reply = card.actions.iter().find(|action| action.id.starts_with("reply:")).expect("reply action");
    assert!(matches!(&reply.comments, Some(CommentSpec { kind: CommentKind::Blocks })));
}

#[test]
fn effort_labels_are_the_same_on_every_client_and_spanish_everywhere() {
    let effort = || crate::options::Spec {
        id: "effort".into(), label: "Effort".into(), kind: crate::options::Kind::Choice,
        values: ["auto", "light", "normal", "hard"].iter()
            .map(|id| crate::options::SpecValue { id: (*id).into(), label: (*id).into(), description: None }).collect(),
        default: crate::options::Choice::Value("auto".into()), builtin: true,
    };
    let labels = |client: Client, lang: Lang| {
        let mut live = LiveSnapshot::default();
        live.project_options.insert("App".into(), vec![effort()]);
        let view = render(&fixture(), &live, client, lang, "schedule:new?project=App", "");
        let form = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
            if let Item::Form(form) = item { Some(form) } else { None }
        }).unwrap();
        let field = form.fields.iter().find(|field| field.id == "effort").unwrap();
        let FieldKind::Choice { items } = &field.kind else { panic!("effort is a choice") };
        (field.label.clone(), items.iter().map(|item| item.label.clone()).collect::<Vec<_>>())
    };
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        assert_eq!(labels(client, Lang::En),
            ("Effort".into(), vec!["Auto".into(), "Light".into(), "Normal".into(), "Hard".into()]));
        assert_eq!(labels(client, Lang::Es),
            ("Esfuerzo".into(), vec!["Automático".into(), "Ligero".into(), "Normal".into(), "A fondo".into()]));
    }
}

#[test]
fn focus_puts_the_card_first_with_its_detail() {
    let state = fixture();
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En,
        "home?focus=i-3", "2026-09-30T10:00:00Z");
    let waiting: Vec<&str> = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| {
        if let Item::Row(row) = item { Some(row.id.as_str()) } else { None }
    }).collect();
    assert_eq!(waiting.first(), Some(&"i-3"));
    let detail = view.sections.iter().find(|section| section.id == "detail").unwrap();
    let Item::Card(card) = &detail.items[0] else { panic!("detail card") };
    assert_eq!(card.id, "i-3");
}

#[test]
fn a_pasted_issue_link_becomes_a_choice() {
    let state = fixture();
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En,
        "issue_picker?q=https%3A%2F%2Fgithub.com%2Fowner%2Frepo%2Fissues%2F154", "");
    let Item::Row(row) = &view.sections[0].items[0] else { panic!("pasted issue option") };
    assert_eq!(row.id, "owner/repo#154");
    assert!(row.title.contains("Use https://github.com/owner/repo/issues/154"));
}

fn new_request_form(view: &View) -> &Form {
    view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Form(form) = item { Some(form) } else { None }
    }).unwrap()
}

#[test]
fn changing_project_reloads_its_options() {
    let mut live = LiveSnapshot::default();
    let option = |id: &str| crate::options::Spec {
        id: id.into(), label: id.into(), kind: crate::options::Kind::Toggle, values: vec![],
        default: crate::options::Choice::Toggle(true), builtin: false,
    };
    live.options = vec![option("default_option")];
    live.project_options.insert("App".into(), vec![option("app_option")]);
    let state = fixture();
    let initial = render(&state, &live, Client::Ios, Lang::En, "new_request", "");
    let form = new_request_form(&initial);
    for field in &form.fields {
        assert_eq!(field.reload_on_change, field.id == "project", "{}", field.id);
    }
    assert!(form.fields.iter().any(|field| field.id == "default_option"));
    let reloaded = render(&state, &live, Client::Ios, Lang::En, "new_request?project=App", "");
    let form = new_request_form(&reloaded);
    assert!(form.fields.iter().any(|field| field.id == "app_option"));
    assert!(!form.fields.iter().any(|field| field.id == "default_option"));
}

#[test]
fn the_sent_history_strips_the_prefix() {
    let mut state = fixture();
    state.requests.insert("r-2".into(), Request::new("r-2".into(), "[Alondra] Fix the login bug\nwith details".into()));
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "new_request", "");
    let sent = view.sections.iter().find(|section| section.id == "sent").unwrap();
    let titles: Vec<&str> = sent.items.iter().filter_map(|item| {
        if let Item::Row(row) = item { Some(row.title.as_str()) } else { None }
    }).collect();
    assert!(titles.contains(&"Fix the login bug"), "{titles:?}");
    assert!(!titles.iter().any(|title| title.contains("[Alondra]")));
}

#[test]
fn more_loads_the_next_finished_page() {
    let mut state = DomainState::new("test-epoch");
    for index in 0..25 {
        let id = format!("r-{index:02}");
        let mut request = Request::new(id.clone().into(), format!("Request {index:02}"));
        request.closed = true;
        state.requests.insert(id.into(), request);
    }
    let live = LiveSnapshot::default();
    let first = render(&state, &live, Client::Ios, Lang::En, "tasks", "");
    let finished = first.sections.iter().find(|section| section.id == "finished").unwrap();
    assert_eq!(finished.items.len(), 20);
    let more = finished.more.as_ref().unwrap();
    assert_eq!((more.key.as_str(), more.remaining), ("tasks?section=finished&after=20", 5));
    let second = render(&state, &live, Client::Ios, Lang::En, &more.key, "");
    let finished = second.sections.iter().find(|section| section.id == "finished").unwrap();
    let ids: Vec<&str> = finished.items.iter().filter_map(|item| {
        if let Item::Row(row) = item { Some(row.id.as_str()) } else { None }
    }).collect();
    assert_eq!(ids, ["r-20", "r-21", "r-22", "r-23", "r-24"]);
    assert!(finished.more.is_none());
}

fn string_leaves(value: &serde_json::Value, path: &str, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::String(text) => out.push((path.to_owned(), text.clone())),
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                string_leaves(item, &format!("{path}/{index}"), out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                string_leaves(item, &format!("{path}/{key}"), out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_builder_string_has_spanish() {
    const NAMES_AND_DATA: &[&str] = &[
        "Alondra · iOS", "Demo Garden", "PRs", "Polish the Alondra beta", "Restore the native review experience", "PR #12", "1 · PR #12", "example/repo #12", "Base",
        "Memory", "Disk available", "6.1 GB of 16 GB", "120 GB", "Waiting for you",
        "Keep the warmer copy", "Ship the concise copy", "Normal",
        "A decision on the welcome copy", "Reviewing the native beta experience",
        "The simulator pass completed successfully.", "Started", "Waiting for a decision",
        "Reading the localization catalog",
    ];
    const FIXTURE_TEXT_PREFIXES: &[&str] = &[
        "## Beta candidate ready", "All required checks passed", "Prepare the next Alondra beta",
        "The iPhone simulator pass", "The new empty state is ready", "The pull request is ready",
        "Add the compact schedule editor", "1 · Add the compact schedule editor",
        "Replaces the long schedule form", "feature/compact-schedule-editor",
    ];
    let state = golden_fixture();
    let live = golden_live();
    let keys = [
        ("home", Client::Ios), ("home", Client::Tui),
        ("card:a98d034b-249e-4487-a89b-57de38c26372", Client::Ios),
        ("card:f12d034b-249e-4487-a89b-57de38c26372", Client::Ios),
        ("card:github.com/example/repo#12", Client::Ios),
        ("card:hf-weekly-2026-09-30", Client::Ios),
        ("tasks", Client::Ios), ("fleet", Client::Ios), ("new_request", Client::Ios),
        ("node:d887a34b-249e-4487-a89b-57de38c26372", Client::Ios),
        ("task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Ios),
        ("request:r-9f2156b5a8894e53901f0af576dff6a2", Client::Ios),
        ("pull_request:github.com/example/repo#12", Client::Ios),
        ("machine", Client::Ios), ("schedules", Client::Ios), ("schedule:new", Client::Ios),
        ("sidebar", Client::Mac),
    ];
    let mut untranslated = BTreeMap::new();
    for (key, client) in keys {
        let leaves = |lang| {
            let view = render(&state, &live, client, lang, key, "2026-09-30T10:00:00Z");
            let mut out = vec![];
            string_leaves(&serde_json::to_value(view).unwrap(), "", &mut out);
            out
        };
        let (english, spanish) = (leaves(Lang::En), leaves(Lang::Es));
        assert_eq!(english.len(), spanish.len(), "{key}");
        for ((path, en), (_, es)) in english.iter().zip(&spanish) {
            let last = path.rsplit('/').next().unwrap();
            let is_text = matches!(last, "title" | "label" | "text" | "subtitle" | "summary" | "help"
                | "message" | "button" | "reason" | "disabled_reason" | "prompt");
            if is_text && en == es && en.chars().any(char::is_alphabetic) && !NAMES_AND_DATA.contains(&en.as_str())
                && !FIXTURE_TEXT_PREFIXES.iter().any(|prefix| en.starts_with(prefix)) {
                untranslated.insert(en.clone(), format!("{key} {path}"));
            }
        }
    }
    assert!(untranslated.is_empty(), "strings identical in Spanish: {untranslated:#?}");
}

fn scheduled_run_fixture(with_run: bool) -> DomainState {
    let mut state = fixture();
    let raw = "scheduled/0789f185-run-7e2035e8e1d7c5e3f9431b9087d457e3";
    state.agents.get_mut(&"a-1".into()).unwrap().name = raw.into();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    if with_run {
        let run: crate::domain_v2::ScheduleRun = serde_json::from_value(serde_json::json!({
            "id":"run-7e2035e8e1d7c5e3f9431b9087d457e3", "schedule_id":"0789f185-0000",
            "origin":"due", "due_at":100, "claimed_at":105, "claim_revision":2,
            "finished_at":110, "state":"sent", "request_id":"r-1", "task_id":"t-1",
            "error":null,
            "definition_snapshot":{"title":"Morning review","text":"Review","project":null,
                "options":{},"disallowed_tools":[]},
            "launch":{"intent":{"name":raw,"cwd":"/tmp","prompt":"Review","options_json":"{}"},
                "state":"handed","claimed_at":105,"finished_at":110,"session_id":"abc",
                "agent_id":"a-1","error":null}
        })).unwrap();
        state.schedule.runs.insert(run.id.clone(), run);
    }
    state
}

fn card_context_texts(state: &DomainState) -> Vec<String> {
    let view = render(state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        Item::Card(card) => Some(card.context.iter().map(|line| line.text.clone()).collect::<Vec<_>>()),
        _ => None,
    }).flatten().collect()
}

#[test]
fn scheduled_run_agent_shows_the_schedule_title_never_the_raw_run_name() {
    let texts = card_context_texts(&scheduled_run_fixture(true));
    assert!(texts.iter().any(|text| text == "Morning review"), "{texts:?}");
    assert!(!texts.iter().any(|text| text.contains("scheduled/")), "{texts:?}");
}

#[test]
fn scheduled_run_agent_without_a_recorded_run_falls_back_to_the_task_title() {
    let texts = card_context_texts(&scheduled_run_fixture(false));
    assert!(texts.iter().any(|text| text == "Build a feature"), "{texts:?}");
    assert!(!texts.iter().any(|text| text.contains("scheduled/")), "{texts:?}");
}

fn card_of(view: &View) -> &Card {
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("card") };
    card
}

fn header_facts(view: &View) -> &[Fact] {
    let Item::Facts(facts) = &view.sections.iter().find(|section| section.id == "header").unwrap().items[0] else {
        panic!("header facts")
    };
    &facts.items
}

#[test]
fn pull_request_card_carries_title_checks_branch_and_change_size() {
    let card_key = "card:github.com/example/repo#12";
    let state = golden_fixture();
    let view = render(&state, &golden_live(), Client::Ios, Lang::En, card_key, "");
    let card = card_of(&view);
    assert_eq!(card.title, "Add the compact schedule editor");
    assert!(card.body.as_ref().is_some_and(|body| body.text.starts_with("Replaces the long schedule form")));
    let facts = &card.facts.as_ref().unwrap().items;
    let value = |label: &str| facts.iter().find(|fact| fact.label == label).map(|fact| fact.value.as_str());
    assert_eq!(value("Repository"), Some("example/repo #12"));
    assert_eq!(value("Head"), Some("feature/compact-schedule-editor"));
    assert_eq!(value("Changes"), Some("+120 −30 in 4 files"));
    let checks = card.checks.as_ref().unwrap();
    assert_eq!(
        checks.items.iter().map(|check| (check.name.as_str(), check.state)).collect::<Vec<_>>(),
        [("Build", CheckState::Pass), ("Unit tests", CheckState::Pass), ("Lint", CheckState::Pending)]
    );
    let spanish = render(&state, &golden_live(), Client::Ios, Lang::Es, card_key, "");
    let spanish_facts = &card_of(&spanish).facts.as_ref().unwrap().items;
    assert!(spanish_facts.iter().any(|fact| fact.label == "Cambios" && fact.value == "+120 −30 en 4 archivos"));
    let mut single = golden_live();
    single.pull_requests[0].changed_files = 1;
    let view = render(&state, &single, Client::Ios, Lang::En, card_key, "");
    assert!(card_of(&view).facts.as_ref().unwrap().items.iter()
        .any(|fact| fact.label == "Changes" && fact.value == "+120 −30 in 1 file"));
}

#[test]
fn pull_request_without_github_details_keeps_the_aggregate_check_and_domain_title() {
    let state = golden_fixture();
    let mut live = golden_live();
    live.pull_requests[0] = LivePullRequest {
        id: "github.com/example/repo#12".into(),
        checks_passed: Some(false),
        ..Default::default()
    };
    let view = render(&state, &live, Client::Ios, Lang::En, "card:github.com/example/repo#12", "");
    let card = card_of(&view);
    assert_eq!(card.title, "example/repo #12");
    let checks = card.checks.as_ref().unwrap();
    assert_eq!(checks.items.len(), 1);
    assert_eq!(checks.items[0].state, CheckState::Fail);
    let facts = &card.facts.as_ref().unwrap().items;
    assert!(facts.iter().all(|fact| fact.label != "Changes" && fact.label != "Head"));
}

#[test]
fn pull_request_detail_shows_the_change_size() {
    let view = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En,
        "pull_request:github.com/example/repo#12", "");
    let facts = view.sections.iter().find(|section| section.id == "facts").unwrap();
    let Item::Facts(facts) = &facts.items[0] else { panic!("facts") };
    assert!(facts.items.iter().any(|fact| fact.label == "Changes" && fact.value == "+120 −30 in 4 files"));
}

#[test]
fn card_node_line_carries_the_node_state_next_to_the_project() {
    let view = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En,
        "card:a98d034b-249e-4487-a89b-57de38c26372", "");
    let node = card_of(&view).context.iter().find(|line| line.icon == Icon::Node).unwrap();
    let state = node.state.as_ref().expect("node state");
    assert_eq!((state.text.as_str(), state.tone), ("Waiting for you", Tone::Accent));
    assert_eq!(node.chip.as_ref().map(|chip| chip.text.as_str()), Some("Demo Garden"));
    let bare = render(&golden_fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En,
        "card:a98d034b-249e-4487-a89b-57de38c26372", "");
    assert!(card_of(&bare).context.iter().all(|line| line.state.is_none()));
}

#[test]
fn action_request_body_is_a_code_block_that_survives_backticks() {
    let mut state = fixture();
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = InteractionPayload::ActionRequest { options: vec![], answer: None };
    interaction.body = "git commit -m \"```fix```\"\n".into();
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    let body = card_of(&view).body.as_ref().expect("body");
    assert_eq!(body.text, "````\ngit commit -m \"```fix```\"\n````");
    let question = render(&fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert_eq!(card_of(&question).body.as_ref().unwrap().text, "Body 0");
}

#[test]
fn node_detail_names_its_folder_and_what_it_is_doing_right_now() {
    let view = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En,
        "node:d887a34b-249e-4487-a89b-57de38c26372", "");
    assert!(header_facts(&view).iter().any(|fact| fact.label == "Folder" && fact.value == "/demo/Demo Garden/ios"));
    let now = view.sections.iter().find(|section| section.id == "now").expect("right now section");
    assert_eq!(now.title.as_deref(), Some("Right now"));
    let Item::Row(row) = &now.items[0] else { panic!("thread row") };
    assert_eq!(row.title, "Reading the localization catalog");
    assert_eq!(row.subtitle.as_deref(), Some("Task Check the Spanish strings"));
    let idle = render(&golden_fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En,
        "node:d887a34b-249e-4487-a89b-57de38c26372", "");
    assert!(idle.sections.iter().all(|section| section.id != "now"));
    assert!(header_facts(&idle).iter().all(|fact| fact.label != "Folder"));
}

#[test]
fn task_detail_says_since_when_it_has_been_in_its_state() {
    let id = "task:t-651c38484cd2f34e74d5107ba05f7f15";
    let view = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En, id, "");
    let since = view.sections.iter().find(|section| section.id == "since").expect("since section");
    let Item::Row(row) = &since.items[0] else { panic!("since row") };
    assert!(matches!(&row.trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:30:00Z"));
    let bare = render(&golden_fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En, id, "");
    assert!(bare.sections.iter().all(|section| section.id != "since"));
}

#[test]
fn request_option_values_keep_their_description_in_both_languages() {
    let mut live = LiveSnapshot::default();
    live.options = crate::options::OptionsFile::default().catalog(None);
    let descriptions = |lang| {
        let view = render(&fixture(), &live, Client::Ios, lang, "new_request", "");
        let Item::Form(form) = view.sections.iter().flat_map(|section| &section.items)
            .find(|item| matches!(item, Item::Form(_))).unwrap() else { panic!("form") };
        ["pr", "effort"].map(|id| {
            let FieldKind::Choice { items } = &form.fields.iter().find(|field| field.id == id).unwrap().kind
                else { panic!("choice") };
            items.iter().map(|item| item.description.clone()).collect::<Vec<_>>()
        })
    };
    let [pr, effort] = descriptions(Lang::En);
    assert_eq!(pr[1].as_deref(), Some("The node merges its own PR once its checks pass"));
    assert!(effort.iter().all(Option::is_some));
    let [pr, effort] = descriptions(Lang::Es);
    assert_eq!(pr[1].as_deref(), Some("El nodo fusiona su propia PR cuando pasan las comprobaciones"));
    assert_eq!(effort[2].as_deref(), Some("El modelo y el esfuerzo por defecto de Claude"));
}

#[test]
fn fleet_rows_offer_close_node_with_a_confirmation() {
    let view = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En, "fleet", "");
    let row = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
        Item::Row(row) if row.id == "d887a34b-249e-4487-a89b-57de38c26372" => Some(row),
        _ => None,
    }).expect("node row");
    let close = row.actions.iter().find(|action| action.label == "Close Node").expect("close action");
    assert_eq!(close.style, Style::Destructive);
    assert!(close.id.starts_with("close:"));
    assert_eq!(close.confirm.as_ref().map(|confirm| confirm.message.as_str()), Some("Close this node?"));
    let detail = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En,
        "node:d887a34b-249e-4487-a89b-57de38c26372", "");
    let Item::Actions(bar) = &detail.sections.iter().find(|section| section.id == "actions").unwrap().items[0]
        else { panic!("actions") };
    assert_eq!(bar.actions.iter().find(|action| action.label == "Close Node").map(|action| &action.id), Some(&close.id));
}

fn held_fixture(hold: crate::domain_v2::Hold, body: &str) -> DomainState {
    let mut state = fixture();
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = InteractionPayload::Question { options: hold.options(), answer: None };
    interaction.body = body.into();
    state.holds.insert("i-1".into(), hold);
    state
}

fn answer_action(card: &Card) -> &Action {
    card.actions.iter().find(|action| action.id.starts_with("ans:i-1@")).expect("answer action")
}

#[test]
fn a_held_permission_card_shows_what_it_would_run_with_allow_and_deny() {
    use crate::domain_v2::Hold;
    let state = held_fixture(Hold::Permission, "Bash: clean the build\n\n```\nrm -rf build\n```");
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!((card.icon, card.eyebrow.as_ref().unwrap().text.as_str()), (Icon::Warning, "Permiso"));
    assert_eq!(card.title, "Bash: clean the build");
    assert_eq!(card.body.as_ref().unwrap().text, "```\nrm -rf build\n```");
    let options = &answer_action(card).options;
    assert_eq!(options.iter().map(|o| (o.id.as_str(), o.label.as_str(), o.key.as_deref())).collect::<Vec<_>>(),
        [("allow", "Permitir", Some("1")), ("deny", "Denegar", Some("2"))]);
    assert_eq!(options[1].style, Style::Destructive);
    assert!(matches!(&options[1].input, Input::Text { placeholder, required: false, .. } if placeholder == "Escribe el motivo"));
    assert!(card.actions.iter().all(|action| !action.id.starts_with("ctx:")), "a held card is answered, not discussed");
    assert_eq!(view.status.summary.split(" · ").next(), Some("1 pregunta"));
    let pushed = push_actions(&state, "i-1", Lang::En).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.option.as_deref(), a.label.as_str(), a.text, a.open)).collect::<Vec<_>>(),
        [(Some("allow"), "Allow", false, false), (Some("deny"), "Deny", false, false)]);
    assert!(pushed.iter().all(|action| action.id == answer_action(card).id));
}

#[test]
fn a_held_plan_card_shows_the_plan_with_approve_and_request_changes_and_a_review_button() {
    use crate::domain_v2::Hold;
    let plan = "# Ship the parser\n\n1. Split the lexer\n2. Add tests";
    let state = held_fixture(Hold::Plan, plan);
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!((card.icon, card.eyebrow.as_ref().unwrap().text.as_str()), (Icon::Document, "Plan"));
    assert_eq!(card.title, "Ship the parser");
    assert_eq!(card.body.as_ref().unwrap().text, plan);
    let options = &answer_action(card).options;
    assert_eq!(options.iter().map(|o| (o.id.as_str(), o.label.as_str(), o.key.as_deref())).collect::<Vec<_>>(),
        [("approve", "Approve", Some("a")), ("request_changes", "Request changes", Some("p"))]);
    let pushed = push_actions(&state, "i-1", Lang::Es).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.option.as_deref(), a.label.as_str(), a.open)).collect::<Vec<_>>(),
        [(Some("approve"), "Aprobar", false), (None, "Revisar", true)]);
    assert_eq!(pushed[1].id, "open:i-1");
    let ios = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert!(answer_action(card_of(&ios)).options.iter().all(|option| option.key.is_none()));
}

#[test]
fn a_held_plan_is_reviewed_with_block_comments_request_changes_and_approve_as_8ccd131() {
    use crate::domain_v2::Hold;
    let state = held_fixture(Hold::Plan, "# Ship the parser\n\n1. Split the lexer");
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    let review: Vec<_> = card.actions.iter()
        .filter(|action| action.id.starts_with("changes:i-1@") || action.id.starts_with("accept:i-1@"))
        .map(|action| (action.label.as_str(), action.style, action.comments.is_some(), action.stamp.as_deref()))
        .collect();
    assert_eq!(review, [("Pedir cambios", Style::Secondary, true, Some("Cambios solicitados")),
        ("Aprobar", Style::Primary, false, Some("Aprobado"))]);
    let pushed = push_actions(&state, "i-1", Lang::En).unwrap();
    assert_eq!(pushed.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(), ["Approve", "Review"],
        "the lock screen keeps Approve and Review");
    let permission = held_fixture(Hold::Permission, "Bash: clean the build");
    let permission_view = render(&permission, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    let card = card_of(&permission_view);
    assert!(card.actions.iter().all(|action| !action.id.starts_with("changes:") && !action.id.starts_with("accept:")));
}

#[test]
fn a_held_ask_user_question_card_shows_the_question_and_its_options_as_8ccd131() {
    use crate::domain_v2::Hold;
    let mut state = held_fixture(Hold::Question, "Which database?\n\n- Also migrate the old rows?");
    let options = ["Postgres", "SQLite"].iter().enumerate().map(|(index, label)| QuestionOption {
        id: format!("o{index}"), label: (*label).into(), description: None, inputs: vec![] }).collect();
    state.interactions.get_mut(&"i-1".into()).unwrap().payload = InteractionPayload::Question { options, answer: None };
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!((card.icon, card.eyebrow.as_ref().unwrap().text.as_str()), (Icon::Question, "Pregunta"));
    assert_eq!(card.title, "Which database?");
    assert_eq!(card.body.as_ref().unwrap().text, "- Also migrate the old rows?");
    assert_eq!(answer_action(card).options.iter().map(|o| (o.id.as_str(), o.label.as_str(), o.key.as_deref())).collect::<Vec<_>>(),
        [("o0", "Postgres", Some("1")), ("o1", "SQLite", Some("2"))]);
    assert!(card.actions.iter().all(|action| !action.id.starts_with("ctx:")), "the hook waits for an answer");
    let pushed = push_actions(&state, "i-1", Lang::En).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.option.as_deref(), a.label.as_str(), a.text)).collect::<Vec<_>>(),
        [(Some("o0"), "Postgres", false), (Some("o1"), "SQLite", false)]);
}

#[test]
fn a_held_elicitation_card_accepts_with_the_form_values_or_declines() {
    use crate::domain_v2::Hold;
    let mut state = held_fixture(Hold::Elicitation, "Which repository?\n\nMCP server “github” requests your input.");
    if let InteractionPayload::Question { options, .. } = &mut state.interactions.get_mut(&"i-1".into()).unwrap().payload {
        options[0].description = Some("Write the Repository".into());
    }
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!(card.title, "Which repository?");
    let options = &answer_action(card).options;
    assert_eq!(options.iter().map(|o| (o.id.as_str(), o.label.as_str(), o.key.as_deref())).collect::<Vec<_>>(),
        [("accept", "Aceptar", Some("1")), ("decline", "Rechazar", Some("2"))]);
    assert!(matches!(&options[0].input, Input::Text { placeholder, .. } if placeholder == "Write the Repository"));
    let pushed = push_actions(&state, "i-1", Lang::En).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.option.as_deref(), a.label.as_str(), a.text)).collect::<Vec<_>>(),
        [(Some("accept"), "Accept", true), (Some("decline"), "Decline", false)]);
}

#[test]
fn status_names_the_node_count_fleet() {
    for (lang, label) in [(Lang::En, "Fleet"), (Lang::Es, "Flota")] {
        let view = render(&fixture(), &LiveSnapshot::default(), Client::Tui, lang, "home", "");
        let count = view.status.counts.iter().find(|count| count.id == "agents").unwrap();
        assert_eq!(count.label, label);
        assert!(view.status.counts.iter().all(|count| !count.label.to_lowercase().contains("agent")));
    }
}
