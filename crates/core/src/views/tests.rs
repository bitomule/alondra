use super::*;
use crate::domain_v2::{
    Agent, Author, DomainState, Interaction, InteractionPayload, InteractionSubject, PrState,
    PullRequest, Recipient, Request, Scope, Task, TaskState,
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
fn huge_home_sends_only_ordered_ids() {
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
    assert_eq!(queue.card_ids.len(), 2_003);
    assert!(queue.items.is_empty());
    assert_eq!(queue.card_ids.first().unwrap(), "i-1");
    assert_eq!(queue.card_ids.last().unwrap(), "q-1999");
    assert!(serde_json::to_vec(&view).unwrap().len() < 160 * 1024);
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
    assert!(push_actions(&state, id, Lang::En).iter().any(|action|
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
        agent_id: "a-1".into(), short_id: Some("short-a-1".into()),
        name: "Node One".into(), project: None, state_key: None,
        state_label: None, summary: None, needs: None, background: vec![],
        background_badge: None, background_suspect: false, working: true,
        waiting: false, last_message: None, last_message_at: None,
    });
    let node = render(&state, &live, Client::Tui, Lang::En,
        "node:a-1", "2026-09-30T10:00:00Z");
    assert!(node.sections.iter().flat_map(|section| &section.items).any(|item| {
        matches!(item, Item::Row(row) if matches!(&row.link,
            Some(Link::Local { local: LocalLink::Attach { session } }) if session == "short-a-1"))
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
    let state = pr_fixture();
    let live = LiveSnapshot::default();
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
    let state = pr_fixture();
    let live = LiveSnapshot::default();
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
    let state = pr_fixture();
    let live = LiveSnapshot::default();
    for (filename, key, expected) in [
        (
            "card-question.json",
            "card:i-1",
            include_str!("../../../../fixtures/views/card-question.json"),
        ),
        (
            "card-pr.json",
            "card:pr-1",
            include_str!("../../../../fixtures/views/card-pr.json"),
        ),
        (
            "card-delivery.json",
            "card:i-3",
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
    let state = pr_fixture();
    let live = LiveSnapshot::default();
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
    let state = pr_fixture();
    let live = LiveSnapshot::default();
    let cases = [
        ("home-ios.json", "home", Client::Ios, Lang::En),
        ("home-tui.json", "home", Client::Tui, Lang::En),
        ("card-question.json", "card:i-1", Client::Ios, Lang::En),
        ("card-pr.json", "card:pr-1", Client::Ios, Lang::En),
        ("card-delivery.json", "card:i-3", Client::Ios, Lang::En),
        ("home-ios-es.json", "home", Client::Ios, Lang::Es),
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
        state_key: None,
        state_label: None,
        summary: None,
        needs: None,
        background: vec![],
        background_badge: None,
        background_suspect: false,
        agent_id: "a-1".into(),
        short_id: Some("a-1".into()),
        name: "Node One".into(),
        working: true,
        waiting: false,
        last_message: Some(" mcp__tools__call ".into()),
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
        checks_passed: None,
        conflicts: false,
        stacked_on: Some("github.com/bitomule/alondra#153".into()),
        url: None,
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
    assert_eq!(merge.disabled_reason.as_deref(), Some("Merge #153 first"));
    assert!(card.facts.as_ref().is_some_and(|facts| {
        facts.items.iter().any(|fact| {
            fact.value == "#153"
                && matches!(&fact.link, Some(Link::View { key })
            if key == "pull_request:github.com/bitomule/alondra#153")
        })
    }));
}
