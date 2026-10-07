use super::*;
use crate::domain::{
    Agent, Author, Document, DocumentRevision, DomainState, Interaction, InteractionPayload,
    InteractionSubject, MergeAttemptState, MergeAuthorization, MergeMethod, PrState, PullRequest, QuestionOption, Recipient, Request, Scope, Task,
    TaskState, ModelChooser, TaskModel,
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
            model: None,
            delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: Default::default(),
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
                withdrawn: false, card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"), clarification: None,
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
            withdrawn: false, card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"), clarification: None,
        },
    );
    state
}
/// The placeholder card these fixtures were written with becomes the card its payload would
/// carry, so a question reads as a decision and a final delivery as a review.
fn with_cards(domain: &DomainState) -> DomainState {
    use crate::domain::{CardKind, CardSpec, Hold};
    let mut state = domain.clone();
    let holds = state.holds.clone();
    for interaction in state.interactions.values_mut() {
        if interaction.card.need != "Read the update" {
            continue;
        }
        let body = interaction.body.clone();
        interaction.card = match &interaction.payload {
            InteractionPayload::Information { .. } => CardSpec::derived(CardKind::Know, &body),
            InteractionPayload::Question { options, .. } => match holds.get(&interaction.id) {
                Some(Hold::Permission) => CardSpec::derived(CardKind::Authorize, &body),
                Some(Hold::Plan) => CardSpec::derived(CardKind::Review, &body),
                Some(Hold::Elicitation) => CardSpec::derived(CardKind::Provide, &body),
                _ => CardSpec::question(&body, options),
            },
            InteractionPayload::ActionRequest { .. } => CardSpec::derived(CardKind::Do, &body),
            InteractionPayload::Delivery { targets, is_final, .. } => CardSpec::delivery(&body, targets, *is_final),
        };
    }
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
            domain: &with_cards(domain),
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
    assert_eq!(
        view.sections[0].card_ids,
        ["i-1", "i-4", "i-3", "i-2"],
        "questions that hold a node first, then what to review, then notices"
    );
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
                withdrawn: false, card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"), clarification: None,
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
    assert_eq!(queue.card_ids.last().unwrap(), "i-2", "the notice waits behind every question");
    let bytes = serde_json::to_vec(&view).unwrap();
    assert!(bytes.len() < 744 * 1024, "home used {} bytes", bytes.len());
    assert_eq!(queue.card_ids.len(), 2_004, "every remaining card is reported by id");
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
fn orchestrator_questions_to_the_user_wait_in_home() {
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
    assert!(view.sections[0].card_ids.contains(&"i-4".to_owned()));
    assert_eq!(view.status.badge, view.sections[0].card_ids.len() as u32);
    assert_eq!(
        view.status.summary,
        "2 questions · 2 to read · 0 nodes working"
    );
}
#[test]
fn icon_badge_counts_what_waits_in_home() {
    let state = fixture();
    let home = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "home", "2026-09-30T10:00:00Z");
    assert_eq!(super::inbox_badge(&state), 4, "the orchestrator's question to the user is the user's");
    assert_eq!(super::inbox_badge(&state), home.status.badge);
    assert_eq!(super::inbox_badge(&state), home.sections[0].card_ids.len() as u32);
    assert_eq!(super::inbox_badge(&DomainState::new("empty")), 0);
}
#[test]
fn a_question_about_a_pr_is_its_own_card_and_no_card_holds_another() {
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
            withdrawn: false, card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"), clarification: None,
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
    assert!(view.sections[0].card_ids.contains(&"i-5".to_owned()), "the question is a card of its own");
    assert!(!view.sections[0].card_ids.contains(&"pr-1".to_owned()), "no card stands for the PR any more");
    assert!(view.sections[0].items.is_empty());
    let detail = render(
        &state,
        &live,
        Client::Ios,
        Lang::En,
        "card:i-5",
        "2026-09-30T10:00:00Z",
    );
    let Item::Card(question) = &detail.sections[0].items[0] else {
        panic!("the question must be a card")
    };
    assert!(question.actions.len() <= 3);
}

/// Rewrites which pull request the final delivery carries.
fn deliver_pr(state: &mut DomainState, pr_id: &str) {
    let head = state.pull_requests[&pr_id.into()].head.clone();
    if let InteractionPayload::Delivery { targets, .. } = &mut state.interactions.get_mut(&"i-3".into()).unwrap().payload {
        *targets = vec![crate::domain::DeliveryTarget::PullRequest { pr_id: pr_id.into(), head }];
    }
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
            withdrawn: false, card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"), clarification: None,
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
            withdrawn: false, card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"), clarification: None,
        },
    );
    deliver_pr(&mut state, "pr-1");
    state
}

fn golden_fixture() -> DomainState {
    let mut state = pr_fixture();
    state.interactions.remove(&"i-4".into());
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
        interaction.body = "The review of the pull request waits for your answer.".into();
    }
    if let Some(interaction) = state.interactions.get_mut(&"i-6".into()) {
        interaction.body = "The pull request checks passed".into();
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
        if let InteractionPayload::Delivery { targets, .. } = &mut interaction.payload {
            for target in targets {
                if let crate::domain::DeliveryTarget::PullRequest { pr_id: id, .. } = target {
                    if id.0 == "pr-1" { *id = pr_id.into(); }
                }
            }
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
        task.pr_id = Some(pr_id.into());
    }
    for interaction in state.interactions.values_mut() {
        if matches!(interaction.payload, InteractionPayload::Delivery { .. }) { interaction.blocking = false; }
    }
    state.revision = state.tasks.values().map(|task| task.revision)
        .chain(state.interactions.values().flat_map(|interaction| [interaction.revision, interaction.order]))
        .max()
        .unwrap_or(state.revision);
    if let Some(pr) = state.pull_requests.get_mut(&pr_id.into()) {
        pr.allowed_methods = vec![crate::domain::MergeMethod::Squash, crate::domain::MergeMethod::Merge];
    }
    {
        use crate::domain::{CardKind, CardTarget, ContentKind as Content};
        let mut card = |id: &str, card: crate::domain::CardSpec| {
            state.interactions.get_mut(&id.into()).unwrap().card = card;
        };
        let mut welcome = spec(CardKind::Decide, "Choose the welcome copy",
            "The new empty state is ready; the beta ships with the copy you pick.")
            .content(Content::Options, "Concise: a short title and one explanatory sentence.\nWarm: the more conversational explanation.");
        welcome.recommended = Some("clear".into());
        card("a98d034b-249e-4487-a89b-57de38c26372", welcome);
        card("f12d034b-249e-4487-a89b-57de38c26372", spec(CardKind::Know, "The simulator pass is clean",
            "Swipe navigation, inline replies and attachment previews are ready for review.")
            .content(Content::Report, "- Swipe navigation\n- Inline replies\n- Attachment previews"));
        card("hf-weekly-2026-09-30", spec(CardKind::Review, "Review the beta candidate",
            "All required checks passed and the branch is up to date with main.")
            .content(Content::Diff, "## Beta candidate ready\n\n- Restored the native review layouts\n- Verified the ARM simulator build"));
        let mut schedule = spec(CardKind::Decide, "Ship the compact schedule editor with this beta?",
            "The review of the pull request waits for your answer.");
        card("8fa4034b-249e-4487-a89b-57de38c26372", schedule);
        card("cc2d034b-249e-4487-a89b-57de38c26372", spec(CardKind::Know, "The pull request checks passed", ""));
        for id in ["8fa4034b-249e-4487-a89b-57de38c26372"] {
            let interaction = state.interactions.get_mut(&id.into()).unwrap();
            interaction.payload = InteractionPayload::Question {
                options: vec![
                    QuestionOption { id: "ship".into(), label: "Ship it".into(), description: None, inputs: vec![] },
                    QuestionOption { id: "hold".into(), label: "Hold it back".into(), description: None, inputs: vec![] },
                ],
                answer: None,
            };
        }
        state.interactions.get_mut(&"f12d034b-249e-4487-a89b-57de38c26372".into()).unwrap().blocking = false;
        state.interactions.get_mut(&"cc2d034b-249e-4487-a89b-57de38c26372".into()).unwrap().blocking = false;
    }
    state
}

const TASKS_GOLDENS: [(&str, Client, Lang); 4] = [
    ("tasks-ios.json", Client::Ios, Lang::En),
    ("tasks-ios-es.json", Client::Ios, Lang::Es),
    ("tasks-mac.json", Client::Mac, Lang::En),
    ("tasks-tui.json", Client::Tui, Lang::En),
];

/// The Tasks list as it reads once the orchestrator names its Requests: one Task waiting on another
/// Request's, a legacy untitled Request with several Tasks, one still being split, and finished work.
fn tasks_fixture() -> (DomainState, LiveSnapshot) {
    let mut state = golden_fixture();
    let mut live = golden_live();
    let parent = "t-651c38484cd2f34e74d5107ba05f7f15";
    let base = state.tasks[&parent.into()].clone();
    let add_request = |state: &mut DomainState, id: &str, title: Option<&str>, body: &str| {
        let mut request = Request::new(id.into(), body.into());
        request.title = title.map(Into::into);
        state.requests.insert(request.id.clone(), request);
    };
    let add_task = |state: &mut DomainState, id: &str, request: &str, title: &str, task_state: TaskState, after: &[&str]| {
        let mut task = base.clone();
        task.id = id.into();
        task.request_id = request.into();
        task.title = title.into();
        task.state = task_state;
        task.agent_id = None;
        task.final_presentation = None;
        task.pr_id = None;
        task.dependencies = after.iter().map(|id| (*id).into()).collect();
        task.revision = state.revision;
        state.tasks.insert(task.id.clone(), task);
    };
    add_request(&mut state, "r-badge", Some("Badge accuracy"), "The badge keeps counting a push the daemon retracted.");
    add_task(&mut state, "t-badge-count", "r-badge", "Count the retracted pushes", TaskState::Running, &[]);
    add_task(&mut state, "t-badge-decrement", "r-badge", "Decrement the badge on retraction", TaskState::NotStarted, &[parent]);
    add_request(&mut state, "r-legacy", None, "Look at the schedule editor and write the notes for the beta.");
    add_task(&mut state, "t-legacy-editor", "r-legacy", "Review the schedule editor", TaskState::ForYourReview, &[]);
    add_task(&mut state, "t-legacy-notes", "r-legacy", "Write the release notes", TaskState::NotStarted, &[]);
    add_request(&mut state, "r-new", None, "Import lists from Notes");
    add_request(&mut state, "r-answer", Some("Is main green?"), "Is main green right now?");
    let answered = state.requests.get_mut(&"r-answer".into()).unwrap();
    answered.closed = true;
    answered.answer = Some("Yes, since Monday".into());
    add_request(&mut state, "r-scheduled", None, "This run belongs to a scheduled task of Alondra. Run the weekly improvement cycle for Lumo.");
    add_task(&mut state, "t-scheduled-lumo", "r-scheduled", "Lumo · weekly improvement", TaskState::Running, &[]);
    scheduled_run(&mut state, "r-scheduled", "t-scheduled-lumo");
    for (id, at) in [
        ("r-9f2156b5a8894e53901f0af576dff6a2", "2026-09-30T09:40:00.000Z"),
        ("r-badge", "2026-09-30T09:52:00.000Z"),
        ("r-legacy", "2026-09-29T17:10:00.000Z"),
        ("r-new", "2026-09-30T09:59:00.000Z"),
    ] {
        live.request_times.insert(id.into(), at.into());
    }
    (state, live)
}

fn scheduled_run(state: &mut DomainState, request: &str, task: &str) {
    use crate::domain::{ScheduleRun, ScheduleRunOrigin, ScheduleRunState};
    let id = format!("run-{task}");
    state.schedule.runs.insert(id.clone().into(), ScheduleRun {
        id: id.into(), schedule_id: "0789f185-0000".into(), origin: ScheduleRunOrigin::Due,
        due_at: Some(100), claimed_at: Some(100), claim_revision: Some(1), finished_at: Some(101),
        state: ScheduleRunState::Sent, request_id: Some(request.into()), task_id: Some(task.into()),
        error: None, definition_snapshot: None, launch: None,
    });
}

fn active_titles(view: &View) -> Vec<String> {
    fn walk(rows: &[Row], out: &mut Vec<String>) {
        for row in rows {
            out.push(row.title.clone());
            walk(&row.children, out);
        }
    }
    let mut out = Vec::new();
    for section in view.sections.iter().filter(|section| section.id == "active") {
        let rows: Vec<Row> = section.items.iter().filter_map(|item| match item {
            Item::Row(row) => Some(row.clone()),
            _ => None,
        }).collect();
        walk(&rows, &mut out);
    }
    out
}

#[test]
fn a_scheduled_run_in_progress_is_an_active_task_marked_scheduled_and_finishes_like_any_other() {
    let (mut state, live) = tasks_fixture();
    for (client, lang, mark) in [(Client::Ios, Lang::En, "Scheduled"), (Client::Ios, Lang::Es, "Programada"),
        (Client::Mac, Lang::En, "Scheduled"), (Client::Tui, Lang::Es, "Programada")] {
        let view = render(&state, &live, client, lang, "tasks", "");
        assert!(active_titles(&view).iter().any(|title| title == "Lumo · weekly improvement"), "{client:?}: {view:?}");
        let rows = serde_json::to_string(&view).unwrap();
        assert!(rows.contains(&format!("\"text\":\"{mark}\"")), "{client:?} {lang:?}: {rows}");
    }
    let sidebar = serde_json::to_string(&render(&state, &live, Client::Mac, Lang::En, "sidebar", "")).unwrap();
    assert!(sidebar.contains("Lumo · weekly improvement") && sidebar.contains("\"text\":\"Scheduled\""), "{sidebar}");
    let unmarked = serde_json::to_string(&render(&state, &live, Client::Ios, Lang::En, "task:t-badge-count", "")).unwrap();
    assert!(!unmarked.contains("\"text\":\"Scheduled\""), "{unmarked}");

    state.tasks.get_mut(&"t-scheduled-lumo".into()).unwrap().state = TaskState::Done;
    let view = render(&state, &live, Client::Ios, Lang::En, "tasks", "");
    assert!(!active_titles(&view).iter().any(|title| title == "Lumo · weekly improvement"));
    let finished = view.sections.iter().find(|section| section.id == "finished").unwrap();
    assert!(serde_json::to_string(finished).unwrap().contains("Lumo · weekly improvement"));
}

#[test]
fn the_tasks_counter_counts_only_tasks_still_under_way() {
    let (mut state, live) = tasks_fixture();
    let count = |state: &DomainState| {
        render(state, &live, Client::Tui, Lang::En, "tasks", "").status.counts.iter()
            .find(|count| count.id == "tasks").unwrap().value
    };
    let all = state.tasks.len() as u32;
    assert!(state.tasks.values().all(|task| task.active()));
    assert_eq!(count(&state), all);
    state.tasks.get_mut(&"t-legacy-editor".into()).unwrap().state = TaskState::Done;
    state.tasks.get_mut(&"t-legacy-notes".into()).unwrap().state = TaskState::Cancelled;
    let archived = state.tasks.get_mut(&"t-badge-count".into()).unwrap();
    (archived.state, archived.archived) = (TaskState::Done, true);
    assert_eq!(count(&state), all - 3);
    for waiting in [TaskState::WaitingDependency, TaskState::NeedsYou, TaskState::ForYourReview, TaskState::Closing] {
        state.tasks.get_mut(&"t-legacy-editor".into()).unwrap().state = waiting;
        assert_eq!(count(&state), all - 2, "{waiting:?}");
    }
}

#[test]
fn tasks_views_match_goldens() {
    let (state, live) = tasks_fixture();
    for (filename, client, lang) in TASKS_GOLDENS {
        let expected = match filename {
            "tasks-ios.json" => include_str!("../../../../fixtures/views/cards/tasks-ios.json"),
            "tasks-ios-es.json" => include_str!("../../../../fixtures/views/cards/tasks-ios-es.json"),
            "tasks-mac.json" => include_str!("../../../../fixtures/views/cards/tasks-mac.json"),
            _ => include_str!("../../../../fixtures/views/cards/tasks-tui.json"),
        };
        let actual = render(&state, &live, client, lang, "tasks", "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

const NODE_CLOSED_GOLDENS: [(&str, Lang); 2] = [("task-closed-ios.json", Lang::En), ("task-closed-ios-es.json", Lang::Es)];
const NODE_CLOSED_TASK: &str = "t-651c38484cd2f34e74d5107ba05f7f15";

/// The golden task after its node was closed by the user: freed, with nothing left open.
fn node_closed_fixture() -> (DomainState, LiveSnapshot) {
    let mut state = golden_fixture();
    let id: crate::domain::TaskId = NODE_CLOSED_TASK.into();
    state.interactions.retain(|_, interaction| interaction.scope != Scope::Task { task_id: id.clone() });
    state.holds.retain(|interaction, _| state.interactions.contains_key(interaction));
    let task = state.tasks.get_mut(&id).unwrap();
    (task.state, task.started, task.agent_id, task.final_presentation) = (TaskState::NotStarted, false, None, None);
    let mut live = golden_live();
    live.closed_tasks.push(id.0);
    (state, live)
}

#[test]
fn a_task_whose_node_was_closed_matches_goldens() {
    let (state, live) = node_closed_fixture();
    for (filename, lang) in NODE_CLOSED_GOLDENS {
        let expected = match filename {
            "task-closed-ios.json" => include_str!("../../../../fixtures/views/cards/task-closed-ios.json"),
            _ => include_str!("../../../../fixtures/views/cards/task-closed-ios-es.json"),
        };
        let actual = render(&state, &live, Client::Ios, lang, &format!("task:{NODE_CLOSED_TASK}"), "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
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
        context_percent: Some(42),
        context_window: Some(1_000_000),
        cost_cents: Some(125),
        ..Default::default()
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
    deliver_pr(&mut state, id);
    let live = LiveSnapshot::default();
    let card = render(&state, &live, Client::Ios, Lang::En, &format!("card:{id}"), "");
    let Item::Card(card) = &card.sections[0].items[0] else { panic!("PR card") };
    let reply = card.reply.as_ref().expect("every card can be answered to its node");
    assert!(action_id::ActionId::parse(&reply.id).is_ok());
    assert!(push_actions(&state, "i-3", Lang::En).unwrap().iter().any(|action|
        action.id.starts_with("reply:i-3@") && action.text), "a push replies in text");
    let detail = render(&state, &live, Client::Mac, Lang::En, &format!("pull_request:{id}"), "");
    assert!(detail.sections.iter().flat_map(|section| &section.items).any(|item|
        matches!(item, Item::ContentRef(content) if content.fetch ==
            "/pull-requests/github.com%2Fbitomule%2Falondra%23154/diff")));
    let tui = render(&state, &live, Client::Tui, Lang::En, &format!("card:{id}"), "");
    let Item::Card(tui_card) = &tui.sections[0].items[0] else { panic!("PR card") };
    assert!(tui_card.more.iter().any(|action| action.key.as_deref() == Some("d") &&
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
    assert!(!card.actions.iter().any(|action| action.label == "Request changes"));
    assert!(card.more.iter().any(|action| action.label == "Close without merging"),
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
        *targets = vec![crate::domain::DeliveryTarget::DocumentRevision {
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
    let card = &decidable_card(&state, Lang::En, "i-3");
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
        assert_eq!(answer.option_id.as_deref(), Some("separate"));
        let changes = card.actions.iter().find(|action| action.id.starts_with("changes:i-1@")).expect("request changes");
        assert_eq!((changes.label.as_str(), changes.stamp.as_deref(), changes.enabled), ("Pedir cambios", Some("Cambios solicitados"), true));
        assert_eq!(changes.comments, Some(CommentSpec { kind: CommentKind::Blocks }));
        assert!(changes.key.is_none());
    }
}

#[test]
fn a_question_linked_to_no_document_keeps_its_plain_answer() {
    let mut state = plan_question_fixture();
    state.interactions.get_mut(&"i-1".into()).unwrap().subject = None;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert!(document_content(&view).is_none());
    let card = card_of(&view);
    assert!(card.actions.iter().all(|action| !action.id.starts_with("changes:")), "{:?}", card.actions);
}

/// The card the fixture's delivery would have if the user could decide it now: the fixture is not
/// a state the domain accepts, so its own availability says no to everything.
fn decidable_card(state: &DomainState, lang: Lang, interaction: &str) -> Card {
    let domain = with_cards(state);
    let live = LiveSnapshot::default();
    let mut item = domain.projection().inbox.into_iter().find(|item| item.id == format!("interaction:{interaction}")).expect("the card exists");
    for action in &mut item.available_actions {
        action.enabled = true;
        action.disabled_reason = None;
    }
    let context = RenderContext { domain: &domain, live: &live, client: Client::Ios, lang, transport: Transport::Loopback,
        cursor: "cursor-1", rendered_at: "" };
    card::from_item(&item, &context, true).unwrap()
}

fn delivery_card(state: &DomainState, lang: Lang, key: &str) -> Card {
    let view = render(state, &LiveSnapshot::default(), Client::Ios, lang, key, "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("delivery card") };
    card.clone()
}

fn reply_to_node(card: &Card) -> &Action {
    let reply = card.reply.as_ref().expect("every card can be answered to its node");
    assert!(reply.id.starts_with("reply:"), "{}", reply.id);
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
        *targets = vec![crate::domain::DeliveryTarget::DocumentRevision {
            document_id: "document-1".into(), revision_id: "revision-1".into(),
        }];
    }
}

#[test]
fn plain_delivery_offers_accept_and_request_changes_with_a_reply_to_the_node() {
    let mut state = fixture();
    state.interactions.get_mut(&"i-3".into()).unwrap().author = Author::Agent { agent_id: "a-1".into() };
    let card = decidable_card(&state, Lang::En, "i-3");
    assert_eq!(card.kind, Some(crate::domain::CardKind::Review));
    let accept = card.actions.iter().find(|action| action.label == "Accept").expect("accept");
    assert_eq!((accept.style, accept.role), (Style::Primary, Some(Role::Advance)));
    let changes = card.actions.iter().find(|action| action.label == "Request changes").expect("request changes");
    assert_eq!(changes.role, Some(Role::Neutral));
    let reply = reply_to_node(&card);
    assert_eq!(reply.label, "Reply");
    assert!(matches!(&reply.input, Input::Text { placeholder, required: true, .. } if placeholder == "Reply to Node One…"));
    assert_eq!(reply.stamp.as_deref(), Some("Sent"));
}

#[test]
fn delivery_reply_placeholder_is_localized_and_falls_back_to_this_node() {
    let state = fixture();
    let spanish = delivery_card(&state, Lang::Es, "card:i-3");
    let reply = reply_to_node(&spanish);
    assert_eq!(reply.label, "Responder");
    assert!(matches!(&reply.input, Input::Text { placeholder, .. } if placeholder == "Responder a este nodo…"));
    let english = delivery_card(&state, Lang::En, "card:i-3");
    assert!(matches!(&reply_to_node(&english).input, Input::Text { placeholder, .. } if placeholder == "Reply to this node…"));
}

#[test]
fn delivery_with_a_document_offers_the_review_with_comments_beside_the_reply() {
    let mut state = fixture();
    attach_document(&mut state);
    let card = decidable_card(&state, Lang::En, "i-3");
    assert!(card.actions.iter().any(|action| action.label == "Approve" && action.style == Style::Primary));
    let changes = card.actions.iter().find(|action| action.id.starts_with("changes:")).expect("review feedback");
    assert!(changes.comments.is_some());
    reply_to_node(&card);
    assert_eq!(card.content_row.as_ref().map(|row| (row.kind.as_str(), row.summary.as_str())), Some(("document", "Design notes")));
}

#[test]
fn a_delivery_carrying_a_claude_artifact_asks_for_changes_in_a_sheet_with_comments_off() {
    let mut state = fixture();
    state.interactions.get_mut(&"i-3".into()).unwrap().body =
        "Plan ready: https://claude.ai/artifact/abc123".into();
    let card = decidable_card(&state, Lang::En, "i-3");
    let changes = card.actions.iter().find(|action| action.label == "Request changes").expect("request changes");
    assert!(matches!(&changes.input, Input::Text { placeholder, .. } if placeholder == "Describe the changes"));
    assert!(card.actions.iter().any(|action| action.label == "Accept" && action.style == Style::Primary));
    reply_to_node(&card);
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
        .find(|action| action.id.starts_with("read:"))
        .expect("information card keeps its read action");
    assert_eq!((read.label.as_str(), read.role), ("Seen", Some(Role::Neutral)));
}

#[test]
fn a_question_without_options_is_answered_in_text_and_one_with_options_offers_its_own_words_behind_more() {
    let view = render(&fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("question card") };
    let answer = card.actions.iter().find(|action| action.id.starts_with("ans:")).expect("answer action");
    assert_eq!((answer.presentation, answer.option_id.clone()), (Presentation::Button, None));
    assert!(matches!(&answer.input, Input::Text { required: true, multiline: true, .. }));
    assert!(answer.attachments);

    let view = render(&golden_fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En,
        "card:a98d034b-249e-4487-a89b-57de38c26372", "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("decision card") };
    assert_eq!(card.kind, Some(crate::domain::CardKind::Decide));
    assert_eq!(card.actions.iter().map(|a| a.option_id.as_deref()).collect::<Vec<_>>(), [Some("clear"), Some("warm")]);
    assert!(card.more.iter().any(|action| action.label == "Write my own answer"
        && matches!(&action.input, Input::Text { required: true, .. })));
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
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Parser work".into());
    state.tasks.get_mut(&"t-1".into()).unwrap().title = "Wire the parser".into();
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
    assert_eq!(request.value, "Parser work");
    assert!(view.sections.iter().any(|section| section.id == "dependencies"));
    let options = view.sections.iter().find(|section| section.id == "options").unwrap();
    let facts = options.items.iter().find_map(|item| if let Item::Facts(facts) = item { Some(&facts.items) } else { None }).unwrap();
    assert!(facts.iter().any(|fact| fact.label == "Project" && fact.value == "Alondra"));
    assert!(facts.iter().any(|fact| fact.label == "Effort" && fact.value == "High"));
}

#[test]
fn a_tui_fleet_node_is_one_row_named_by_its_task_with_its_state_and_since_when() {
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode {
        agent_id: "a-1".into(), name: "Node One".into(), working: true, project: Some("App".into()),
        tool: Some("cargo test".into()), state_since: Some("2026-09-30T09:58:00Z".into()),
        ..LiveNode::default()
    });
    let view = render(&state, &live, Client::Tui, Lang::En, "fleet", "");
    let node = view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Row(row) = item && row.id == "a-1" { Some(row) } else { None }
    }).unwrap();
    assert_eq!(node.title, "Build a feature");
    assert!(node.children.is_empty(), "one task per node: no task row repeats the node");
    assert_eq!(node.badges.first().map(|badge| badge.text.as_str()), Some("Working"));
    assert_eq!(node.subtitle.as_deref(), Some("cargo test"));
    assert_eq!(node.trailing, Some(Trailing::Time { at: "2026-09-30T09:58:00Z".into(), format: TimeFormat::Relative }));
    assert!(view.sections.iter().any(|section| section.title.as_deref() == Some("App")));
}

#[test]
fn a_tui_task_list_shows_a_request_with_one_task_as_that_task_and_nests_only_several() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    let mut live = LiveSnapshot::default();
    live.request_times.insert("r-1".into(), "2026-10-03T19:49:18.945+02:00".into());
    let row = |state: &DomainState| {
        let view = render(state, &live, Client::Tui, Lang::Es, "tasks", "");
        view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
            Item::Row(row) if row.id == "r-1" => Some(row.clone()),
            _ => None,
        }).unwrap()
    };
    let single = row(&state);
    assert!(single.children.is_empty());
    assert_eq!(single.title, "Build a feature");
    assert_eq!(single.badges.first().map(|badge| badge.text.as_str()), Some("Trabajando"));
    assert!(single.subtitle.is_none(), "the request's own state would contradict the task's");
    assert!(!matches!(single.trailing, Some(Trailing::Count { .. })));
    let mut second = state.tasks[&"t-1".into()].clone();
    second.id = "t-2".into();
    second.title = "Write the docs".into();
    second.state = TaskState::Done;
    state.tasks.insert(second.id.clone(), second);
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Docs and feature".into());
    let several = row(&state);
    assert_eq!(several.children.iter().map(|task| task.id.as_str()).collect::<Vec<_>>(), ["t-1", "t-2"]);
    assert_eq!(several.badges.first().map(|badge| badge.text.as_str()), Some("1 de 2"));
    assert!(several.children.iter().all(|task| !matches!(task.trailing, Some(Trailing::Count { .. }))));
}

#[test]
fn an_empty_tui_inbox_has_no_section_titles_only_its_empty_state() {
    let mut state = fixture();
    state.interactions.clear();
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "home", "");
    assert!(view.sections.iter().all(|section| section.title.is_none()), "{:?}", view.sections);
    assert!(view.empty.is_some());
}

#[test]
fn task_detail_offers_only_the_actions_its_state_allows() {
    let mut state = fixture();
    state.revision = 5;
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().final_presentation = None;
    let cases = [
        (TaskState::NotStarted, false, false, &["Cancel Task"][..]),
        (TaskState::Running, false, false, &["Cancel Task"]),
        (TaskState::NeedsYou, false, false, &["Cancel Task"]),
        (TaskState::ForYourReview, false, false, &["Cancel Task"]),
        (TaskState::WaitingDependency, false, false, &["Cancel Task"]),
        (TaskState::Closing, false, false, &["Cancel Task"]),
        (TaskState::Done, false, false, &["Reopen", "Archive"]),
        (TaskState::Cancelled, false, false, &["Reopen", "Archive"]),
        (TaskState::Done, true, false, &["Reopen", "Unarchive"]),
        (TaskState::Done, false, true, &["Archive"]),
        (TaskState::Cancelled, true, true, &["Unarchive"]),
    ];
    for (task_state, archived, request_closed, expected) in cases {
        let task = state.tasks.get_mut(&"t-1".into()).unwrap();
        task.state = task_state;
        task.archived = archived;
        state.requests.get_mut(&"r-1".into()).unwrap().closed = request_closed;
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            let view = render(&state, &LiveSnapshot::default(), client, Lang::En, "task:t-1", "");
            let actions = tui_document_actions(&view);
            let case = format!("{task_state:?} archived={archived} request_closed={request_closed} {client:?}");
            assert!(actions.iter().all(|action| action.enabled && action.disabled_reason.is_none()), "{case}: {actions:?}");
            let labels: Vec<&str> = actions.iter().map(|action| action.label.as_str())
                .filter(|label| *label != "Message").collect();
            assert_eq!(labels, expected, "{case}");
            assert_eq!(actions.iter().any(|action| action.key.is_some()), client == Client::Tui, "{case}: keys are a terminal thing");
        }
    }
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
            if let Item::Row(row) = item && row.id == "a-1" { Some(row.title.clone()) } else { None }
        }).unwrap()
    };
    assert_eq!(node_row(&state), "Build a feature");
    let detail = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "node:a-1", "");
    assert!(header_facts(&detail).iter().any(|fact| fact.label == "Task" && fact.value == "Build a feature"));
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    let idle = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "node:a-1", "");
    assert!(header_facts(&idle).iter().all(|fact| fact.label != "Task"), "a node between tasks is on none");
    state.tasks.get_mut(&"t-1".into()).unwrap().revision = 2;
    assert_eq!(node_row(&state), "Build a feature", "between tasks a node shows the last one it had");
}

#[test]
fn each_fleet_node_shows_one_state_derived_from_its_task_never_its_connection() {
    let mut state = fixture();
    state.interactions.clear();
    let template = state.tasks.remove(&"t-1".into()).unwrap();
    for (agent, connected, task, task_state) in [
        ("one-task", true, "t-one-task-per-node", TaskState::ForYourReview),
        ("recent-queue", false, "t-recent-requests-queue", TaskState::Done),
        ("home-zero", true, "t-home-counters-zero", TaskState::Done),
        ("claude-mods", true, "t-claude-mods-analysis", TaskState::Done),
        ("busy", true, "t-busy", TaskState::Running),
        ("idle", true, "t-idle", TaskState::Running),
    ] {
        state.agents.insert(agent.into(), Agent { id: agent.into(), name: agent.into(), connected });
        let mut row = template.clone();
        row.id = task.into();
        row.agent_id = Some(agent.into());
        row.state = task_state;
        state.tasks.insert(task.into(), row);
    }
    let mut live = LiveSnapshot::default();
    for (agent, key, working) in [
        ("one-task", "free", false), ("recent-queue", "free", false), ("home-zero", "free", false),
        ("busy", "busy", true), ("idle", "free", false),
    ] {
        live.nodes.push(LiveNode {
            agent_id: agent.into(), name: agent.into(), state_key: Some(key.into()),
            state_label: Some("inactivo".into()), working, ..LiveNode::default()
        });
    }
    let view = render(&state, &live, Client::Ios, Lang::Es, "fleet", "");
    let node = |id: &str| view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
        if let Item::Row(row) = item && row.id == id { Some(row.clone()) } else { None }
    }).unwrap();
    for (agent, text, tone) in [
        ("one-task", "Espera tu revisión", Tone::Accent),
        ("recent-queue", "Terminado", Tone::Success),
        ("home-zero", "Terminado", Tone::Success),
        ("claude-mods", "Terminado", Tone::Success),
        ("busy", "Trabajando", Tone::Info),
        ("idle", "Inactivo", Tone::Neutral),
    ] {
        let row = node(agent);
        assert_eq!(row.title, "Build a feature", "{agent}");
        assert_eq!(row.subtitle.as_deref(), Some(agent), "{agent}");
        assert_eq!(row.badges, vec![Label { text: text.into(), tone }], "{agent}");
    }
    let detail = render(&state, &live, Client::Ios, Lang::Es, "node:recent-queue", "");
    let Item::Facts(facts) = &detail.sections.iter().find(|section| section.id == "header").unwrap().items[0] else {
        panic!("header facts")
    };
    assert_eq!(facts.items[0].value, "Terminado");
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

fn tasks_items(state: &DomainState, client: Client, lang: Lang) -> Vec<Row> {
    let view = render(state, &LiveSnapshot::default(), client, lang, "tasks", "");
    let active = view.sections.into_iter().find(|section| section.id == "active").unwrap();
    active.items.into_iter().filter_map(|item| if let Item::Row(row) = item { Some(row) } else { None }).collect()
}

fn two_tasks_with_body(body: &str) -> DomainState {
    let mut state = fixture();
    let mut second = state.tasks[&"t-1".into()].clone();
    second.id = "t-2".into();
    second.title = "Second thing".into();
    state.tasks.insert("t-2".into(), second);
    state.requests.get_mut(&"r-1".into()).unwrap().body = body.into();
    state
}

fn every_text(rows: &[Row]) -> Vec<String> {
    rows.iter().flat_map(|row| {
        let mut texts = vec![row.title.clone()];
        texts.extend(row.subtitle.clone());
        texts.extend(every_text(&row.children));
        texts
    }).collect()
}

#[test]
fn a_request_with_several_tasks_shows_the_orchestrators_title() {
    let mut state = two_tasks_with_body("Can Alondra detect conflicts before sending the PR push?");
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Push without a card".into());
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for lang in [Lang::En, Lang::Es] {
            let rows = tasks_items(&state, client, lang);
            assert_eq!(rows.len(), 1);
            assert_eq!((rows[0].id.as_str(), rows[0].title.as_str(), rows[0].depth), ("r-1", "Push without a card", 0));
        }
    }
}

#[test]
fn an_untitled_request_with_several_tasks_has_no_header_and_its_tasks_stand_loose() {
    let body = "Can Alondra detect conflicts before sending the PR push? I got the push and then there was no card";
    let state = two_tasks_with_body(body);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let rows = tasks_items(&state, client, Lang::En);
        assert_eq!(rows.iter().map(|row| (row.id.as_str(), row.depth)).collect::<Vec<_>>(), [("t-1", 1), ("t-2", 1)], "{client:?}");
        assert!(every_text(&rows).iter().all(|text| !text.contains("Can Alondra")), "the prompt is never a title");
    }
}

#[test]
fn an_untitled_request_with_one_task_is_that_task_and_one_with_none_is_new() {
    let mut state = fixture();
    state.requests.insert("r-2".into(), Request::new("r-2".into(), "Prompt text that is not a title".into()));
    for (lang, new_request) in [(Lang::En, "New request"), (Lang::Es, "Nueva petición")] {
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            let rows = tasks_items(&state, client, lang);
            let title = |id: &str| rows.iter().find(|row| row.id == id).unwrap().title.clone();
            assert_eq!(title("r-1"), "Build a feature");
            assert_eq!(title("r-2"), new_request);
            assert!(every_text(&rows).iter().all(|text| !text.contains("Prompt text")), "the prompt is never a title");
        }
    }
}

#[test]
fn a_closed_request_that_only_answered_says_so() {
    let mut state = fixture();
    state.tasks.clear();
    let request = state.requests.get_mut(&"r-1".into()).unwrap();
    request.closed = true;
    request.answer = Some("Yes, since Monday".into());
    for (lang, label) in [(Lang::En, "Answered"), (Lang::Es, "Respondida")] {
        let view = render(&state, &LiveSnapshot::default(), Client::Ios, lang, "tasks", "");
        let finished = view.sections.iter().find(|section| section.id == "finished").unwrap();
        let Item::Row(row) = &finished.items[0] else { panic!("request row") };
        assert_eq!((row.subtitle.as_deref(), row.icon), (Some(label), Icon::Check));
    }
    state.requests.get_mut(&"r-1".into()).unwrap().answer = None;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "tasks", "");
    let finished = view.sections.iter().find(|section| section.id == "finished").unwrap();
    let Item::Row(row) = &finished.items[0] else { panic!("request row") };
    assert_eq!(row.subtitle.as_deref(), Some("Finished"));
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
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Three tasks".into());
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
fn on_the_iphone_a_task_row_says_what_its_node_does_now_like_fleet() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode {
        agent_id: "a-1".into(), name: "Node One".into(), working: true,
        tool: Some("cargo test".into()), state_since: Some("2026-09-30T09:58:00Z".into()),
        ..LiveNode::default()
    });
    let task_children = |client| {
        let view = render(&state, &live, client, Lang::En, "tasks", "");
        view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
            Item::Row(row) if row.id == "r-1" => row.children.iter().find(|child| child.id == "t-1").map(|task| task.children.clone()),
            _ => None,
        }).unwrap()
    };
    let ios = task_children(Client::Ios);
    assert_eq!(ios.len(), 1);
    assert_eq!(ios[0].id, "activity");
    assert_eq!(ios[0].title, "cargo test");
    assert_eq!(ios[0].trailing, Some(Trailing::Time {
        at: "2026-09-30T09:58:00Z".into(), format: TimeFormat::Relative,
    }));
    let tui = render(&state, &live, Client::Tui, Lang::En, "tasks", "");
    assert!(tui.sections.iter().flat_map(|section| &section.items).all(|item| match item {
        Item::Row(row) => row.children.is_empty(),
        _ => true,
    }), "the TUI shows a one-task request as that one task row");
}

#[test]
fn only_the_iphone_gets_no_link_to_the_request_view() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().title = "Wire the parser".into();
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Parser work".into());
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
    let schedule: crate::domain::Schedule = serde_json::from_value(serde_json::json!({
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
        waiting: false, state_since: None, last_message: None, last_message_at: None, ..Default::default()
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
        matches!(item, Item::Card(card) if card.more.iter().any(|action|
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
        include_str!("../../../../fixtures/views/cards/home-ios.json"),
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
        include_str!("../../../../fixtures/views/cards/home-tui.json"),
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
            include_str!("../../../../fixtures/views/cards/card-question.json"),
        ),
        (
            "card-information.json",
            "card:f12d034b-249e-4487-a89b-57de38c26372",
            include_str!("../../../../fixtures/views/cards/card-information.json"),
        ),
        (
            "card-pr.json",
            "card:github.com/example/repo#12",
            include_str!("../../../../fixtures/views/cards/card-pr.json"),
        ),
        (
            "card-delivery.json",
            "card:hf-weekly-2026-09-30",
            include_str!("../../../../fixtures/views/cards/card-delivery.json"),
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

/// Only a destructive action asks "are you sure?": merging, accepting, approving and answering are
/// the decision itself, on every client and in every language.
#[test]
fn only_destructive_card_actions_ask_for_confirmation() {
    fn walk(value: &serde_json::Value, merges: &mut usize, confirms: &mut usize) {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("id").and_then(|id| id.as_str()).is_some_and(|id| id.starts_with("merge:")) {
                    *merges += 1;
                }
                if map.get("confirm").is_some_and(|confirm| !confirm.is_null()) {
                    *confirms += 1;
                    assert_eq!(map["style"], "destructive", "only a destructive action confirms: {value}");
                }
                map.values().for_each(|child| walk(child, merges, confirms));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|child| walk(child, merges, confirms)),
            _ => {}
        }
    }
    let state = golden_fixture();
    let live = golden_live();
    let (mut merges, mut confirms) = (0, 0);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for lang in [Lang::En, Lang::Es] {
            for key in [
                "card:github.com/example/repo#12",
                "card:hf-weekly-2026-09-30",
                "card:a98d034b-249e-4487-a89b-57de38c26372",
                "pull_request:github.com/example/repo#12",
            ] {
                let view = render(&state, &live, client, lang, key, "2026-09-30T10:00:00Z");
                walk(&serde_json::to_value(&view).unwrap(), &mut merges, &mut confirms);
            }
        }
    }
    assert!(merges > 0, "the fixtures offer a merge");
    assert!(confirms > 0, "closing without merging still confirms");
}

#[test]
fn spanish_cards_match_goldens() {
    let state = golden_fixture();
    let live = golden_live();
    for (filename, client, key, expected) in [
        ("card-pr-es.json", Client::Ios, "card:github.com/example/repo#12", include_str!("../../../../fixtures/views/cards/card-pr-es.json")),
        ("card-pr-es-mac.json", Client::Mac, "card:github.com/example/repo#12", include_str!("../../../../fixtures/views/cards/card-pr-es-mac.json")),
        ("card-question-es.json", Client::Ios, "card:a98d034b-249e-4487-a89b-57de38c26372", include_str!("../../../../fixtures/views/cards/card-question-es.json")),
        ("card-information-es.json", Client::Ios, "card:f12d034b-249e-4487-a89b-57de38c26372", include_str!("../../../../fixtures/views/cards/card-information-es.json")),
        ("card-delivery-es.json", Client::Ios, "card:hf-weekly-2026-09-30", include_str!("../../../../fixtures/views/cards/card-delivery-es.json")),
    ] {
        let actual = render(&state, &live, client, Lang::Es, key, "2026-09-30T10:00:00Z");
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
        include_str!("../../../../fixtures/views/cards/home-ios-es.json"),
    );
}

#[test]
fn mac_views_match_goldens() {
    let state = golden_fixture();
    let live = golden_live();
    for (filename, key, expected) in [
        ("sidebar-mac.json", "sidebar", include_str!("../../../../fixtures/views/cards/sidebar-mac.json")),
        ("card-question-mac.json", "card:a98d034b-249e-4487-a89b-57de38c26372", include_str!("../../../../fixtures/views/cards/card-question-mac.json")),
        ("card-pr-mac.json", "card:github.com/example/repo#12", include_str!("../../../../fixtures/views/cards/card-pr-mac.json")),
        ("card-information-mac.json", "card:f12d034b-249e-4487-a89b-57de38c26372", include_str!("../../../../fixtures/views/cards/card-information-mac.json")),
        ("pull-request-mac.json", "pull_request:github.com/example/repo#12", include_str!("../../../../fixtures/views/cards/pull-request-mac.json")),
        ("node-mac.json", "node:d887a34b-249e-4487-a89b-57de38c26372", include_str!("../../../../fixtures/views/cards/node-mac.json")),
    ] {
        let actual = render(&state, &live, Client::Mac, Lang::En, key, "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

#[test]
fn the_mac_sidebar_lists_one_waiting_section_by_need() {
    let state = golden_fixture();
    let live = golden_live();
    let sidebar = render(&state, &live, Client::Mac, Lang::En, "sidebar", "");
    let ids: Vec<_> = sidebar.sections.iter().map(|section| section.id.as_str()).collect();
    assert!(!ids.contains(&"pull_requests"), "a pull request waits in the same list as everything else: {ids:?}");
    let waiting = sidebar.sections.iter().find(|section| section.id == "waiting").unwrap();
    let rows: Vec<_> = waiting.items.iter().filter_map(|item| match item { Item::Row(row) => Some(row), _ => None }).collect();
    assert_eq!(rows.len() as u32, waiting.count.unwrap());
    assert!(rows.iter().any(|row| row.kind == Some(CardKind::Review)), "the pull request review is listed with the rest");
    for row in &rows {
        let Some(Item::Card(card)) = render(&state, &live, Client::Mac, Lang::En, &format!("card:{}", row.id), "").sections.iter()
            .flat_map(|section| section.items.clone()).next() else { panic!("{} renders as a card", row.id) };
        assert_eq!(Some(&row.title), card.need.as_ref(), "a row is titled by what the card needs");
        assert_eq!(row.kind, card.kind, "a row carries its card's kind");
        assert_eq!(row.badges.first(), card.eyebrow.as_ref());
    }
}

#[test]
fn the_mac_sidebar_does_not_say_the_same_thing_twice() {
    let state = golden_fixture();
    let live = golden_live();
    let sidebar = render(&state, &live, Client::Mac, Lang::En, "sidebar", "");
    let tasks = sidebar.sections.iter().find(|section| section.id == "tasks").unwrap();
    for item in &tasks.items {
        let Item::Row(group) = item else { continue };
        for child in &group.children {
            assert_ne!(child.subtitle.as_deref(), Some(group.title.as_str()), "a task under \"{}\" does not repeat it", group.title);
        }
    }
    let orchestrator = sidebar.sections.iter().find(|section| section.id == "orchestrator").unwrap();
    for item in &orchestrator.items {
        let Item::Row(row) = item else { continue };
        assert_eq!(row.trailing, None, "the orchestrator state is the badge, not a second label");
        assert_eq!(row.badges.len(), 1);
    }
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
        ("card-pr-es.json", "card:github.com/example/repo#12", Client::Ios, Lang::Es),
        ("card-question-es.json", "card:a98d034b-249e-4487-a89b-57de38c26372", Client::Ios, Lang::Es),
        ("card-information-es.json", "card:f12d034b-249e-4487-a89b-57de38c26372", Client::Ios, Lang::Es),
        ("card-delivery-es.json", "card:hf-weekly-2026-09-30", Client::Ios, Lang::Es),
        ("card-pr-es-mac.json", "card:github.com/example/repo#12", Client::Mac, Lang::Es),
        ("new-request-ios.json", "new_request", Client::Ios, Lang::En),
        ("node-ios.json", "node:d887a34b-249e-4487-a89b-57de38c26372", Client::Ios, Lang::En),
        ("task-ios.json", "task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Ios, Lang::En),
        ("request-ios.json", "request:r-9f2156b5a8894e53901f0af576dff6a2", Client::Ios, Lang::En),
        ("pull-request-ios.json", "pull_request:github.com/example/repo#12", Client::Ios, Lang::En),
        ("machine-ios.json", "machine", Client::Ios, Lang::En),
        ("schedules-ios.json", "schedules", Client::Ios, Lang::En),
        ("schedule-new-ios.json", "schedule:new", Client::Ios, Lang::En),
        ("sidebar-mac.json", "sidebar", Client::Mac, Lang::En),
        ("card-question-mac.json", "card:a98d034b-249e-4487-a89b-57de38c26372", Client::Mac, Lang::En),
        ("card-pr-mac.json", "card:github.com/example/repo#12", Client::Mac, Lang::En),
        ("card-information-mac.json", "card:f12d034b-249e-4487-a89b-57de38c26372", Client::Mac, Lang::En),
        ("pull-request-mac.json", "pull_request:github.com/example/repo#12", Client::Mac, Lang::En),
        ("node-mac.json", "node:d887a34b-249e-4487-a89b-57de38c26372", Client::Mac, Lang::En),
    ];
    let mut with_pending = state.clone();
    let pending = Request::new("r-4b7e2c1d9a0f4e3b8c6d5a4f3e2b1c0d".into(), "Import lists from Notes".into());
    with_pending.requests.insert(pending.id.clone(), pending);
    let (tasks_state, tasks_live) = tasks_fixture();
    for (filename, client, lang) in TASKS_GOLDENS {
        let view = render(&tasks_state, &tasks_live, client, lang, "tasks", "2026-09-30T10:00:00Z");
        std::fs::write(
            format!("{dir}/{filename}"),
            format!("{}\n", serde_json::to_string_pretty(&view).unwrap()),
        )
        .unwrap();
    }
    let (closed_state, closed_live) = node_closed_fixture();
    for (filename, lang) in NODE_CLOSED_GOLDENS {
        let view = render(&closed_state, &closed_live, Client::Ios, lang, &format!("task:{NODE_CLOSED_TASK}"), "2026-09-30T10:00:00Z");
        std::fs::write(
            format!("{dir}/{filename}"),
            format!("{}\n", serde_json::to_string_pretty(&view).unwrap()),
        )
        .unwrap();
    }
    let (fleet_state, fleet_live) = fleet_fixture();
    for (filename, client, lang) in FLEET_GOLDENS {
        let view = render(&fleet_state, &fleet_live, client, lang, "fleet", "2026-09-30T10:00:00Z");
        std::fs::write(
            format!("{dir}/{filename}"),
            format!("{}\n", serde_json::to_string_pretty(&view).unwrap()),
        )
        .unwrap();
    }
    for (filename, key, client, lang) in cases {
        let state = if key == "new_request" { &with_pending } else { &state };
        let view = render(state, &live, client, lang, key, "2026-09-30T10:00:00Z");
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
fn card_detail_context_is_one_line_with_no_task_state() {
    let view = render(&fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "2026-09-30T10:00:00Z");
    let context = view.sections.iter().find(|section| section.id == "context").expect("context line");
    assert_eq!(context.items.len(), 1);
    assert!(matches!(&context.items[0], Item::Facts(facts) if facts.items.len() == 1));
    assert!(view.sections.iter().flat_map(|section| &section.items).all(|item| !matches!(item, Item::Actions(_))));
}

#[test]
fn task_board_nests_a_dependent_under_its_parent() {
    let mut state = fixture();
    let mut child = state.tasks.get(&"t-1".into()).unwrap().clone();
    child.id = "t-2".into();
    child.title = "Follow-up".into();
    child.dependencies.insert("t-1".into());
    state.tasks.insert(child.id.clone(), child);
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Two steps".into());
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "tasks", "2026-09-30T10:00:00Z");
    let active = view.sections.iter().find(|section| section.id == "active").unwrap();
    let Item::Row(request) = &active.items[0] else { panic!("request row") };
    assert_eq!(request.children[0].id, "t-1");
    assert_eq!(request.children[0].children[0].id, "t-2");
    assert_eq!(request.children[0].children[0].depth, 2);
}

fn across_requests() -> DomainState {
    let mut state = fixture();
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Retract pushes".into());
    let mut badges = Request::new("r-2".into(), "The badge keeps counting a retracted push".into());
    badges.title = Some("Badge accuracy".into());
    state.requests.insert("r-2".into(), badges);
    for (id, title, dependencies) in [("t-3", "Count the pushes", &[][..]), ("t-4", "Decrement the badge", &["t-1"][..])] {
        let mut task = state.tasks[&"t-1".into()].clone();
        task.id = id.into();
        task.request_id = "r-2".into();
        task.title = title.into();
        task.state = TaskState::NotStarted;
        task.dependencies = dependencies.iter().map(|id| (*id).into()).collect();
        state.tasks.insert(task.id.clone(), task);
    }
    state
}

fn find<'a>(rows: &'a [Row], id: &str) -> &'a Row {
    rows.iter().find(|row| row.id == id)
        .unwrap_or_else(|| panic!("no row {id} in {:?}", rows.iter().map(|row| &row.id).collect::<Vec<_>>()))
}

fn count_ids(rows: &[Row], id: &str) -> usize {
    rows.iter().map(|row| usize::from(row.id == id) + count_ids(&row.children, id)).sum()
}

#[test]
fn a_task_that_follows_another_requests_task_nests_under_it_there_and_waits_in_its_own_request() {
    let state = across_requests();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let rows = tasks_items(&state, client, Lang::En);
        let parents = find(&rows, "r-1");
        let parent_task = if client == Client::Ios { find(&parents.children, "t-1") } else { parents };
        let nested = find(&parent_task.children, "t-4");
        assert_eq!((nested.title.as_str(), nested.depth), ("Decrement the badge", 2), "{client:?}");
        let origin = match client {
            Client::Ios => find(&nested.children, "from").title.clone(),
            _ => nested.subtitle.clone().unwrap(),
        };
        assert!(origin.ends_with("from: Badge accuracy"), "{client:?}: {origin}");
        let own = find(&rows, "r-2");
        let ids: Vec<_> = own.children.iter().map(|child| child.id.as_str()).collect();
        assert_eq!(ids, ["t-3", "waits:t-4:t-1"], "{client:?}: the nested task is not repeated in its own request");
        let waits = find(&own.children, "waits:t-4:t-1");
        assert_eq!(waits.title, "waits for Build a feature");
        assert_eq!(waits.subtitle.as_deref(), Some("Decrement the badge"));
        assert_eq!(waits.link, Some(Link::View { key: "task:t-4".into() }));
        assert_eq!(count_ids(&rows, "t-4"), 1, "{client:?}: one full row");
    }
}

#[test]
fn the_spanish_labels_for_a_task_that_follows_another_requests_task() {
    let rows = tasks_items(&across_requests(), Client::Tui, Lang::Es);
    let nested = find(&find(&rows, "r-1").children, "t-4");
    assert_eq!(nested.subtitle.as_deref(), Some("de: Badge accuracy"));
    assert_eq!(find(&find(&rows, "r-2").children, "waits:t-4:t-1").title, "espera a Build a feature");
}

#[test]
fn a_dependency_in_a_folded_section_leaves_only_the_waits_row_beside_the_full_task() {
    let mut state = across_requests();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    state.requests.get_mut(&"r-1".into()).unwrap().closed = true;
    let rows = tasks_items(&state, Client::Ios, Lang::En);
    let own = find(&rows, "r-2");
    let decrement = find(&own.children, "t-4");
    assert_eq!(decrement.children.iter().map(|child| child.id.as_str()).collect::<Vec<_>>(), ["waits:t-4:t-1"]);
    assert!(!rows.iter().any(|row| row.id == "r-1"), "the finished request is not in the active list");
}

#[test]
fn a_task_that_follows_its_own_requests_task_and_another_requests_keeps_its_same_request_indent() {
    let mut state = across_requests();
    state.tasks.get_mut(&"t-4".into()).unwrap().dependencies.insert("t-3".into());
    let rows = tasks_items(&state, Client::Ios, Lang::En);
    let own = find(&rows, "r-2");
    let first = find(&own.children, "t-3");
    let decrement = find(&first.children, "t-4");
    assert_eq!(decrement.depth, 2);
    assert_eq!(decrement.children.iter().map(|child| child.id.as_str()).collect::<Vec<_>>(), ["waits:t-4:t-1"]);
    assert!(find(&rows, "r-1").children[0].children.is_empty(), "it is not repeated under the other request's task");
}

#[test]
fn tasks_that_wait_on_each_other_across_requests_are_each_shown_once() {
    let mut state = across_requests();
    state.tasks.get_mut(&"t-1".into()).unwrap().dependencies.insert("t-4".into());
    let rows = tasks_items(&state, Client::Ios, Lang::En);
    assert_eq!((count_ids(&rows, "t-1"), count_ids(&rows, "t-4")), (1, 1));
}

#[test]
fn ambiguous_orchestrator_run_exposes_both_choices_with_confirmation() {
    let state = fixture();
    let live = LiveSnapshot { orchestrator: Some(live::LiveOrchestrator {
        working: false, summary: Some("An execution needs a decision".into()), failure: None,
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
fn orchestrator_waiting(can_retry: bool) -> LiveSnapshot {
    LiveSnapshot { orchestrator: Some(live::LiveOrchestrator {
        working: false, summary: Some("Claude exited without a successful terminal result".into()), failure: None,
        queue_depth: 1, active_run_id: Some("run-1".into()), can_retry, can_cancel: !can_retry,
    }), ..Default::default() }
}

#[test]
fn an_orchestrator_problem_is_a_card_like_any_other_and_comes_first() {
    use crate::domain::{CardKind, ContentKind as Content};
    let mut state = fixture();
    let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None, inputs: vec![] };
    state.interactions.insert("problem-orchestrator-run-1".into(), Interaction {
        attachments: vec![], id: "problem-orchestrator-run-1".into(), scope: Scope::Orchestrator { request_id: None },
        author: Author::System, recipient: Recipient::User, body: "Claude exited without a successful terminal result".into(),
        blocking: false, subject: None,
        payload: InteractionPayload::Question {
            options: vec![option("retry", "Retry"), option("continue", "Continue without repeating"), option("stop", "Stop")],
            answer: None,
        },
        revision: 1, order: 9, withdrawn: false,
        card: spec(CardKind::Problem, "The orchestrator failed partway", "Some of its actions may already have happened.")
            .content(Content::Diagnosis, "Claude exited without a successful terminal result\nretried once"),
        clarification: None,
    });
    let live = LiveSnapshot::default();
    let home = render(&state, &live, Client::Ios, Lang::Es, "home", "");
    assert_eq!(home.sections[0].card_ids, ["problem-orchestrator-run-1", "i-1", "i-4", "i-3", "i-2"], "a problem comes first");

    let view = render(&state, &live, Client::Ios, Lang::Es, "card:problem-orchestrator-run-1", "");
    let card = card_of(&view);
    assert_eq!(card.kind, Some(CardKind::Problem));
    assert_eq!((card.icon, card.eyebrow.as_ref().unwrap().text.as_str()), (Icon::Warning, "Problema"));
    assert!(card.body.is_none(), "the card carries no body");
    assert_eq!(card.content_row.as_ref().map(|row| (row.kind.as_str(), row.summary.as_str())),
        Some(("diagnosis", "Claude exited without a successful terminal result")));
    let diagnosis = view.sections.iter().find(|section| section.id == "content").expect("the diagnosis is the content");
    assert!(matches!(&diagnosis.items[0], Item::Markdown(text) if text.text.contains("retried once")));
    assert_eq!(card.actions.iter().map(|a| (a.option_id.as_deref().unwrap(), a.role.unwrap())).collect::<Vec<_>>(),
        [("retry", Role::Advance), ("continue", Role::Neutral), ("stop", Role::Neutral)]);
    assert!(card.reply.as_ref().is_some_and(|reply| reply.id.starts_with("msg:view@")), "it replies to the orchestrator");

    let tui = render(&state, &live, Client::Tui, Lang::En, "home", "");
    let Item::Card(first) = &tui.sections[0].items[0] else { panic!("problem card") };
    assert_eq!(first.id, "problem-orchestrator-run-1");
    let sidebar = render(&state, &live, Client::Mac, Lang::En, "sidebar", "");
    let waiting = sidebar.sections.iter().find(|section| section.id == "waiting").unwrap();
    assert!(waiting.items.iter().any(|item| matches!(item, Item::Row(row) if row.id == "problem-orchestrator-run-1")));
}

fn orchestrator_screen(live: Option<live::LiveOrchestrator>, lang: Lang) -> (Vec<Fact>, Option<String>, Vec<String>) {
    let live = LiveSnapshot { orchestrator: live, ..Default::default() };
    let view = render(&fixture(), &live, Client::Ios, lang, "node:orchestrator", "");
    let items = |id: &'static str| view.sections.iter().filter(move |section| section.id == id).flat_map(|section| &section.items);
    let facts = items("header").flat_map(|item| match item { Item::Facts(facts) => facts.items.clone(), _ => vec![] }).collect();
    let failure = items("failure").find_map(|item| match item { Item::Markdown(text) => Some(text.text.clone()), _ => None });
    let verbs = items("actions").flat_map(|item| match item { Item::Actions(bar) => bar.actions.clone(), _ => vec![] })
        .map(|action| action.id.split(':').next().unwrap().to_owned()).collect();
    (facts, failure, verbs)
}

#[test]
fn the_orchestrator_screen_says_it_failed_and_why_in_the_users_language() {
    let refused = live::LiveOrchestrator {
        working: false, summary: Some("Claude denied a required tool permission in auto mode".into()),
        failure: Some("denied".into()), queue_depth: 0, active_run_id: Some("run-1".into()), can_retry: true, can_cancel: false,
    };
    let (facts, failure, verbs) = orchestrator_screen(Some(refused), Lang::Es);
    assert_eq!((facts[0].value.as_str(), facts[0].tone), ("Falló", Some(Tone::Warning)));
    let failure = failure.expect("the failure is shown");
    assert!(failure.contains("le negó una de las herramientas de Alondra"), "{failure}");
    assert!(!failure.contains("Claude denied"), "Alondra's English diagnostic is not shown: {failure}");
    assert_eq!(verbs, ["msg", "orch_retry", "orch_ack"]);

    let crashed = live::LiveOrchestrator {
        working: false, summary: Some("exit status: 1".into()), failure: Some("interrupted".into()),
        queue_depth: 0, active_run_id: Some("run-1".into()), can_retry: true, can_cancel: false,
    };
    let failure = orchestrator_screen(Some(crashed), Lang::En).1.unwrap();
    assert!(failure.starts_with("Its last run stopped partway") && failure.ends_with("Detail: `exit status: 1`"), "{failure}");

    let fine = live::LiveOrchestrator {
        working: false, summary: None, failure: None, queue_depth: 0, active_run_id: None, can_retry: false, can_cancel: false,
    };
    let (facts, failure, verbs) = orchestrator_screen(Some(fine), Lang::Es);
    assert_eq!(facts[0].value, "Inactivo");
    assert_eq!(failure, None);
    assert_eq!(verbs, ["msg"], "a run that finished leaves nothing to decide");
}

#[test]
fn a_request_not_split_yet_says_it_waits_for_the_users_decision() {
    let mut state = fixture();
    state.requests.insert("r-2".into(), Request::new("r-2".into(), "Still splitting".into()));
    let subtitle = |live: &LiveSnapshot| {
        let view = render(&state, live, Client::Ios, Lang::Es, "tasks", "");
        view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
            Item::Row(row) if row.id == "r-2" => row.subtitle.clone(),
            _ => None,
        }).unwrap()
    };
    assert_eq!(subtitle(&orchestrator_waiting(true)), "Esperando tu decisión");
    assert_eq!(subtitle(&orchestrator_waiting(false)), "El orquestador la está dividiendo");
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
fn request_clarification_is_behind_more_and_allows_a_blank_note() {
    let state = fixture();
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    let card = card_of(&view);
    assert!(card.actions.iter().all(|action| !action.id.starts_with("clarify:")), "it never takes a visible button");
    let action = card
        .more
        .iter()
        .find(|action| action_id::ActionId::parse(&action.id).is_ok_and(|id| id.verb == action_id::Verb::Clarify))
        .expect("an agent's card can be sent back to be clarified");
    assert_eq!(action.label, "Request clarification");
    assert!(matches!(action.input, Input::Text { required: false, .. }));
    assert_eq!(action.options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["unclear", "contradicts", "missing_context"]);
    let spanish = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    assert!(card_of(&spanish).more.iter().any(|action| action.label == "Pedir aclaración"));
}

#[test]
fn stacked_pr_merge_names_upstream_number_and_keeps_canonical_link() {
    use crate::domain::{DeliveryTarget, MergeMethod};
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
    assert!(
        !card.actions.iter().any(|action| {
            action_id::ActionId::parse(&action.id).is_ok_and(|id| id.verb == action_id::Verb::Merge)
        }),
        "a PR stacked on an open one has nothing to merge yet"
    );
    assert!(view.sections.iter().flat_map(|section| &section.items).any(|item| matches!(item, Item::Facts(facts)
        if facts.items.iter().any(|fact| fact.value == "#153"
            && matches!(&fact.link, Some(Link::View { key }) if key == "pull_request:github.com/bitomule/alondra#153")))));
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
        last_message_at: Some("2026-09-30T09:58:00Z".into()), ..Default::default()
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
    assert!(node.children.iter().any(|child| child.id == "activity"
        && child.title == "Ready for review"
        && matches!(&child.trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:58:00Z")));
    let search = render(&state, &live, Client::Ios, Lang::En, "search?q=Node", "");
    let Item::Row(result) = &search.sections[0].items[0] else { panic!("search result") };
    assert!(result.children.iter().any(|child| child.id == "activity"));
}

#[test]
fn a_node_shows_the_model_it_runs_on_in_its_row_and_its_detail() {
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode { agent_id: "a-1".into(), name: "Build a feature".into(), model: Some("claude-opus-5-5[1m]".into()), ..Default::default() });
    let fleet = render(&state, &live, Client::Ios, Lang::En, "fleet", "");
    let row = fleet.sections.iter().flat_map(|section| &section.items)
        .find_map(|item| if let Item::Row(row) = item && row.id == "a-1" { Some(row.clone()) } else { None }).unwrap();
    assert!(row.badges.iter().any(|badge| badge.text == "Opus 5.5"));
    let detail = render(&state, &live, Client::Ios, Lang::Es, "node:a-1", "");
    let facts: Vec<_> = detail.sections.iter().flat_map(|section| &section.items)
        .flat_map(|item| match item { Item::Facts(facts) => facts.items.clone(), _ => vec![] }).collect();
    assert!(facts.iter().any(|fact| fact.label == "Modelo" && fact.value == "Opus 5.5"));
}

fn working_node() -> LiveNode {
    LiveNode {
        agent_id: "a-1".into(), name: "Build a feature".into(), working: true,
        steps: vec![
            LiveStep { subject: "Read the code".into(), state: StepState::Completed, active_form: None },
            LiveStep { subject: "Write the fix".into(), state: StepState::Completed, active_form: None },
            LiveStep { subject: "Run the tests".into(), state: StepState::InProgress, active_form: Some("Running the tests".into()) },
            LiveStep { subject: "Open the PR".into(), state: StepState::Pending, active_form: None },
            LiveStep { subject: "Reply".into(), state: StepState::Pending, active_form: None },
        ],
        subagents: vec![
            LiveRun { label: "Explore the parser".into(), kind: Some("Explore".into()), model: Some("claude-haiku-4-5".into()),
                since: "2026-09-30T09:58:00Z".into(), ..Default::default() },
            LiveRun { label: "Review the diff".into(), kind: Some("general-purpose".into()), since: "2026-09-30T09:40:00Z".into(),
                ran_secs: Some(125), ..Default::default() },
        ],
        workflows: vec![LiveRun { label: "review-loop".into(), agents: Some(4), since: "2026-09-30T09:50:00Z".into(), suspect: true, ..Default::default() }],
        ..Default::default()
    }
}

fn node_work_rows<'a>(view: &'a View, id: &str) -> Vec<&'a Row> {
    view.sections.iter().filter(|section| section.id == id).flat_map(|section| &section.items)
        .filter_map(|item| if let Item::Row(row) = item { Some(row) } else { None }).collect()
}

fn section_facts(view: &View, id: &str) -> Vec<(String, String)> {
    view.sections.iter().filter(|section| section.id == id).flat_map(|section| &section.items)
        .flat_map(|item| match item { Item::Facts(facts) => facts.items.clone(), _ => vec![] })
        .map(|fact| (fact.label, fact.value)).collect()
}

fn badges(row: &Row) -> Vec<&str> {
    row.badges.iter().map(|badge| badge.text.as_str()).collect()
}

#[test]
fn a_node_detail_shows_its_task_list_its_subagents_and_its_workflows() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let mut live = LiveSnapshot::default();
    live.nodes.push(working_node());
    let view = render(&state, &live, Client::Ios, Lang::En, "node:a-1", "");
    assert_eq!(section_facts(&view, "steps"), [("Progress".into(), "2 of 5".into()), ("Now".into(), "Running the tests".into())]);
    let steps = node_work_rows(&view, "steps");
    assert_eq!(steps.iter().map(|row| (row.title.as_str(), row.struck)).collect::<Vec<_>>(),
        [("Read the code", true), ("Write the fix", true), ("Run the tests", false), ("Open the PR", false), ("Reply", false)]);
    assert_eq!(badges(steps[2]), ["In progress"]);
    assert!(badges(steps[3]).is_empty());

    let subagents = node_work_rows(&view, "subagents");
    assert_eq!(subagents[0].subtitle.as_deref(), Some("Explore · Haiku 4.5"));
    assert_eq!(badges(subagents[0]), ["Running"]);
    assert!(matches!(&subagents[0].trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:58:00Z"));
    assert_eq!(subagents[1].subtitle.as_deref(), Some("general-purpose"));
    assert_eq!(badges(subagents[1]), ["Finished"]);
    assert!(matches!(&subagents[1].trailing, Some(Trailing::Text { text }) if text == "2m 05s"));

    let workflows = node_work_rows(&view, "workflows");
    assert_eq!((workflows[0].title.as_str(), workflows[0].subtitle.as_deref()), ("review-loop", Some("4 agents")));
    assert_eq!(badges(workflows[0]), ["Running", "Possibly hung"]);

    let spanish = render(&state, &live, Client::Ios, Lang::Es, "node:a-1", "");
    assert_eq!(section_facts(&spanish, "steps"), [("Progreso".into(), "2 de 5".into()), ("Ahora".into(), "Running the tests".into())]);
    assert_eq!(spanish.sections.iter().filter_map(|section| section.title.as_deref())
        .filter(|title| ["Pasos", "Subagentes", "Flujos de trabajo"].contains(title)).count(), 3);
    assert_eq!(badges(node_work_rows(&spanish, "steps")[2]), ["En curso"]);
    assert_eq!(badges(node_work_rows(&spanish, "subagents")[1]), ["Terminado"]);
    assert_eq!(node_work_rows(&spanish, "workflows")[0].subtitle.as_deref(), Some("4 agentes"));

    let quiet = render(&state, &LiveSnapshot { nodes: vec![LiveNode { agent_id: "a-1".into(), ..Default::default() }], ..Default::default() },
        Client::Ios, Lang::En, "node:a-1", "");
    assert!(quiet.sections.iter().all(|section| !["steps", "subagents", "workflows"].contains(&section.id.as_str())));
}

#[test]
fn the_task_a_node_works_shows_the_nodes_progress_and_a_finished_task_does_not() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    let mut live = LiveSnapshot::default();
    live.nodes.push(working_node());
    let view = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    assert_eq!(section_facts(&view, "steps")[0], ("Progreso".into(), "2 de 5".into()));
    assert_eq!(node_work_rows(&view, "subagents").len(), 2);
    assert_eq!(node_work_rows(&view, "workflows").len(), 1);
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    let done = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    assert!(done.sections.iter().all(|section| section.id != "steps"));
}

fn workflow_agent(label: &str, phase: &str, model: Option<&str>, running: bool) -> LiveWorkflowAgent {
    LiveWorkflowAgent { label: label.into(), phase: Some(phase.into()), model: model.map(Into::into), running }
}

#[test]
fn a_running_workflow_shows_its_phase_its_agents_and_how_many_more_there_are() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    let mut node = working_node();
    let mut agents = vec![workflow_agent("review-auth", "Review", Some("haiku"), true),
        workflow_agent("review-db", "Review", Some("claude-sonnet-5-5"), false)];
    agents.extend((0..6).map(|index| workflow_agent(&format!("scan-{index}"), "Scan", None, false)));
    node.workflows[0].agents = Some(10);
    node.workflows[0].workflow = Some(LiveWorkflow {
        phase: Some("Review".into()), phase_index: Some(2), phase_count: Some(4), running: 1, done: 9, agents, ..Default::default()
    });
    let live = LiveSnapshot { nodes: vec![node], ..Default::default() };
    for view in ["node:a-1", "task:t-1"] {
        let english = render(&state, &live, Client::Ios, Lang::En, view, "");
        assert_eq!(section_facts(&english, "workflows"), [("Phase".into(), "Review (2 of 4)".into()), ("Agents".into(), "1 running · 9 done".into())]);
        let rows = node_work_rows(&english, "workflows");
        assert_eq!(rows.len(), 1 + 8 + 1, "{view}");
        assert_eq!((rows[1].title.as_str(), rows[1].subtitle.as_deref(), badges(rows[1])), ("review-auth", Some("Review · Haiku"), vec!["Running"]));
        assert_eq!((rows[2].subtitle.as_deref(), badges(rows[2])), (Some("Review · Sonnet 5.5"), vec!["Done"]));
        assert_eq!(rows[3].subtitle.as_deref(), Some("Scan"));
        assert_eq!(rows[9].title, "+2 more");
        assert!(rows.iter().all(|row| row.link.is_none()));
    }

    let spanish = render(&state, &live, Client::Ios, Lang::Es, "node:a-1", "");
    assert_eq!(section_facts(&spanish, "workflows"), [("Fase".into(), "Review (2 de 4)".into()), ("Agentes".into(), "1 en marcha · 9 terminados".into())]);
    let rows = node_work_rows(&spanish, "workflows");
    assert_eq!((badges(rows[1]), badges(rows[2]), rows[9].title.as_str()), (vec!["En marcha"], vec!["Hecho"], "+2 más"));

    let mut finished = live.clone();
    let workflow = finished.nodes[0].workflows[0].workflow.as_mut().unwrap();
    *workflow = LiveWorkflow { phase: Some("Second".into()), running: 0, done: 3, total_tokens: Some(88_896),
        agents: vec![workflow_agent("count-c", "Second", Some("claude-haiku-4-5-20251001"), false)], ..Default::default() };
    let view = render(&state, &finished, Client::Ios, Lang::En, "node:a-1", "");
    assert_eq!(section_facts(&view, "workflows"), [("Phase".into(), "Second".into()), ("Agents".into(), "0 running · 3 done".into()), ("Tokens".into(), "88K".into())]);
    assert_eq!(node_work_rows(&view, "workflows").last().unwrap().title, "+2 more");
}

#[test]
fn a_fleet_row_shows_how_far_the_task_list_has_got_until_every_step_is_done() {
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let mut live = LiveSnapshot::default();
    live.nodes.push(working_node());
    for (client, lang) in [(Client::Ios, Lang::En), (Client::Tui, Lang::Es)] {
        let fleet = render(&state, &live, client, lang, "fleet", "");
        let row = fleet.sections.iter().flat_map(|section| &section.items)
            .find_map(|item| if let Item::Row(row) = item && row.id == "a-1" { Some(row.clone()) } else { None }).unwrap();
        assert!(row.badges.iter().any(|badge| badge.text == "2/5"), "{:?}", row.badges);
    }
    for step in &mut live.nodes[0].steps { step.state = StepState::Completed; }
    let fleet = render(&state, &live, Client::Ios, Lang::En, "fleet", "");
    let row = fleet.sections.iter().flat_map(|section| &section.items)
        .find_map(|item| if let Item::Row(row) = item && row.id == "a-1" { Some(row.clone()) } else { None }).unwrap();
    assert!(row.badges.iter().all(|badge| !badge.text.contains('/')));
}

#[test]
fn a_task_detail_says_the_model_it_was_launched_with_and_who_chose_it() {
    let mut state = fixture();
    let model = |state: &DomainState, lang| {
        let view = render(state, &LiveSnapshot::default(), Client::Ios, lang, "task:t-1", "");
        view.sections.iter().flat_map(|section| &section.items)
            .flat_map(|item| match item { Item::Facts(facts) => facts.items.clone(), _ => vec![] })
            .find(|fact| fact.label == "Model" || fact.label == "Modelo").map(|fact| (fact.label, fact.value))
    };
    assert_eq!(model(&state, Lang::En), None, "a task launched before models were recorded shows none");
    for (chosen_by, en, es) in [
        (ModelChooser::Orchestrator, "Sonnet · chosen by the orchestrator", "Sonnet · elegido por el orquestador"),
        (ModelChooser::User, "Sonnet · from your Effort choice", "Sonnet · por tu elección de esfuerzo"),
    ] {
        state.tasks.get_mut(&"t-1".into()).unwrap().model = Some(TaskModel { model: "sonnet".into(), chosen_by });
        assert_eq!(model(&state, Lang::En), Some(("Model".into(), en.into())));
        assert_eq!(model(&state, Lang::Es), Some(("Modelo".into(), es.into())));
    }
    state.tasks.get_mut(&"t-1".into()).unwrap().model = Some(TaskModel { model: "claude-opus-5-5".into(), chosen_by: ModelChooser::User });
    assert_eq!(model(&state, Lang::En).unwrap().1, "Opus 5.5 · from your Effort choice");
}

#[test]
fn an_ios_fleet_item_is_the_node_and_its_one_task_without_readings() {
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    state.agents.get_mut(&"a-1".into()).unwrap().name = "Build a feature".into();
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode {
        agent_id: "a-1".into(), name: "Build a feature".into(), working: true,
        tool: Some("cargo test".into()), state_since: Some("2026-09-30T09:55:00Z".into()),
        last_message: Some("Earlier note".into()), last_message_at: Some("2026-09-30T09:50:00Z".into()),
        context_percent: Some(6), context_window: Some(1_000_000), cost_cents: Some(27), last_turn_aborted: true,
        background_badge: Some("2 subagents".into()),
        ..Default::default()
    });
    let item = |state: &DomainState, live: &LiveSnapshot| {
        render(state, live, Client::Ios, Lang::Es, "fleet", "").sections.iter().flat_map(|section| &section.items)
            .find_map(|item| if let Item::Row(row) = item && row.id == "a-1" { Some(row.clone()) } else { None }).unwrap()
    };
    let working = item(&state, &live);
    assert_eq!(working.title, "Build a feature");
    assert_eq!(working.subtitle, None, "a node named after its task does not repeat the title");
    assert_eq!(working.badges.iter().map(|badge| badge.text.as_str()).collect::<Vec<_>>(), ["Trabajando", "2 subagents"],
        "context, cost and interruptions stay on the node's screen");
    assert_eq!(working.children.len(), 1, "the task is the item, not a row under it");
    assert_eq!(working.children[0].title, "cargo test");
    assert!(matches!(&working.children[0].trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:55:00Z"));
    let verbs = |row: &Row| row.actions.iter().map(|action| action.id.split(':').next().unwrap().to_owned()).collect::<Vec<_>>();
    assert_eq!(verbs(&working), ["interrupt", "close"], "a working node can be interrupted or closed from its card");
    let interrupt = &working.actions[0];
    assert_eq!((interrupt.label.as_str(), interrupt.style), ("Interrumpir", Style::Secondary));
    assert!(interrupt.confirm.is_some());

    live.nodes[0].working = false;
    state.agents.get_mut(&"a-1".into()).unwrap().name = "alondra-ios".into();
    let resting = item(&state, &live);
    assert_eq!(resting.subtitle.as_deref(), Some("alondra-ios"), "a node name that adds something stays");
    assert_eq!(resting.children[0].title, "Earlier note");
    assert_eq!(verbs(&resting), ["close"], "nothing to interrupt once its turn ended");
    let detail = render(&state, &live, Client::Ios, Lang::En, "node:a-1", "");
    let Item::Actions(bar) = &detail.sections.iter().find(|section| section.id == "actions").unwrap().items[0] else { panic!("actions") };
    assert!(bar.actions.iter().all(|action| !action.id.starts_with("interrupt:")));
    assert!(matches!(&resting.children[0].trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:50:00Z"));

    let tui = render(&state, &live, Client::Tui, Lang::En, "fleet", "");
    let tui_row = tui.sections.iter().flat_map(|section| &section.items)
        .find_map(|item| if let Item::Row(row) = item && row.id == "a-1" { Some(row) } else { None }).unwrap();
    assert!(tui_row.badges.iter().all(|badge| !badge.text.contains("Context") && !badge.text.contains('$')));
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
fn a_task_waiting_on_a_question_says_so_in_its_state_and_its_row_without_a_pr_ahead() {
    let mut state = golden_fixture();
    let question = "8fa4034b-249e-4487-a89b-57de38c26372";
    state.interactions.get_mut(&question.into()).unwrap().card.blocks =
        vec![crate::domain::CardTarget::PullRequest { pr_id: "github.com/example/repo#12".into() }];
    let task = state.pull_requests[&"github.com/example/repo#12".into()].task_id.clone();
    state.tasks.get_mut(&task).unwrap().state = TaskState::WaitingDependency;
    assert!(matches!(state.attention(&state.tasks[&task]), crate::domain::Attention::Wait(crate::domain::WaitReason::Answer { .. })));
    for (lang, expected) in [
        (Lang::En, "Waits for your answer to “Ship the compact schedule editor with this beta?”"),
        (Lang::Es, "Espera tu respuesta a «Ship the compact schedule editor with this beta?»"),
    ] {
        let view = render(&state, &golden_live(), Client::Ios, lang, &format!("task:{task}"), "2026-09-30T10:00:00Z");
        let texts: Vec<String> = view.sections.iter().flat_map(|section| &section.items).flat_map(|item| match item {
            Item::Facts(facts) => facts.items.iter().map(|fact| fact.value.clone()).collect(),
            _ => vec![],
        }).collect();
        assert!(texts.iter().any(|text| text == expected), "{lang:?}: the state says why it waits: {texts:?}");
    }
}

/// §7.1: whatever the state, no card has an action the domain would turn down. Every action a
/// card carries is compared with what the domain says is available for that very card.
#[test]
fn no_card_of_any_fixture_carries_an_action_the_domain_says_is_not_available() {
    use action_id::{ActionId, Verb};
    let mut compared = 0;
    for domain in [fixture(), golden_fixture(), plan_question_fixture()] {
        let domain = with_cards(&domain);
        let live = golden_live();
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            for lang in [Lang::En, Lang::Es] {
                let context = RenderContext { domain: &domain, live: &live, client, lang, transport: Transport::Loopback,
                    cursor: "cursor-1", rendered_at: "" };
                for item in domain.projection().inbox {
                    let card = card::from_item(&item, &context, true).unwrap();
                    for action in card.actions.iter().chain(&card.more).chain(&card.reply) {
                        let Ok(id) = ActionId::parse(&action.id) else { continue };
                        let kinds: &[&str] = match id.verb {
                            Verb::Accept => &["accept_delivery", "answer_question"],
                            Verb::Merge => &["authorize_merge"],
                            Verb::Changes => &["submit_feedback", "answer_question"],
                            Verb::Ans => &["answer_question"],
                            Verb::Read => &["mark_read"],
                            Verb::Clarify => &["request_clarification"],
                            _ => continue,
                        };
                        compared += 1;
                        assert!(
                            item.available_actions.iter().any(|available| available.enabled && kinds.contains(&available.kind.as_str())),
                            "{} carries {} but the domain has none of {kinds:?} available: {:?}", item.id, action.id, item.available_actions
                        );
                    }
                }
            }
        }
    }
    assert!(compared > 100, "the property compared {compared} actions");
}

#[test]
fn task_detail_has_no_now_line_while_the_node_just_works() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "task:t-1", "2026-09-30T10:00:00Z");
    assert!(view.sections.iter().all(|section| section.id != "now"));
}

#[test]
fn node_detail_names_its_state_and_since_when() {
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
        last_message_at: None, ..Default::default()
    });
    let view = render(&state, &live, Client::Ios, Lang::Es, "node:a-1", "2026-09-30T10:00:00Z");
    let Item::Facts(facts) = &view.sections.iter().find(|section| section.id == "header").unwrap().items[0] else {
        panic!("header facts")
    };
    assert_eq!(facts.items[0].value, "Esperando respuesta");
    let since = view.sections.iter().find(|section| section.id == "since").expect("since section");
    let Item::Row(row) = &since.items[0] else { panic!("since row") };
    assert!(matches!(&row.trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:55:00Z"));
}

#[test]
fn node_detail_without_live_data_names_its_state_not_its_connection() {
    let mut state = fixture();
    state.agents.get_mut(&"a-1".into()).unwrap().connected = false;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "node:a-1", "2026-09-30T10:00:00Z");
    let Item::Facts(facts) = &view.sections.iter().find(|section| section.id == "header").unwrap().items[0] else {
        panic!("header facts")
    };
    assert_eq!(facts.items[0].value, "Waiting for an answer");
    assert!(view.sections.iter().all(|section| section.id != "since"));
}

#[test]
fn every_card_can_be_replied_to_in_text_with_attachments() {
    let state = fixture();
    for id in ["i-1", "i-2", "i-3", "i-4"] {
        let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, &format!("card:{id}"), "2026-09-30T10:00:00Z");
        let Item::Card(card) = &view.sections[0].items[0] else { panic!("card") };
        let reply = card.reply.as_ref().unwrap_or_else(|| panic!("{id} has a reply"));
        assert!(matches!(&reply.input, Input::Text { required: true, multiline: true, .. }), "{id}");
        assert!(reply.attachments, "{id}");
        assert!(reply.enabled, "{id}");
    }
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-2", "");
    let Item::Card(information) = &view.sections[0].items[0] else { panic!("card") };
    assert!(information.reply.as_ref().unwrap().id.starts_with("reply:i-2@"), "a task's card replies to its node");
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-4", "");
    let Item::Card(orchestrator) = &view.sections[0].items[0] else { panic!("card") };
    assert!(orchestrator.reply.as_ref().unwrap().id.starts_with("msg:view@"), "the orchestrator's card replies to the orchestrator");
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
        if let Item::Card(card) = item { Some(card.id.as_str()) } else { None }
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
fn the_pending_list_shows_only_requests_the_orchestrator_has_not_handled() {
    let pending = |state: &DomainState| -> Vec<(String, Vec<String>)> {
        let view = render(state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "new_request", "");
        let section = view.sections.iter().find(|section| section.id == "sent").unwrap();
        assert_eq!(section.title.as_deref(), Some("Solicitudes pendientes"));
        section.items.iter().filter_map(|item| {
            if let Item::Row(row) = item { Some((row.id.clone(), row.badges.iter().map(|badge| badge.text.clone()).collect())) } else { None }
        }).collect()
    };
    let mut state = fixture();
    let mut answered = Request::new("r-answered".into(), "What is left?".into());
    answered.closed = true;
    state.requests.insert("r-answered".into(), answered);
    let mut withdrawn = Request::new("r-withdrawn".into(), "Never mind".into());
    (withdrawn.closed, withdrawn.withdrawn) = (true, true);
    state.requests.insert("r-withdrawn".into(), withdrawn);
    state.requests.insert("r-new".into(), Request::new("r-new".into(), "Fix the login bug".into()));
    assert_eq!(pending(&state), [("r-new".to_string(), vec!["En cola".to_string()])]);

    let mut defined = state.tasks[&"t-1".into()].clone();
    (defined.id, defined.request_id, defined.state) = ("t-2".into(), "r-new".into(), TaskState::NotStarted);
    state.tasks.insert("t-2".into(), defined);
    assert!(pending(&state).is_empty(), "defining a request into tasks takes it out of the queue");

    state.tasks.remove(&"t-2".into());
    state.requests.get_mut(&"r-new".into()).unwrap().closed = true;
    assert!(pending(&state).is_empty(), "closing a request takes it out of the queue");
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
        "Alondra", "Alondra · iOS", "Alondra · iOS · Demo Garden", "Demo Garden", "PRs", "squash", "rebase", "Polish the Alondra beta", "Restore the native review experience", "PR #12", "1 · PR #12", "example/repo #12", "Base",
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
        "Choose the welcome copy", "Concise:", "- Swipe", "Review the beta candidate", "Ship the compact schedule editor",
        "Swipe navigation, inline", "The pull request checks passed", "The review of the pull request",
        "The simulator pass is clean", "★ Ship the concise copy", "Beta candidate ready",
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
        let run: crate::domain::ScheduleRun = serde_json::from_value(serde_json::json!({
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

fn section_of<'a>(view: &'a View, id: &str) -> Option<&'a super::Section> {
    view.sections.iter().find(|section| section.id == id)
}

fn content_checks(view: &View) -> Option<&Checks> {
    section_of(view, "checks")?.items.iter().find_map(|item| if let Item::Checks(checks) = item { Some(checks) } else { None })
}

#[test]
fn a_pull_request_detail_puts_the_diff_before_the_folded_description_and_drops_the_table() {
    let view = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En, "card:github.com/example/repo#12", "");
    let ids = view.sections.iter().map(|section| section.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["card", "checks", "diff", "description", "context"]);
    let diff = section_of(&view, "diff").unwrap();
    assert!(matches!(&diff.items[..], [Item::ContentRef(reference)] if reference.kind == super::ContentKind::Diff));
    let description = section_of(&view, "description").unwrap();
    assert!(matches!(&description.items[..], [Item::Markdown(body)] if body.text.starts_with("Replaces the long schedule form")));
    assert!(view.sections.iter().flat_map(|section| &section.items).all(|item| !matches!(item, Item::Facts(facts)
        if facts.items.iter().any(|fact| fact.label == "Repository" || fact.label == "Base" || fact.label == "Head"))));
}

#[test]
fn a_pull_request_delivery_carries_checks_branch_and_change_size_in_its_detail() {
    let card_key = "card:github.com/example/repo#12";
    let state = golden_fixture();
    let view = render(&state, &golden_live(), Client::Ios, Lang::En, card_key, "");
    let card = card_of(&view);
    assert_eq!(card.kind, Some(crate::domain::CardKind::Review));
    assert_eq!(card.eyebrow.as_ref().unwrap().text, "Review · PR #12");
    assert_eq!(card.icon, Icon::PullRequest);
    assert_eq!(card.need.as_deref(), Some("Review the beta candidate"));
    assert_eq!(card.title, "Add the compact schedule editor", "the detail is titled with the PR's own title");
    assert_eq!(
        card.meta.as_deref(),
        Some("example/repo · feature/compact-schedule-editor → main · 4 files · +120 −30 · Checks passed")
    );
    assert!(card.body.is_none(), "the card holds none of it");
    let row = card.content_row.as_ref().expect("content row");
    assert_eq!((row.kind.as_str(), row.label.as_str(), row.summary.as_str()), ("diff", "Changes", "4 files · +120 −30"));
    assert_eq!(
        content_checks(&view).unwrap().items.iter().map(|check| (check.name.as_str(), check.state)).collect::<Vec<_>>(),
        [("Build", CheckState::Pass), ("Unit tests", CheckState::Pass), ("Lint", CheckState::Pending)]
    );
    let spanish = render(&state, &golden_live(), Client::Ios, Lang::Es, card_key, "");
    assert_eq!(card_of(&spanish).content_row.as_ref().unwrap().summary, "4 archivos · +120 −30");
    assert_eq!(
        card_of(&spanish).meta.as_deref(),
        Some("example/repo · feature/compact-schedule-editor → main · 4 archivos · +120 −30 · Comprobaciones superadas")
    );
    let mut single = golden_live();
    single.pull_requests[0].changed_files = 1;
    let view = render(&state, &single, Client::Ios, Lang::En, card_key, "");
    assert!(card_of(&view).meta.as_deref().unwrap().contains("1 file · +120 −30"));
}

fn reviewed_fixture(verdict: crate::domain::Verdict) -> DomainState {
    reviewed_fixture_after(verdict, true)
}

/// `saw_this_delivery` false puts the reviewer's result before the card's delivery, as when the
/// reviewed task delivered again after the review.
fn reviewed_fixture_after(verdict: crate::domain::Verdict, saw_this_delivery: bool) -> DomainState {
    let mut state = golden_fixture();
    let delivery = state.interactions[&"hf-weekly-2026-09-30".into()].clone();
    let mut result = delivery.clone();
    result.id = "reviewer-result".into();
    result.order = if saw_this_delivery { delivery.order + 1000 } else { 0 };
    state.interactions.insert(result.id.clone(), result);
    let template = state.tasks[&"t-651c38484cd2f34e74d5107ba05f7f15".into()].clone();
    let mut reviewer = template;
    reviewer.id = "t-reviewer".into();
    reviewer.title = "Review the editor".into();
    reviewer.state = TaskState::Done;
    reviewer.final_presentation = Some("reviewer-result".into());
    reviewer.pr_id = None;
    reviewer.agent_id = None;
    reviewer.blocks = std::collections::BTreeSet::from(["t-651c38484cd2f34e74d5107ba05f7f15".into()]);
    reviewer.verdict = Some(verdict);
    state.tasks.insert(reviewer.id.clone(), reviewer);
    state
}

fn context_texts(state: &DomainState, lang: Lang) -> Vec<String> {
    let view = render(state, &golden_live(), Client::Ios, lang, "card:hf-weekly-2026-09-30", "");
    card_of(&view).context.iter().map(|line| line.text.clone()).collect()
}

#[test]
fn the_card_of_a_reviewed_task_carries_the_line_of_its_reviewer() {
    let state = reviewed_fixture(crate::domain::Verdict::Approved { summary: "4 fixed".into(), open_issues: false });
    assert!(context_texts(&state, Lang::En).contains(&"Reviewed by “Review the editor”: 4 fixed".to_owned()));
    assert!(context_texts(&state, Lang::Es).contains(&"Revisado por «Review the editor»: 4 fixed".to_owned()));
}

#[test]
fn a_new_delivery_after_the_review_carries_no_line_of_that_review() {
    let state = reviewed_fixture_after(crate::domain::Verdict::Approved { summary: "4 fixed".into(), open_issues: false }, false);
    assert!(!context_texts(&state, Lang::En).iter().any(|line| line.starts_with("Reviewed by")), "the review saw an older delivery");
}

#[test]
fn one_failure_left_is_said_in_the_singular_in_both_languages() {
    let state = reviewed_fixture(crate::domain::Verdict::Approved { summary: "Empty list crashes".into(), open_issues: true });
    assert!(context_texts(&state, Lang::En).contains(&"Reviewed by “Review the editor”: 1 failure left after 3 rounds".to_owned()));
    assert!(context_texts(&state, Lang::Es).contains(&"Revisado por «Review the editor»: queda 1 fallo tras 3 vueltas".to_owned()));
}

#[test]
fn a_delivery_held_by_a_question_is_grouped_and_counted_as_waiting_never_as_in_review() {
    let mut state = golden_fixture();
    let question = "8fa4034b-249e-4487-a89b-57de38c26372";
    let asked = state.interactions.get_mut(&question.into()).unwrap();
    asked.card.blocks = vec![crate::domain::CardTarget::PullRequest { pr_id: "github.com/example/repo#12".into() }];
    asked.blocking = false;
    for other in state.interactions.values_mut() {
        other.blocking = false;
    }
    state.derive_states();
    let task = "t-651c38484cd2f34e74d5107ba05f7f15";
    assert_eq!(state.tasks[&task.into()].state, TaskState::WaitingDependency);
    let sidebar = render(&state, &golden_live(), Client::Mac, Lang::Es, "sidebar", "");
    let group_of = |id: &str| sidebar.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
        Item::Row(row) if row.id == id => Some(row.children.iter().map(|child| child.id.clone()).collect::<Vec<_>>()),
        _ => None,
    }).unwrap_or_default();
    assert_eq!(group_of("waiting_dependency"), [task.to_owned()], "grouped as waiting");
    assert!(group_of("in_review").is_empty(), "not as in review");
    let detail = render(&state, &golden_live(), Client::Ios, Lang::Es, &format!("task:{task}"), "");
    let texts: Vec<String> = detail.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        Item::Row(row) => Some(row.title.clone()), _ => None,
    }).collect();
    assert!(texts.iter().any(|text| text.starts_with("Espera tu respuesta a «")), "the detail says why it waits: {texts:?}");
}

#[test]
fn a_review_that_ran_out_of_rounds_says_how_many_failures_are_left_and_lists_them() {
    let state = reviewed_fixture(crate::domain::Verdict::Approved { summary: "Empty list crashes\nWrong total".into(), open_issues: true });
    let en = context_texts(&state, Lang::En);
    assert!(en.contains(&"Reviewed by “Review the editor”: 2 failures left after 3 rounds".to_owned()), "{en:?}");
    assert!(en.contains(&"Empty list crashes\nWrong total".to_owned()), "the failures are in the detail: {en:?}");
    assert!(context_texts(&state, Lang::Es).contains(&"Revisado por «Review the editor»: quedan 2 fallos tras 3 vueltas".to_owned()));
}

#[test]
fn the_sidebar_groups_a_task_waiting_for_its_reviewer_and_the_history_names_the_new_states() {
    let mut state = golden_fixture();
    let task = "t-651c38484cd2f34e74d5107ba05f7f15";
    state.tasks.get_mut(&task.into()).unwrap().state = TaskState::AwaitingBlocker;
    let mut live = golden_live();
    live.task_history.insert(task.into(), vec![
        LiveHistoryStep { at: "2026-09-30T08:00:00Z".into(), state: "awaiting_blocker".into(), summary: "Delivered".into() },
        LiveHistoryStep { at: "2026-09-30T09:00:00Z".into(), state: "for_your_review".into(), summary: "Reviewed".into() },
    ]);
    let tasks = render(&state, &live, Client::Ios, Lang::Es, &format!("task:{task}"), "");
    let titles: Vec<String> = tasks.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        Item::Row(row) => Some(row.title.clone()), _ => None,
    }).collect();
    assert!(titles.contains(&"Esperando".to_owned()) && titles.contains(&"En revisión".to_owned()), "{titles:?}");
    assert!(!titles.iter().any(|title| title.contains("awaiting_blocker") || title.contains("for_your_review")), "never a raw state name: {titles:?}");
    let sidebar = render(&state, &live, Client::Mac, Lang::Es, "sidebar", "");
    let group = sidebar.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
        Item::Row(row) if row.id == "awaiting_blocker" => Some(row),
        _ => None,
    }).expect("the sidebar lists the tasks waiting for a reviewer as a group");
    assert_eq!((group.badges[0].text.as_str(), group.badges[0].tone), ("Esperando", Tone::Info), "not filed under a made-up state");
    assert_eq!(group.children.len(), 1);
}

#[test]
fn the_task_whose_node_resolves_conflicts_says_so_with_its_pr_in_both_languages() {
    let mut state = golden_fixture();
    let agent = state.agents.values().find(|agent| agent.name == "Alondra · iOS").unwrap().id.clone();
    state.tasks.get_mut(&"t-651c38484cd2f34e74d5107ba05f7f15".into()).unwrap().agent_id = Some(agent);
    let delivery = state.interactions.get_mut(&"hf-weekly-2026-09-30".into()).unwrap();
    if let InteractionPayload::Delivery { feedback, .. } = &mut delivery.payload {
        feedback.push(crate::domain::Feedback {
            attachments: vec![], user_id: crate::domain::Feedback::ALONDRA.into(), body: "Conflicts".into(),
            at_revision: 99, instruction: crate::domain::Instruction::Conflicts,
        });
    }
    for (lang, expected) in [(Lang::En, "“Alondra · iOS” is resolving the conflicts on PR #12"), (Lang::Es, "«Alondra · iOS» está resolviendo los conflictos de la PR #12")] {
        let view = render(&state, &golden_live(), Client::Ios, lang, "task:t-651c38484cd2f34e74d5107ba05f7f15", "");
        let texts: Vec<String> = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
            Item::Row(row) => Some(row.title.clone()), _ => None,
        }).collect();
        assert!(texts.contains(&expected.to_owned()), "{texts:?}");
    }
}

#[test]
fn a_pull_request_without_github_details_keeps_the_aggregate_check_and_names_the_pr() {
    let state = golden_fixture();
    let mut live = golden_live();
    live.pull_requests[0] = LivePullRequest {
        id: "github.com/example/repo#12".into(),
        checks_passed: Some(false),
        ..Default::default()
    };
    let view = render(&state, &live, Client::Ios, Lang::En, "card:github.com/example/repo#12", "");
    let card = card_of(&view);
    assert_eq!(card.content_row.as_ref().unwrap().summary, "example/repo #12");
    assert_eq!(card.meta.as_deref(), Some("example/repo · → main · Checks failing"));
    assert_eq!(card.title, "Review the beta candidate", "without GitHub's title the node's own is kept");
    let checks = content_checks(&view).unwrap();
    assert_eq!(checks.items.len(), 1);
    assert_eq!(checks.items[0].state, CheckState::Fail);
}

#[test]
fn a_pull_request_description_repeats_neither_the_report_nor_the_link_to_the_pr() {
    let mut state = golden_fixture();
    let report = "PR: https://github.com/example/repo/pull/12 (rebased)\n\nReplaces the long schedule form with a compact editor.";
    for interaction in state.interactions.values_mut() {
        if interaction.card.need == "Review the beta candidate" {
            interaction.card.content = Some(crate::domain::CardContent {
                kind: crate::domain::ContentKind::Report,
                summary: None,
                body: report.into(),
            });
        }
    }
    let view = render(&state, &golden_live(), Client::Ios, Lang::En, "card:github.com/example/repo#12", "");
    let description = section_of(&view, "description").unwrap();
    let [Item::Markdown(body)] = &description.items[..] else { panic!("one folded text: {:?}", description.items) };
    assert!(!body.text.contains("/pull/12"), "{}", body.text);
    assert!(body.text.starts_with("Replaces the long schedule form"), "{}", body.text);
    assert_eq!(body.text.matches("Replaces the long schedule form").count(), 1, "{}", body.text);
}

#[test]
fn internal_task_ids_in_a_card_read_as_the_task_title() {
    let mut state = golden_fixture();
    let (id, title) = {
        let task = state.tasks.values().next().unwrap();
        (task.id.0.clone(), task.title.clone())
    };
    for interaction in state.interactions.values_mut() {
        if interaction.card.need == "Review the beta candidate" {
            interaction.card.why = format!("Closes {id} and unblocks t-unknown-task.");
        }
    }
    let view = render(&state, &golden_live(), Client::Ios, Lang::En, "card:github.com/example/repo#12", "");
    assert_eq!(card_of(&view).why.as_deref(), Some(format!("Closes {title} and unblocks t-unknown-task.").as_str()));
}

#[test]
fn a_task_and_node_with_the_same_name_appear_once_in_the_context() {
    let mut state = golden_fixture();
    let title = state.tasks.values().next().unwrap().title.clone();
    assert!(!state.agents.is_empty());
    for agent in state.agents.values_mut() {
        agent.name = title.clone();
    }
    let view = render(&state, &golden_live(), Client::Ios, Lang::En, "card:github.com/example/repo#12", "");
    let Some(Item::Facts(context)) = section_of(&view, "context").and_then(|section| section.items.first()) else {
        panic!("context line")
    };
    assert_eq!(context.items[0].value.matches(&title).count(), 1, "{}", context.items[0].value);
}

#[test]
fn merge_is_not_offered_until_the_card_that_blocks_it_is_answered() {
    let mut state = golden_fixture();
    let key = "card:hf-weekly-2026-09-30";
    let question = "8fa4034b-249e-4487-a89b-57de38c26372";
    state.interactions.get_mut(&question.into()).unwrap().card.blocks =
        vec![crate::domain::CardTarget::PullRequest { pr_id: "github.com/example/repo#12".into() }];
    let view = render(&state, &golden_live(), Client::Ios, Lang::En, key, "");
    let card = card_of(&view);
    assert!(!card.actions.iter().any(|action| action.id.starts_with("merge:")), "a blocked merge is not a button");
    assert!(!card.actions.iter().chain(&card.more).any(|action| action.id.starts_with("accept:")), "accepting is held back too");
    assert_eq!(card.blocked_by.len(), 1);
    assert_eq!(card.blocked_by[0].id, question);
    assert!(card.blocked_by[0].label.starts_with("blocked by Ship the compact schedule editor"));
    assert!(matches!(&card.blocked_by[0].link, Some(Link::View { key }) if key == &format!("card:{question}")));
    let spanish = render(&state, &golden_live(), Client::Ios, Lang::Es, key, "");
    assert!(!card_of(&spanish).actions.iter().any(|action| action.id.starts_with("merge:")));

    let blocker = render(&state, &golden_live(), Client::Ios, Lang::En, &format!("card:{question}"), "");
    let chip = &card_of(&blocker).blocks[0];
    assert_eq!((chip.id.as_str(), chip.label.as_str()), ("hf-weekly-2026-09-30", "blocks PR #12"));

    if let InteractionPayload::Question { answer, .. } = &mut state.interactions.get_mut(&question.into()).unwrap().payload {
        *answer = Some(crate::domain::Answer {
            attachments: vec![], option_id: "ship".into(), inputs: BTreeMap::new(), responder: "u".into(), at_revision: 1,
        });
    }
    let answered = render(&state, &golden_live(), Client::Ios, Lang::En, key, "");
    let card = card_of(&answered);
    assert!(card.blocked_by.is_empty(), "an answered card holds nothing back");
    assert!(card.actions.iter().any(|action| action.id.starts_with("merge:")), "answered, the merge is offered");
}

#[test]
fn every_content_kind_has_a_one_line_row_and_the_card_has_no_body() {
    use crate::domain::{CardKind, ContentKind as Content};
    let row = |content: Option<(Content, &str)>, attachments: usize, options: usize| {
        let mut state = fixture();
        let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
        interaction.payload = InteractionPayload::Question {
            options: (0..options).map(|n| QuestionOption { id: format!("o{n}"), label: format!("Option {n}"), description: None, inputs: vec![] }).collect(),
            answer: None,
        };
        let mut card = spec(CardKind::Decide, "Pick one", "It matters.");
        if let Some((kind, body)) = content {
            card = card.content(kind, body);
        }
        interaction.body = card.why.clone();
        interaction.card = card;
        interaction.attachments = (0..attachments).map(|n| crate::domain::AttachmentRef {
            id: format!("a{n}"), name: format!("shot{n}.png"), content_digest: "d".repeat(64), content_reference: format!("stored/{n}"), bytes: 10,
        }).collect();
        let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
        let card = card_of(&view).clone();
        assert!(card.body.is_none());
        card.content_row
    };
    let summary = |row: Option<ContentRow>| row.map(|row| (row.kind, row.summary, row.more_count));
    assert_eq!(summary(row(Some((Content::Plan, "1. a\n2. b")), 0, 2)), Some(("plan".into(), "2 steps".into(), 0)));
    assert_eq!(summary(row(Some((Content::Command, "alondra update\nmore")), 0, 2)), Some(("command".into(), "alondra update".into(), 0)));
    assert_eq!(summary(row(Some((Content::Report, "- a\n- b\n- c")), 0, 2)), Some(("report".into(), "3 points".into(), 0)));
    assert_eq!(summary(row(Some((Content::Steps, "1. a\n2. b\n3. c\n4. d")), 0, 2)), Some(("steps".into(), "4 steps".into(), 0)));
    assert_eq!(summary(row(Some((Content::Options, "why")), 0, 3)), Some(("options".into(), "3 options".into(), 0)));
    assert_eq!(summary(row(Some((Content::Diagnosis, "Build failed\nlog")), 0, 2)), Some(("diagnosis".into(), "Build failed".into(), 0)));
    assert_eq!(summary(row(Some((Content::Attachments, "")), 3, 2)), Some(("attachments".into(), "3 attachments".into(), 0)));
    assert_eq!(summary(row(Some((Content::Report, "- a\n- b")), 2, 2)), Some(("report".into(), "2 points".into(), 2)),
        "a report with two screenshots behind it shows the report and +2");
    assert_eq!(summary(row(None, 0, 2)), None, "a card with nothing attached has no content row");
    assert_eq!(summary(row(None, 1, 2)), Some(("attachments".into(), "1 attachment".into(), 0)));
    let mut sent = spec(CardKind::Decide, "Pick one", "It matters.").content(Content::Plan, "1. a\n2. b");
    sent.content.as_mut().unwrap().summary = Some("12 pasos del plan".into());
    let mut state = fixture();
    state.interactions.get_mut(&"i-1".into()).unwrap().card = sent;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert_eq!(card_of(&view).content_row.as_ref().unwrap().summary, "12 pasos del plan", "what the agent sent wins");
}

#[test]
fn pull_request_detail_shows_the_change_size() {
    let view = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En,
        "pull_request:github.com/example/repo#12", "");
    let facts = view.sections.iter().find(|section| section.id == "facts").unwrap();
    let Item::Facts(facts) = &facts.items[0] else { panic!("facts") };
    assert!(facts.items.iter().any(|fact| fact.label == "Changes" && fact.value == "4 files · +120 −30"));
}

#[test]
fn card_node_line_carries_the_node_state_next_to_the_project() {
    let view = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En,
        "card:a98d034b-249e-4487-a89b-57de38c26372", "");
    let node = card_of(&view).context.iter().find(|line| line.icon == Icon::Node).unwrap();
    let state = node.state.as_ref().expect("node state");
    assert_eq!((state.text.as_str(), state.tone), ("Waiting for an answer", Tone::Warning));
    assert_eq!(node.chip.as_ref().map(|chip| chip.text.as_str()), Some("Demo Garden"));
    let bare = render(&golden_fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En,
        "card:a98d034b-249e-4487-a89b-57de38c26372", "");
    assert!(card_of(&bare).context.iter().all(|line| line.state.is_none()));
}

#[test]
fn a_command_is_a_code_block_in_the_detail_that_survives_backticks() {
    use crate::domain::{CardKind, ContentKind as Content};
    let mut state = fixture();
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = InteractionPayload::ActionRequest { options: vec![], answer: None };
    interaction.card = spec(CardKind::Do, "Run the commit", "The node cannot sign it.")
        .content(Content::Command, "git commit -m \"```fix```\"\n");
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    let content = view.sections.iter().find(|section| section.id == "content").expect("the detail's content");
    assert!(matches!(&content.items[0], Item::Markdown(body) if body.text == "````\ngit commit -m \"```fix```\"\n````"));
    assert!(card_of(&view).body.is_none());
    assert_eq!(card_of(&view).content_row.as_ref().map(|row| row.summary.as_str()), Some("git commit -m \"```fix```\""));
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
    assert_eq!(pr[1].as_deref(), Some("Alondra merges the PR once its checks pass"));
    assert!(effort.iter().all(Option::is_some));
    let [pr, effort] = descriptions(Lang::Es);
    assert_eq!(pr[1].as_deref(), Some("Alondra fusiona la PR cuando pasan sus comprobaciones"));
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

fn held_fixture(hold: crate::domain::Hold, card: crate::domain::CardSpec) -> DomainState {
    let mut state = fixture();
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = InteractionPayload::Question { options: hold.options(), answer: None };
    interaction.body = card.need.clone();
    interaction.card = card;
    state.holds.insert("i-1".into(), hold);
    state
}

fn spec(kind: crate::domain::CardKind, need: &str, why: &str) -> crate::domain::CardSpec {
    crate::domain::CardSpec::new(kind, need).why(why)
}

fn answers(card: &Card) -> Vec<&CardAction> {
    card.actions.iter().filter(|action| action.id.starts_with("ans:i-1@")).collect()
}

#[test]
fn a_held_permission_card_shows_what_it_would_run_with_allow_and_deny() {
    use crate::domain::{CardKind, ContentKind as Content, Hold};
    let card_spec = spec(CardKind::Authorize, "Allow cleaning the build", "Deletes the build folder.")
        .content(Content::Command, "rm -rf build\nls");
    let state = held_fixture(Hold::Permission, card_spec);
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!((card.icon, card.eyebrow.as_ref().unwrap().text.as_str()), (Icon::Warning, "Autorizar"));
    assert_eq!((card.title.as_str(), card.subtitle.as_deref()), ("Allow cleaning the build", Some("Deletes the build folder.")));
    assert!(card.body.is_none(), "the card holds no body");
    assert_eq!(card.content_row.as_ref().map(|row| (row.kind.as_str(), row.summary.as_str())), Some(("command", "rm -rf build")));
    let buttons = answers(card);
    assert_eq!(buttons.iter().map(|a| (a.option_id.as_deref().unwrap(), a.label.as_str(), a.key.as_deref(), a.role.unwrap())).collect::<Vec<_>>(),
        [("allow", "Permitir", Some("1"), Role::Risk), ("deny", "Denegar", Some("2"), Role::Neutral)]);
    assert!(matches!(&buttons[1].input, Input::Text { placeholder, required: false, .. } if placeholder == "Escribe el motivo"));
    assert!(card.actions.iter().all(|action| !action.id.starts_with("ctx:")), "a held card is answered, not discussed");
    assert!(card.more.iter().all(|action| !action.id.starts_with("clarify:")), "Claude Code waits on it: no clarification");
    assert_eq!(view.status.summary.split(" · ").next(), Some("2 preguntas"));
    let pushed = push_actions(&state, "i-1", Lang::En).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.option.as_deref(), a.label.as_str(), a.text, a.open)).collect::<Vec<_>>(),
        [(Some("allow"), "Allow", false, false), (Some("deny"), "Deny", true, false), (None, "Reply", true, false)]);
    assert!(pushed[..2].iter().all(|action| action.id == buttons[0].id));
}

#[test]
fn a_held_plan_card_shows_the_plan_with_approve_and_request_changes() {
    use crate::domain::{CardKind, ContentKind as Content, Hold};
    let card_spec = spec(CardKind::Review, "Ship the parser", "Two steps, tests first.")
        .content(Content::Plan, "1. Split the lexer\n2. Add tests");
    let state = held_fixture(Hold::Plan, card_spec);
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!((card.icon, card.eyebrow.as_ref().unwrap().text.as_str()), (Icon::Document, "Review"));
    assert_eq!(card.title, "Ship the parser");
    assert_eq!(card.content_row.as_ref().map(|row| (row.kind.as_str(), row.summary.as_str())), Some(("plan", "2 steps")));
    assert_eq!(card.actions.iter().map(|a| (a.label.as_str(), a.key.as_deref(), a.role.unwrap())).collect::<Vec<_>>(),
        [("Approve", Some("1"), Role::Advance), ("Request changes", Some("2"), Role::Neutral)]);
    let pushed = push_actions(&state, "i-1", Lang::Es).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.label.as_str(), a.text)).collect::<Vec<_>>(), [("Aprobar", false), ("Responder", true)],
        "the lock screen approves or replies; changes need the comments only the app takes");
    let ios = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert!(card_of(&ios).actions.iter().all(|action| action.key.is_none()));
}

fn fixed_options() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("request_action", vec![("done", "Done"), ("cannot", "I can't do it")]),
        ("request_test", vec![("works", "Works"), ("fails", "Doesn't work")]),
        ("propose", vec![("do_it", "Do it"), ("not_now", "Not now")]),
        ("request_input", vec![("provide", "Send the file"), ("skip", "I can't provide it")]),
        ("request_input", vec![("provide", "Send the text"), ("skip", "I can't provide it")]),
        ("request_input", vec![("provide", "Send the secret"), ("skip", "I can't provide it")]),
        ("problem", vec![("retry", "Retry the batch"), ("continue", "Continue without it"), ("stop", "Stop it")]),
    ]
}

fn proposal_options(options: &[(&str, &str)]) -> InteractionPayload {
    InteractionPayload::Question {
        options: options.iter().map(|(id, label)| crate::domain::QuestionOption { id: (*id).into(), label: (*label).into(), description: None, inputs: vec![] }).collect(),
        answer: None,
    }
}

fn option_labels(options: &[(&str, &str)], lang: Lang) -> Vec<String> {
    let mut state = fixture();
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = proposal_options(options);
    interaction.card = spec(crate::domain::CardKind::Propose, "Fix it", "Because.");
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, lang, "card:i-1", "");
    answers(card_of(&view)).iter().map(|action| action.label.clone()).collect()
}

#[test]
fn the_options_the_system_words_itself_read_in_the_language_of_the_view() {
    for (tool, options) in fixed_options() {
        let spanish = option_labels(&options, Lang::Es);
        let english = option_labels(&options, Lang::En);
        let source: Vec<String> = options.iter().map(|(_, label)| (*label).to_owned()).collect();
        assert_eq!(english, source, "{tool}: English reads as it was written");
        assert!(spanish.iter().zip(&english).all(|(es, en)| es != en), "{tool}: {spanish:?} is not Spanish");
    }
    assert_eq!(option_labels(&[("done", "Hecho a mi manera")], Lang::Es), ["Hecho a mi manera"], "a label someone wrote is never replaced");
}

#[test]
fn a_proposal_whose_task_ended_is_still_answerable_in_the_card() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = proposal_options(&[("do_it", "Do it"), ("not_now", "Not now")]);
    interaction.card = spec(crate::domain::CardKind::Propose, "Fix it", "Because.");
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    let buttons = answers(card_of(&view));
    assert_eq!(buttons.iter().map(|a| (a.label.as_str(), a.enabled, a.disabled_reason.clone())).collect::<Vec<_>>(),
        [("Hazlo", true, None), ("Ahora no", true, None)]);
}

fn markdown_items(view: &View) -> Vec<&Markdown> {
    view.sections.iter().filter(|section| section.id == "content").flat_map(|section| &section.items)
        .filter_map(|item| if let Item::Markdown(markdown) = item { Some(markdown) } else { None }).collect()
}

#[test]
fn the_text_of_a_held_plan_can_be_commented_block_by_block_and_other_text_cannot() {
    use crate::domain::{CardKind, ContentKind as Content, Hold};
    let plan = "1. Split the lexer\n2. Add tests";
    let held = held_fixture(Hold::Plan, spec(CardKind::Review, "Ship the parser", "Two steps.").content(Content::Report, plan));
    let view = render(&held, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert_eq!(markdown_items(&view).iter().map(|m| (m.text.as_str(), m.commentable)).collect::<Vec<_>>(), [(plan, true)]);
    let permission = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow", "").content(Content::Command, "rm -rf build"));
    let view = render(&permission, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert!(markdown_items(&view).iter().all(|m| !m.commentable));
}

#[test]
fn a_held_plan_is_reviewed_with_block_comments_request_changes_and_approve() {
    use crate::domain::{CardKind, Hold};
    let state = held_fixture(Hold::Plan, spec(CardKind::Review, "Ship the parser", "Two steps."));
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    let review: Vec<_> = card.actions.iter()
        .filter(|action| action.id.starts_with("changes:i-1@") || action.id.starts_with("accept:i-1@"))
        .map(|action| (action.label.as_str(), action.style, action.comments.is_some(), action.stamp.as_deref()))
        .collect();
    assert_eq!(review, [("Aprobar", Style::Primary, false, Some("Aprobado")),
        ("Pedir cambios", Style::Secondary, true, Some("Cambios solicitados"))]);
    let permission = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow cleaning the build", ""));
    let permission_view = render(&permission, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    let card = card_of(&permission_view);
    assert!(card.actions.iter().all(|action| !action.id.starts_with("changes:") && !action.id.starts_with("accept:")));
}

#[test]
fn a_permission_card_with_a_suggestion_offers_yes_always_and_no_in_that_order() {
    use crate::domain::{CardKind, Hold};
    let mut state = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow cleaning the build", ""));
    state.interactions.get_mut(&"i-1".into()).unwrap().payload =
        InteractionPayload::Question { options: Hold::Permission.options_offering(true), answer: None };
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!(answers(card).iter().map(|a| (a.option_id.as_deref().unwrap(), a.label.as_str(), a.role)).collect::<Vec<_>>(),
        [("allow", "Permitir", Some(Role::Risk)), ("always", "Siempre para esto", Some(Role::Neutral)),
         ("deny", "Denegar", Some(Role::Neutral))]);
}

#[test]
fn a_held_ask_user_question_card_shows_the_question_and_its_options() {
    use crate::domain::{CardKind, Hold};
    let mut state = held_fixture(Hold::Question, spec(CardKind::Decide, "Which database?", "The migration depends on it."));
    let options = ["Postgres", "SQLite"].iter().enumerate().map(|(index, label)| QuestionOption {
        id: format!("o{index}"), label: (*label).into(), description: None, inputs: vec![] }).collect();
    state.interactions.get_mut(&"i-1".into()).unwrap().payload = InteractionPayload::Question { options, answer: None };
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!((card.icon, card.eyebrow.as_ref().unwrap().text.as_str()), (Icon::Question, "Decidir"));
    assert_eq!(card.title, "Which database?");
    assert_eq!(answers(card).iter().map(|a| (a.option_id.as_deref().unwrap(), a.label.as_str(), a.key.as_deref())).collect::<Vec<_>>(),
        [("o0", "Postgres", Some("1")), ("o1", "SQLite", Some("2"))]);
    assert!(card.actions.iter().all(|action| !action.id.starts_with("ctx:")), "the hook waits for an answer");
    let pushed = push_actions(&state, "i-1", Lang::En).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.option.as_deref(), a.label.as_str(), a.text)).collect::<Vec<_>>(),
        [(Some("o0"), "Postgres", false), (Some("o1"), "SQLite", false), (None, "Reply", true)]);
}

#[test]
fn a_held_elicitation_card_accepts_with_the_form_values_or_declines() {
    use crate::domain::{CardKind, Hold};
    let mut state = held_fixture(Hold::Elicitation, spec(CardKind::Provide, "Which repository?", "The server asks."));
    if let InteractionPayload::Question { options, .. } = &mut state.interactions.get_mut(&"i-1".into()).unwrap().payload {
        options[0].description = Some("Write the Repository".into());
    }
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!(card.title, "Which repository?");
    let buttons = answers(card);
    assert_eq!(buttons.iter().map(|a| (a.option_id.as_deref().unwrap(), a.label.as_str(), a.key.as_deref())).collect::<Vec<_>>(),
        [("accept", "Aceptar", Some("1")), ("decline", "Rechazar", Some("2"))]);
    assert!(matches!(&buttons[0].input, Input::Text { placeholder, .. } if placeholder == "Write the Repository"));
    let pushed = push_actions(&state, "i-1", Lang::En).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.option.as_deref(), a.label.as_str(), a.text)).collect::<Vec<_>>(),
        [(Some("accept"), "Accept", true), (Some("decline"), "Decline", false), (None, "Reply", true)]);
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

fn presented_pr() -> DomainState {
    use crate::domain::{Actor, Command, DeliveryTarget, MergeMethod};
    let mut state = DomainState::new("test-epoch");
    let agent = Actor::Agent { agent_id: "a".into() };
    let user = Actor::User { user_id: "u".into() };
    for (actor, command) in [
        (Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node A".into() }),
        (user.clone(), Command::CreateRequest { request_id: "r".into(), body: "Fix it".into(), auto_merge: false }),
        (user.clone(), Command::CreateTask { task_id: "t".into(), request_id: "r".into(), title: "Fix it".into(),
            agent_id: Some("a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None }),
        (agent.clone(), Command::RegisterPr { task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(),
            repository: "owner/repo".into(), number: 1, head: "head1".into(), base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash] }),
        (agent, Command::PresentResult { attachments: vec![], task_id: "t".into(), interaction_id: "d".into(),
            body: "Ready".into(), targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }],
            is_final: true, card: crate::domain::CardSpec::delivery("Review the result", &[], true) }),
    ] {
        execute(&mut state, actor, command);
    }
    state
}

fn execute(state: &mut DomainState, actor: crate::domain::Actor, command: crate::domain::Command) {
    let envelope = crate::domain::CommandEnvelope {
        protocol_version: 2, data_epoch: state.data_epoch.clone(),
        command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command,
    };
    state.execute(&actor, &envelope).unwrap();
}

#[test]
fn a_pr_sent_back_leaves_waiting_for_you_and_its_task_says_what_alondra_asked() {
    use crate::domain::{Actor, Command, DeliveryTarget, Instruction};
    let live = LiveSnapshot::default();
    let home = |state: &DomainState| render(state, &live, Client::Ios, Lang::En, "home", "").sections[0].card_ids.clone();
    let task = |state: &DomainState, lang| serde_json::to_string(&render(state, &live, Client::Tui, lang, "task:t", "")).unwrap();
    let presented = presented_pr();
    assert_eq!(home(&presented), ["d"], "the delivery is the card; the PR it carries is not");
    let user = Actor::User { user_id: "u".into() };
    for (actor, instruction, english, spanish) in [
        (Actor::System, Instruction::Conflicts, "Resolving conflicts", "Resolviendo conflictos"),
        (user.clone(), Instruction::Changes, "Changes requested", "Cambios solicitados"),
        (user.clone(), Instruction::Question, "Answering your question", "Respondiendo a tu pregunta"),
        (user.clone(), Instruction::Reply, "Acting on your reply", "Atendiendo tu respuesta"),
    ] {
        let mut state = presented.clone();
        execute(&mut state, actor, Command::SubmitFeedback {
            attachments: vec![], presentation_id: "d".into(), body: "Back to you".into(), instruction,
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }],
        });
        assert!(home(&state).is_empty(), "{instruction:?} takes the card out of Waiting for You");
        assert!(task(&state, Lang::En).contains(english), "{instruction:?}");
        assert!(task(&state, Lang::Es).contains(spanish), "{instruction:?}");
    }
    let mut state = presented.clone();
    execute(&mut state, Actor::System, Command::SubmitFeedback {
        attachments: vec![], presentation_id: "d".into(), instruction: Instruction::Conflicts,
        body: crate::domain::merge_failed_text(12, "https://github.com/example/repo/pull/12", "main", "its checks failed (lint)"),
        targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }],
    });
    let english = task(&state, Lang::En);
    assert!(english.contains("Alondra couldn't merge it") && !english.contains("Resolving conflicts"), "{english}");
    assert!(task(&state, Lang::Es).contains("Alondra no pudo fusionarla"));
    let card = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::En, "card:d", "")).unwrap();
    assert!(card.contains("Alondra couldn't merge #12") && card.contains("its checks failed (lint)")
        && !card.contains("Conflicts were reported"), "{card}");
}

#[test]
fn a_node_reads_merging_from_the_claim_of_its_merge_authorization_until_the_merge_is_observed() {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.keys().next().unwrap().clone();
    state.tasks.get_mut(&task).unwrap().agent_id = Some("a-1".into());
    state.pull_requests.insert("pr-1".into(), PullRequest {
        id: "pr-1".into(), task_id: task.clone(), host: "github.com".into(), repository: "me/app".into(), number: 7,
        head: "abc".into(), base: "main".into(), state: PrState::Open, merged_head: None, allowed_methods: vec![], review: None,
    });
    let authorization = |attempt| MergeAuthorization {
        id: "auth-1".into(), task_id: task.clone(), pr_id: "pr-1".into(), presentation_id: "presentation".into(), head: "abc".into(),
        method: MergeMethod::Merge, user_id: "u".into(), valid: true, attempt, claimed_by: Some("a-1".into()), at_revision: 1,
    };
    let state_of = |state: &DomainState, lang| {
        let view = render(state, &LiveSnapshot::default(), Client::Ios, lang, "fleet", "");
        view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
            if let Item::Row(row) = item && row.id == "a-1" { Some(row.badges[0].text.clone()) } else { None }
        }).unwrap()
    };
    state.merge_authorizations.insert("auth-1".into(), authorization(MergeAttemptState::Unclaimed));
    assert_ne!(state_of(&state, Lang::En), "Merging", "an authorization nobody claimed yet is not a merge");
    state.merge_authorizations.insert("auth-1".into(), authorization(MergeAttemptState::Claimed));
    assert_eq!(state_of(&state, Lang::En), "Merging");
    assert_eq!(state_of(&state, Lang::Es), "Fusionando");
    state.pull_requests.get_mut(&"pr-1".into()).unwrap().state = PrState::Merged;
    assert_ne!(state_of(&state, Lang::En), "Merging", "the merge was observed");
}

#[test]
fn a_push_shows_what_the_card_needs_over_why() {
    use crate::domain::{CardKind, Hold};
    let alert = |state: &DomainState| push_content(state, "i-1", Lang::En).unwrap().alert.unwrap();
    let state = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow publishing Brew 0.2.36", "Restarts the daemon for a few seconds."));
    assert_eq!(alert(&state), PushAlert {
        title: "Allow publishing Brew 0.2.36".into(),
        body: Some("Restarts the daemon for a few seconds.".into()),
    }, "the lock screen says what to do and why, never the raw command");
    assert_eq!(push_content(&state, "i-1", Lang::Es).unwrap().alert.unwrap().title, "Allow publishing Brew 0.2.36",
        "the agent's own words are not translated");
    let bare = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow publishing Brew 0.2.36", ""));
    assert_eq!(alert(&bare).body.as_deref(), Some("Authorize"), "without a why the card says what kind it is");

    let long = held_fixture(Hold::Question, spec(CardKind::Decide, "Which database?", &format!("Should I {}?", "really ".repeat(30))));
    let long = alert(&long);
    assert_eq!(long.body.as_ref().unwrap().chars().count(), PUSH_BODY_CHARS);
    assert!(long.body.unwrap().ends_with('…'));
}

#[test]
fn a_push_interrupts_by_what_the_card_holds_back_and_groups_by_task_then_request() {
    use crate::domain::{CardKind::*, Hold};
    let level = |kind, blocking: bool| {
        let mut state = held_fixture(Hold::Permission, spec(kind, "Do it", "Because."));
        state.interactions.get_mut(&"i-1".into()).unwrap().blocking = blocking;
        push_content(&state, "i-1", Lang::En).unwrap().interruption_level
    };
    for (kind, blocking, expected) in [
        (Problem, true, "time-sensitive"), (Authorize, true, "time-sensitive"), (Do, true, "time-sensitive"),
        (Authorize, false, "active"), (Do, false, "active"),
        (Decide, true, "active"), (Review, false, "active"), (Test, false, "active"), (Provide, true, "active"),
        (Know, false, "passive"), (Propose, false, "passive"),
    ] {
        assert_eq!(level(kind, blocking), expected, "{kind:?} blocking={blocking}");
    }
    let state = held_fixture(Hold::Permission, spec(Authorize, "Do it", "Because."));
    let thread = |state: &DomainState, id: &str| push_content(state, id, Lang::En).unwrap().thread;
    assert_eq!(thread(&state, "i-1").as_deref(), Some("t-1"), "one thread per Task, whoever wrote the card");
    assert_eq!(thread(&state, "i-2").as_deref(), Some("t-1"), "a system card of the Task joins the agent's");
    assert_eq!(thread(&state, "i-4").as_deref(), Some("orchestrator"), "no Task and no Request");
    let mut requested = state.clone();
    requested.interactions.get_mut(&"i-4".into()).unwrap().scope = Scope::Orchestrator { request_id: Some("r-9".into()) };
    assert_eq!(thread(&requested, "i-4").as_deref(), Some("r-9"), "an orchestrator card groups under its Request");
}

/// The actions the terminal offers for a document: its cards' own (each child card is a row of
/// its own), its action bars and its notices'.
fn tui_document_actions(view: &View) -> Vec<Action> {
    let mut out = Vec::new();
    for item in view.sections.iter().flat_map(|section| &section.items) {
        match item {
            Item::Card(card) => out.extend(card.actions.iter().chain(&card.reply).chain(&card.more).cloned().map(Action::from)),
            Item::Actions(bar) => out.extend(bar.actions.iter().cloned()),
            Item::Notice(notice) => out.extend(notice.action.iter().cloned()),
            _ => {}
        }
    }
    let mut seen = BTreeSet::new();
    out.retain(|action| seen.insert((action.id.clone(), action.label.clone())));
    out
}

#[test]
fn every_tui_action_a_screen_offers_has_its_own_key() {
    let (golden, golden_live) = (golden_fixture(), golden_live());
    let (plan, no_live) = (plan_question_fixture(), LiveSnapshot::default());
    let mut problems = Vec::new();
    let screens = [
        "card:a98d034b-249e-4487-a89b-57de38c26372",
        "card:f12d034b-249e-4487-a89b-57de38c26372",
        "card:github.com/example/repo#12",
        "card:hf-weekly-2026-09-30",
        "tasks",
        "node:d887a34b-249e-4487-a89b-57de38c26372",
        "node:orchestrator",
        "task:t-651c38484cd2f34e74d5107ba05f7f15",
        "request:r-9f2156b5a8894e53901f0af576dff6a2",
        "pull_request:github.com/example/repo#12",
    ]
    .map(|key| (&golden, &golden_live, key));
    for (state, live, key) in screens.into_iter().chain([(&plan, &no_live, "card:i-1"), (&plan, &no_live, "card:plan-r1")]) {
        let view = render(state, live, Client::Tui, Lang::En, key, "2026-09-30T10:00:00Z");
        let mut seen = BTreeSet::new();
        for action in tui_document_actions(&view).into_iter().filter(|action| action.enabled) {
            match &action.key {
                None => problems.push(format!("{key}: {} ({}) has no key", action.label, action.id)),
                Some(hint) if !seen.insert(hint.clone()) => {
                    problems.push(format!("{key}: {} ({}) reuses {hint}", action.label, action.id))
                }
                Some(_) => {}
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Task t-1 of a scheduled run whose launch failed, reached through the domain's own commands so
/// the state is one the daemon could hold.
fn failed_launch_fixture(error: &str) -> DomainState {
    use crate::domain::{
        Actor, Command, CommandEnvelope, Schedule, ScheduleFrequency, ScheduleLaunchIntent, ScheduleRunOrigin,
    };
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.clear();
    state.requests.clear();
    let run = |state: &mut DomainState, command| {
        state.execute(&Actor::System, &crate::domain::CommandEnvelope {
            protocol_version: 2,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("c-{}", state.revision).into(),
            expected_revision: state.revision,
            command,
        }).unwrap();
    };
    run(&mut state, Command::CreateSchedule { schedule: Schedule {
        id: "0789f185-0000".into(), title: "Build a feature".into(), text: "Build a feature".into(),
        project: Some("alondra".into()), options: BTreeMap::new(), disallowed_tools: vec![],
        frequency: ScheduleFrequency::Daily, hour: 9, minute: 0, weekday: None, interval: None,
        start_at: None, enabled: true, next_run: Some(100), revision: 1, deleted_at: None,
    }});
    run(&mut state, Command::ClaimScheduleRun {
        schedule_id: "0789f185-0000".into(), run_id: "run-7e2035e8e1d7c5e3f9431b9087d457e3".into(),
        origin: ScheduleRunOrigin::Due, now: 100, next_after_now: Some(200), expected_schedule_revision: None,
    });
    let mut task = fixture().tasks[&"t-1".into()].clone();
    (task.state, task.started, task.final_presentation) = (TaskState::NotStarted, false, None);
    let options = crate::options::OptionsFile::default().resolve(Some("alondra"), &BTreeMap::new()).unwrap();
    run(&mut state, Command::RecordScheduleRunSent {
        run_id: "run-7e2035e8e1d7c5e3f9431b9087d457e3".into(),
        request: Request::new("r-1".into(), "Build a feature".into()),
        task,
        launch: ScheduleLaunchIntent {
            name: "scheduled/run-1".into(), cwd: "/synthetic/Projects/alondra".into(),
            prompt: "Build a feature".into(), options_json: serde_json::to_string(&options).unwrap(),
        },
        finished_at: 101,
    });
    run(&mut state, Command::RecordScheduleRunFailed {
        run_id: "run-7e2035e8e1d7c5e3f9431b9087d457e3".into(), reason: error.into(), finished_at: 102,
    });
    state
}

#[test]
fn a_task_whose_scheduled_launch_failed_says_so_and_why() {
    let state = failed_launch_fixture("Workspace not trusted.\n");
    let live = LiveSnapshot::default();
    let detail = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::En, "task:t-1", "")).unwrap();
    assert!(detail.contains("Did not start"), "{detail}");
    assert!(detail.contains("the scheduled run could not launch its node: Workspace not trusted."), "{detail}");
    assert!(!detail.contains("Not started"), "{detail}");
    let list = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::Es, "tasks", "")).unwrap();
    assert!(list.contains("No arrancó"), "{list}");
}

#[test]
fn a_task_without_a_failed_launch_is_still_not_started() {
    let mut state = failed_launch_fixture("boom");
    state.schedule.runs.clear();
    let detail = serde_json::to_string(&render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "task:t-1", "")).unwrap();
    assert!(detail.contains("Not started") && !detail.contains("Did not start"), "{detail}");
}

fn waiting_for_a_relaunch(live: &LiveSnapshot, client: Client, lang: Lang, key: &str) -> (String, Option<Action>) {
    let mut state = failed_launch_fixture("boom");
    state.schedule.runs.clear();
    let view = render(&state, live, client, lang, key, "");
    let relaunch = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
        Item::Actions(bar) => bar.actions.iter().find(|action| action.id.starts_with("relaunch:")).cloned(),
        _ => None,
    });
    (serde_json::to_string(&view).unwrap(), relaunch)
}

#[test]
fn a_task_whose_node_the_user_closed_reads_node_closed_and_offers_relaunch_in_its_actions_and_its_row() {
    let live = LiveSnapshot { closed_tasks: vec!["t-1".into()], ..LiveSnapshot::default() };
    let (detail, relaunch) = waiting_for_a_relaunch(&live, Client::Tui, Lang::En, "task:t-1");
    let relaunch = relaunch.expect("the task offers Relaunch");
    assert_eq!((relaunch.label.as_str(), relaunch.key.as_deref(), relaunch.style), ("Relaunch", Some("l"), Style::Primary));
    assert!(relaunch.id.starts_with("relaunch:t-1@"), "{}", relaunch.id);
    assert!(detail.contains("Node closed") && !detail.contains("Not started"), "{detail}");
    let (spanish, relaunch) = waiting_for_a_relaunch(&live, Client::Ios, Lang::Es, "task:t-1");
    assert_eq!(relaunch.unwrap().label, "Relanzar");
    assert!(spanish.contains("Nodo cerrado"), "{spanish}");
    for (client, lang) in [(Client::Ios, Lang::En), (Client::Mac, Lang::Es), (Client::Tui, Lang::En)] {
        let (list, _) = waiting_for_a_relaunch(&live, client, lang, "tasks");
        assert!(list.contains("\"id\":\"relaunch:t-1@"), "{client:?}: the row carries the action: {list}");
        assert!(list.contains(if lang == Lang::En { "Node closed" } else { "Nodo cerrado" }), "{client:?}: {list}");
    }
}

#[test]
fn a_task_is_not_offered_a_relaunch_unless_it_is_not_started_with_no_node_and_its_node_was_closed() {
    let (detail, relaunch) = waiting_for_a_relaunch(&LiveSnapshot::default(), Client::Tui, Lang::En, "task:t-1");
    assert!(relaunch.is_none() && detail.contains("Not started"), "a task nobody closed: {detail}");
    let closed = LiveSnapshot { closed_tasks: vec!["t-1".into()], ..LiveSnapshot::default() };
    let mut state = failed_launch_fixture("boom");
    state.schedule.runs.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let detail = serde_json::to_string(&render(&state, &closed, Client::Tui, Lang::En, "task:t-1", "")).unwrap();
    assert!(!detail.contains("relaunch:") && !detail.contains("Node closed"), "a task a new node took: {detail}");
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = None;
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Cancelled;
    let detail = serde_json::to_string(&render(&state, &closed, Client::Tui, Lang::En, "task:t-1", "")).unwrap();
    assert!(!detail.contains("relaunch:"), "a cancelled task: {detail}");
}

#[test]
fn the_tui_cancels_a_task_with_x() {
    let state = failed_launch_fixture("boom");
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "task:t-1", "");
    let cancel = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
        Item::Actions(bar) => bar.actions.iter().find(|action| action.id.starts_with("cancel:")),
        _ => None,
    }).unwrap_or_else(|| panic!("cancel action: {:?}", state.task_actions(&"t-1".into())));
    assert_eq!(cancel.key.as_deref(), Some("x"));
}
fn answered_and_closed_request() -> DomainState {
    use crate::domain::{Actor, Command, CommandEnvelope, DomainEffect, PROTOCOL_VERSION};
    let run = |state: &mut DomainState, actor: Actor, command: Command| {
        state.execute(&actor, &crate::domain::CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("c-{}", state.revision).into(),
            expected_revision: state.revision,
            command,
        }).unwrap()
    };
    let mut state = DomainState::new("epoch-1");
    let orchestrator = Actor::Orchestrator { generation: 0 };
    run(&mut state, Actor::User { user_id: "u".into() }, Command::CreateRequest {
        request_id: "a7efd99d".into(), body: "can we encrypt images via R2".into(), auto_merge: false,
    });
    let published = run(&mut state, orchestrator.clone(), Command::PublishInformation {
        attachments: vec![],
        interaction_id: "a7efd99d-answer-1".into(),
        scope: Scope::Orchestrator { request_id: Some("a7efd99d".into()) },
        recipient: Recipient::User,
        body: "Sí. Para el cifrado una imagen son bytes como cualquier otro.".into(), card: crate::domain::CardSpec::derived(crate::domain::CardKind::Know, "Read the update"),
    });
    assert_eq!(published.effects, vec![DomainEffect::NotifyUser {
        task_id: None, interaction_id: "a7efd99d-answer-1".into(),
    }]);
    run(&mut state, orchestrator, Command::CloseRequest {
        request_id: "a7efd99d".into(),
        answer: Some("Sí: se cifran en el cliente antes de subirlas a R2".into()),
        withdrawn: false,
        title: Some("Cifrar imágenes vía R2".into()),
    });
    state
}
#[test]
fn an_orchestrator_answer_waits_in_every_inbox_after_its_request_closes() {
    let state = answered_and_closed_request();
    let live = LiveSnapshot::default();
    let ios = render(&state, &live, Client::Ios, Lang::Es, "home", "");
    assert_eq!(ios.sections[0].card_ids, vec!["a7efd99d-answer-1".to_owned()]);
    assert_eq!(ios.status.badge, 1);
    assert_eq!(inbox_badge(&state), 1);
    let tui = render(&state, &live, Client::Tui, Lang::En, "home", "");
    let waiting = tui.sections.iter().find(|section| section.id == "waiting").unwrap();
    assert!(matches!(waiting.items.as_slice(), [Item::Card(card)] if card.id == "a7efd99d-answer-1"));
    let sidebar = render(&state, &live, Client::Mac, Lang::En, "sidebar", "");
    assert!(serde_json::to_string(&sidebar).unwrap().contains("a7efd99d-answer-1"));
    let card = render(&state, &live, Client::Ios, Lang::Es, "card:a7efd99d-answer-1", "");
    let Some(Item::Card(card)) = card.sections.iter().flat_map(|section| &section.items).next() else {
        panic!("the answer renders as a card");
    };
    assert_eq!(card.title, "Sí. Para el cifrado una imagen son bytes como cualquier otro.");
    assert_eq!(card.meta.as_deref(), Some("Orquestador"));
    assert!(card.actions.iter().any(|action| action.id.starts_with("read:")));
    assert!(card.reply.as_ref().is_some_and(|reply| reply.id.starts_with("msg:view@")),
        "the answer is replied to the orchestrator");
    let pushed = push_content(&with_cards(&state), "a7efd99d-answer-1", Lang::Es).unwrap();
    let alert = pushed.alert.unwrap();
    assert_eq!(alert.title, "Sí. Para el cifrado una imagen son bytes como cualquier otro.");
    assert_eq!((pushed.interruption_level, pushed.thread.as_deref()), ("passive", Some("a7efd99d")),
        "the answer groups with the rest of its Request");
}

/// Three questions asked together, the ones listed in `answered` (0-based) already answered.
fn group_state(answered: &[usize]) -> DomainState {
    use crate::domain::{CardKind, QuestionGroup};
    let mut state = fixture();
    let base = state.interactions.remove(&"i-1".into()).unwrap();
    for (at, need) in ["Which name?", "Ship it today?", "Which channel?"].into_iter().enumerate() {
        let mut one = base.clone();
        one.id = format!("g-q{}", at + 1).into();
        one.order = at as u64 + 1;
        one.body = need.into();
        let mut card = spec(CardKind::Decide, need, "The release needs it");
        card.group = Some(QuestionGroup { id: "g".into(), index: at as u8 + 1, total: 3 });
        one.card = card;
        let options = vec![
            QuestionOption { id: "o0".into(), label: "First".into(), description: None, inputs: vec![] },
            QuestionOption { id: "o1".into(), label: "Second".into(), description: None, inputs: vec![] },
        ];
        let answer = answered.contains(&at).then(|| crate::domain::Answer {
            attachments: vec![], option_id: "o1".into(), inputs: BTreeMap::new(), responder: "u".into(), at_revision: 1,
        });
        one.payload = InteractionPayload::Question { options, answer };
        state.interactions.insert(one.id.clone(), one);
    }
    state
}

#[test]
fn a_question_asked_together_says_where_it_stands_in_the_group_on_every_client() {
    let state = group_state(&[]);
    let live = LiveSnapshot::default();
    let card_of_client = |client, lang, id: &str| render(&state, &live, client, lang, &format!("card:{id}"), "").sections[0].items.iter()
        .find_map(|item| match item { Item::Card(card) => Some(card.clone()), _ => None }).unwrap();
    let phone = card_of_client(Client::Ios, Lang::En, "g-q2");
    assert_eq!(phone.eyebrow.as_ref().unwrap().text, "Decide · 2 of 3");
    assert!(phone.meta.as_deref().unwrap().ends_with("· 2 of 3"), "the phone shows the kind as a badge, so the position is in the line under: {:?}", phone.meta);
    assert!(phone.actions.iter().all(|a| a.id.starts_with("ans:g-q2@")), "an ordinary card answered with the ordinary answer");
    let spanish = card_of_client(Client::Ios, Lang::Es, "g-q3");
    assert_eq!(spanish.eyebrow.as_ref().unwrap().text, "Decidir · 3 de 3");
    let mac = card_of_client(Client::Mac, Lang::En, "g-q1");
    assert_eq!(mac.eyebrow.as_ref().unwrap().text, "Decide · 1 of 3");
    assert!(!mac.meta.unwrap_or_default().contains("1 of 3"), "the Mac's header already says it");
}

#[test]
fn change_answers_lists_what_was_answered_while_the_group_is_incomplete() {
    let live = LiveSnapshot::default();
    let more = |state: &DomainState, id: &str| render(state, &live, Client::Ios, Lang::En, &format!("card:{id}"), "").sections[0].items.iter()
        .find_map(|item| match item { Item::Card(card) => Some(card.more.clone()), _ => None }).unwrap();
    let change = |more: &[CardAction]| more.iter().find(|a| a.id.starts_with("retake:")).cloned();
    assert!(change(&more(&group_state(&[]), "g-q1")).is_none(), "nothing answered, nothing to change");
    let state = group_state(&[0, 2]);
    let action = change(&more(&state, "g-q2")).expect("Change answers on the one still open");
    assert_eq!((action.label.as_str(), action.presentation), ("Change answers", Presentation::Options));
    assert_eq!(action.options.iter().map(|o| (o.id.as_str(), o.label.as_str())).collect::<Vec<_>>(),
        [("g-q1", "Which name? → Second"), ("g-q3", "Which channel? → Second")]);
    let spanish = render(&state, &live, Client::Ios, Lang::Es, "card:g-q2", "").sections[0].items.iter()
        .find_map(|item| match item { Item::Card(card) => card.more.iter().find(|a| a.id.starts_with("retake:")).map(|a| a.label.clone()), _ => None });
    assert_eq!(spanish.as_deref(), Some("Cambiar respuestas"));
}

#[test]
fn each_question_of_a_group_is_its_own_notification_with_its_own_buttons_and_its_place() {
    let state = group_state(&[]);
    let content = push_content(&state, "g-q2", Lang::Es).unwrap();
    let alert = content.alert.unwrap();
    assert_eq!(alert.title, "Ship it today?");
    assert!(alert.body.unwrap().starts_with("2 de 3 · "), "the subtitle says which question it is");
    assert_eq!(content.actions.iter().filter_map(|a| a.option.as_deref()).collect::<Vec<_>>(), ["o0", "o1"]);
    assert!(content.actions.iter().filter(|a| a.option.is_some()).all(|a| a.id.starts_with("ans:g-q2@")));
    assert_eq!(push_content(&state, "g-q1", Lang::En).unwrap().thread, content.thread, "one thread for the node");
}

const GROUP_CASES: [(&str, Client, Lang); 3] = [
    ("card-group.json", Client::Ios, Lang::En),
    ("card-group-es.json", Client::Ios, Lang::Es),
    ("card-group-mac.json", Client::Mac, Lang::En),
];

/// The second of three questions asked together, the first already answered.
#[test]
fn a_question_asked_together_matches_goldens() {
    let state = group_state(&[0]);
    for ((filename, client, lang), expected) in GROUP_CASES.into_iter().zip([
        include_str!("../../../../fixtures/views/cards/card-group.json"),
        include_str!("../../../../fixtures/views/cards/card-group-es.json"),
        include_str!("../../../../fixtures/views/cards/card-group-mac.json"),
    ]) {
        let actual = render(&state, &LiveSnapshot::default(), client, lang, "card:g-q2", "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

#[test]
#[ignore = "explicit fixture generation after Bazel authorization"]
fn generate_group_goldens() {
    let dir = std::env::var("ALONDRA_GOLDEN_OUT").expect("ALONDRA_GOLDEN_OUT required");
    let state = group_state(&[0]);
    for (filename, client, lang) in GROUP_CASES {
        let view = render(&state, &LiveSnapshot::default(), client, lang, "card:g-q2", "2026-09-30T10:00:00Z");
        std::fs::write(format!("{dir}/{filename}"), format!("{}\n", serde_json::to_string_pretty(&view).unwrap())).unwrap();
    }
}

const FLEET_GOLDENS: [(&str, Client, Lang); 4] = [
    ("fleet-ios.json", Client::Ios, Lang::En),
    ("fleet-ios-es.json", Client::Ios, Lang::Es),
    ("fleet-mac.json", Client::Mac, Lang::En),
    ("fleet-tui.json", Client::Tui, Lang::En),
];

/// A fleet where a node needing the user, a queued one with no live entry, a working one and a
/// waiting one belong to projects, and two nodes with nothing open are idle.
fn fleet_fixture() -> (DomainState, LiveSnapshot) {
    let mut state = golden_fixture();
    let mut live = golden_live();
    let golden_task = state.tasks.get_mut(&"t-651c38484cd2f34e74d5107ba05f7f15".into()).unwrap();
    golden_task.agent_id = Some("d887a34b-249e-4487-a89b-57de38c26372".into());
    let template = golden_task.clone();
    let golden_request = template.request_id.0.clone();
    for (agent, request, title, task_state) in [
        ("queued", golden_request.as_str(), "Wire the badge after the review", TaskState::WaitingDependency),
        ("zeta-worker", "r-zeta", "Build the importer", TaskState::Running),
        ("alpha-waiting", "r-alpha", "Plan the migration", TaskState::NotStarted),
        ("finished-yesterday", golden_request.as_str(), "Ship the compact editor", TaskState::Done),
        ("finished-today", golden_request.as_str(), "Trim the push payload", TaskState::Done),
    ] {
        state.agents.insert(agent.into(), Agent { id: agent.into(), name: agent.into(), connected: true });
        if !state.requests.contains_key(&request.into()) {
            state.requests.insert(request.into(), Request::new(request.into(), title.into()));
        }
        let mut task = template.clone();
        task.id = format!("t-{agent}").into();
        task.request_id = request.into();
        task.agent_id = Some(agent.into());
        task.title = title.into();
        task.state = task_state;
        task.final_presentation = None;
        task.pr_id = None;
        task.dependencies = BTreeSet::new();
        task.revision = state.revision;
        state.tasks.insert(task.id.clone(), task);
    }
    state.tasks.get_mut(&"t-queued".into()).unwrap().dependencies = ["t-651c38484cd2f34e74d5107ba05f7f15".into()].into();
    for (agent, project, working, at) in [
        ("zeta-worker", Some("Zeta"), true, "2026-09-30T09:57:00Z"),
        ("alpha-waiting", Some("Alpha"), false, "2026-09-30T09:20:00Z"),
        ("finished-yesterday", None, false, "2026-09-29T16:00:00Z"),
        ("finished-today", None, false, "2026-09-30T09:30:00Z"),
    ] {
        live.nodes.push(LiveNode {
            agent_id: agent.into(), name: agent.into(), project: project.map(Into::into), working,
            state_key: Some(if working { "busy" } else { "free" }.into()),
            last_message_at: Some(at.into()), ..LiveNode::default()
        });
    }
    (state, live)
}

fn section_ids(view: &View) -> Vec<&str> {
    view.sections.iter().map(|section| section.id.as_str()).collect()
}

fn section_rows<'a>(view: &'a View, id: &str) -> Vec<&'a str> {
    view.sections.iter().find(|section| section.id == id).unwrap().items.iter()
        .filter_map(|item| if let Item::Row(row) = item { Some(row.id.as_str()) } else { None }).collect()
}

#[test]
fn a_node_without_a_live_entry_takes_the_project_of_its_tasks_request() {
    let (state, live) = fleet_fixture();
    let view = render(&state, &live, Client::Ios, Lang::En, "fleet", "");
    assert!(section_rows(&view, "project:Demo Garden").contains(&"queued"));
    assert!(!section_ids(&view).contains(&"project:Other"), "{:?}", section_ids(&view));
}

#[test]
fn a_node_with_no_project_anywhere_is_in_other_after_the_named_projects() {
    let (mut state, live) = fleet_fixture();
    let mut task = state.tasks[&"t-queued".into()].clone();
    state.requests.insert("r-orphan".into(), Request::new("r-orphan".into(), "Orphan".into()));
    state.agents.insert("orphan".into(), Agent { id: "orphan".into(), name: "orphan".into(), connected: true });
    task.id = "t-orphan".into();
    task.request_id = "r-orphan".into();
    task.agent_id = Some("orphan".into());
    task.state = TaskState::NeedsYou;
    state.tasks.insert(task.id.clone(), task);
    let view = render(&state, &live, Client::Ios, Lang::En, "fleet", "");
    let ids = section_ids(&view);
    assert_eq!(&ids[1..4], ["project:Demo Garden", "project:Other", "project:Zeta"],
        "Other ties with a named project on urgency and comes after it");
    let es = render(&state, &live, Client::Ios, Lang::Es, "fleet", "");
    assert!(section_ids(&es).contains(&"project:Otros"));
}

#[test]
fn fleet_projects_come_by_their_most_urgent_node_and_nodes_by_urgency() {
    let (state, live) = fleet_fixture();
    let view = render(&state, &live, Client::Ios, Lang::En, "fleet", "");
    assert_eq!(section_ids(&view), ["orchestrator", "project:Demo Garden", "project:Zeta", "project:Alpha", "idle"]);
    assert_eq!(section_rows(&view, "project:Demo Garden"), ["d887a34b-249e-4487-a89b-57de38c26372", "queued"],
        "a node that needs the user comes before one that waits");
}

#[test]
fn idle_nodes_are_a_collapsed_last_section_with_the_most_recent_first() {
    let (state, live) = fleet_fixture();
    let view = render(&state, &live, Client::Ios, Lang::En, "fleet", "");
    let idle = view.sections.last().unwrap();
    assert_eq!((idle.id.as_str(), idle.title.as_deref(), idle.count, idle.collapsed), ("idle", Some("Idle"), Some(2), true));
    assert_eq!(section_rows(&view, "idle"), ["finished-today", "finished-yesterday"]);
    assert!(view.sections.iter().filter(|section| section.id != "idle").all(|section| !section.collapsed));
    let es = render(&state, &live, Client::Ios, Lang::Es, "fleet", "");
    assert_eq!(es.sections.last().unwrap().title.as_deref(), Some("Inactivos"));
}

#[test]
fn fleet_views_match_goldens() {
    let (state, live) = fleet_fixture();
    for (filename, client, lang) in FLEET_GOLDENS {
        let expected = match filename {
            "fleet-ios.json" => include_str!("../../../../fixtures/views/cards/fleet-ios.json"),
            "fleet-ios-es.json" => include_str!("../../../../fixtures/views/cards/fleet-ios-es.json"),
            "fleet-mac.json" => include_str!("../../../../fixtures/views/cards/fleet-mac.json"),
            _ => include_str!("../../../../fixtures/views/cards/fleet-tui.json"),
        };
        let actual = render(&state, &live, client, lang, "fleet", "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

#[test]
fn a_node_with_an_open_task_never_reads_idle_and_idle_is_only_for_the_bottom_section() {
    let (state, live) = fleet_fixture();
    let label = |lang, id: &str| {
        let view = render(&state, &live, Client::Ios, lang, "fleet", "");
        view.sections.iter().flat_map(|section| &section.items).find_map(|item| {
            if let Item::Row(row) = item && row.id == id { Some(row.badges[0].text.clone()) } else { None }
        }).unwrap()
    };
    assert_eq!(label(Lang::En, "queued"), "Waiting for Restore the native review experience");
    assert_eq!(label(Lang::Es, "queued"), "Esperando a Restore the native review experience");
    assert_eq!(label(Lang::En, "alpha-waiting"), "Queued");
    assert_eq!(label(Lang::Es, "alpha-waiting"), "En cola");
    let view = render(&state, &live, Client::Ios, Lang::En, "fleet", "");
    for section in view.sections.iter().filter(|section| section.id.starts_with("project:")) {
        for item in &section.items {
            if let Item::Row(row) = item {
                assert_ne!(row.badges[0].text, "Idle", "{}", row.id);
            }
        }
    }
}

fn now_title(state: &DomainState, lang: Lang) -> Option<String> {
    let view = render(state, &LiveSnapshot::default(), Client::Ios, lang, "task:t-1", "2026-09-30T10:00:00Z");
    let section = view.sections.iter().find(|section| section.id == "now")?;
    let Item::Row(row) = &section.items[0] else { panic!("now row") };
    Some(row.title.clone())
}

#[test]
fn a_closing_task_says_whether_it_waits_for_its_merge_or_its_nodes_cleanup() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Closing;
    assert_eq!(now_title(&state, Lang::En).unwrap(), "accepted; Alondra merges its pull request");
    assert_eq!(now_title(&state, Lang::Es).unwrap(), "aceptada; Alondra fusiona su PR");
    state.tasks.get_mut(&"t-1".into()).unwrap().wrap_up.asked_at = Some(5);
    assert_eq!(now_title(&state, Lang::En).unwrap(), "finished; the node cleans up before Alondra closes it");
    assert_eq!(now_title(&state, Lang::Es).unwrap(), "terminada; el nodo limpia antes de que Alondra lo cierre");
}

#[test]
fn a_finished_task_says_what_became_of_its_node() {
    use crate::domain::NodeEnd;
    let mut state = fixture();
    let lines = [
        (NodeEnd::Closed, "node closed", "nodo cerrado"),
        (NodeEnd::Gone, "node already closed", "el nodo ya estaba cerrado"),
        (NodeEnd::KeptByUser, "node left open: you chose to keep it", "nodo abierto: elegiste dejarlo"),
        (NodeEnd::UsersOwn, "node left open: it is your own session", "nodo abierto: es tu propia sesión"),
    ];
    for terminal in [TaskState::Done, TaskState::Cancelled] {
        state.tasks.get_mut(&"t-1".into()).unwrap().state = terminal;
        assert_eq!(now_title(&state, Lang::En), None, "old tasks say nothing about a node");
        for (end, english, spanish) in lines {
            state.tasks.get_mut(&"t-1".into()).unwrap().wrap_up.node = Some(end);
            assert_eq!(now_title(&state, Lang::En).unwrap(), english);
            assert_eq!(now_title(&state, Lang::Es).unwrap(), spanish);
        }
        state.tasks.get_mut(&"t-1".into()).unwrap().wrap_up.node = None;
    }
}

fn opt(id: &str, inputs: Vec<crate::domain::InputSpec>) -> QuestionOption {
    QuestionOption { id: id.into(), label: format!("Option {id}"), description: None, inputs }
}

fn answer_to(option_id: &str) -> crate::domain::Answer {
    crate::domain::Answer { attachments: vec![], option_id: option_id.into(), inputs: Default::default(), responder: "device:1".into(), at_revision: 1 }
}

fn card_state(card: &Card) -> String {
    card.state.as_ref().map(|state| state.text.clone()).unwrap_or_default()
}

#[test]
fn every_open_card_with_options_or_input_can_be_answered_and_a_settled_one_says_so_in_every_client() {
    use crate::domain::{CardKind, Hold, InputKind, InputSpec};
    let note = || vec![InputSpec { id: "note".into(), kind: InputKind::Text, required: false }];
    let option_sets = [vec![], vec![opt("retry", vec![]), opt("later", vec![])], vec![opt("a", note()), opt("b", vec![]), opt("c", vec![])]];
    let kinds = [CardKind::Decide, CardKind::Problem, CardKind::Propose, CardKind::Do, CardKind::Provide, CardKind::Test];
    let holds = [None, Some(Hold::Question), Some(Hold::Permission), Some(Hold::Elicitation)];
    let task_states = [TaskState::NotStarted, TaskState::Running, TaskState::NeedsYou, TaskState::WaitingDependency, TaskState::Done];
    let mut checked = 0;
    for options in &option_sets {
        for kind in kinds {
            for hold in holds {
                for task_state in task_states {
                    for orchestrator in [false, true] {
                        for blocking in [false, true] {
                            for settle in ["open", "answered", "withdrawn"] {
                                let mut state = fixture();
                                state.tasks.get_mut(&"t-1".into()).unwrap().state = task_state;
                                let question = state.interactions.get_mut(&"i-1".into()).unwrap();
                                if orchestrator {
                                    question.author = Author::Orchestrator { generation: 1 };
                                }
                                question.blocking = blocking;
                                question.card = spec(kind, "Choose how to go on", "The task needs it.");
                                let answer = (settle == "answered").then(|| answer_to(options.first().map_or("text", |option| option.id.as_str())));
                                question.withdrawn = settle == "withdrawn";
                                question.payload = if kind == CardKind::Do {
                                    InteractionPayload::ActionRequest { options: options.clone(), answer }
                                } else {
                                    InteractionPayload::Question { options: options.clone(), answer }
                                };
                                if let Some(hold) = hold {
                                    state.holds.insert("i-1".into(), hold);
                                }
                                let pending = with_cards(&state).projection().inbox.iter()
                                    .find(|item| item.id == "interaction:i-1").expect("the card is in the inbox").pending;
                                for client in [Client::Ios, Client::Mac, Client::Tui] {
                                    let view = render(&state, &LiveSnapshot::default(), client, Lang::Es, "card:i-1", "");
                                    let Item::Card(card) = &view.sections[0].items[0] else { panic!("question card") };
                                    let answers = card.actions.iter().chain(&card.more).any(|action| action.id.starts_with("ans:"));
                                    let label = card_state(card);
                                    let case = format!("{kind:?} {hold:?} {task_state:?} orchestrator={orchestrator} blocking={blocking} {settle} options={} {client:?}", options.len());
                                    if pending {
                                        assert!(answers, "an open card has no answer: {case}");
                                    } else {
                                        assert!(!answers, "a settled card offers an answer: {case}");
                                        assert!(!label.starts_with("No bloquea") && !label.starts_with("Bloquea"), "a settled card reads as open ({label}): {case}");
                                    }
                                    checked += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(checked > 1000, "{checked}");
}

#[test]
fn an_open_text_delivery_can_be_accepted_or_sent_back_and_an_accepted_one_says_so_in_every_client() {
    use crate::domain::{Acceptance, DeliveryTarget};
    let mut open = 0;
    let cases = [(TaskState::ForYourReview, false), (TaskState::NeedsYou, false), (TaskState::Running, false), (TaskState::Closing, true), (TaskState::Done, true)];
    for (task_state, accepted) in cases {
        {
            let mut state = fixture();
            state.interactions.retain(|id, _| id.0 == "i-3");
            state.revision = 10;
            state.tasks.get_mut(&"t-1".into()).unwrap().state = task_state;
            let delivery = state.interactions.get_mut(&"i-3".into()).unwrap();
            delivery.author = Author::Agent { agent_id: "a-1".into() };
            delivery.blocking = false;
            delivery.payload = InteractionPayload::Delivery {
                targets: vec![DeliveryTarget::Text { text: "No hay reservas acumuladas.".into() }],
                is_final: true,
                acceptance: accepted.then(|| Acceptance { user_id: "device:1".into(), at_revision: 1, valid: true }),
                feedback: vec![],
            };
            let pending = with_cards(&state).projection().inbox.iter()
                .find(|item| item.id == "interaction:i-3").expect("the delivery is in the inbox").pending;
            open += usize::from(pending);
            for client in [Client::Ios, Client::Mac, Client::Tui] {
                let view = render(&state, &LiveSnapshot::default(), client, Lang::Es, "card:i-3", "");
                let Item::Card(card) = &view.sections[0].items[0] else { panic!("delivery card") };
                let ids: Vec<&str> = card.actions.iter().chain(&card.more).map(|action| action.id.as_str()).collect();
                let case = format!("{task_state:?} accepted={accepted} {client:?} {ids:?}");
                if pending {
                    assert!(ids.iter().any(|id| id.starts_with("accept:")), "an open delivery has no accept: {case}");
                    assert!(ids.iter().any(|id| id.starts_with("changes:")), "an open delivery cannot be sent back: {case}");
                }
                if accepted {
                    assert!(!ids.iter().any(|id| id.starts_with("accept:")), "an accepted delivery offers accept again: {case}");
                    assert_eq!(card_state(card), "Aceptada", "{case}");
                }
            }
        }
    }
    assert!(open > 0);
}

#[test]
fn a_withdrawn_problem_and_an_answered_question_say_why_they_wait_for_nothing() {
    use crate::domain::CardKind;
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::NotStarted;
    let problem = state.interactions.get_mut(&"i-1".into()).unwrap();
    problem.author = Author::Orchestrator { generation: 1 };
    problem.blocking = false;
    problem.card = spec(CardKind::Problem, "No consigo arrancar la tarea", "Cuatro intentos fallan.");
    problem.payload = InteractionPayload::Question {
        options: vec![opt("retry_later", vec![]), opt("user_freed", vec![]), opt("wait_launch_fix", vec![])],
        answer: None,
    };
    problem.withdrawn = true;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("problem card") };
    assert_eq!(card_state(card), "Retirada: ya no hace falta");

    let question = state.interactions.get_mut(&"i-1".into()).unwrap();
    question.withdrawn = false;
    question.blocking = true;
    question.card = spec(CardKind::Decide, "Decide si repito el grabado de snapshots", "Sin ellos no puedo abrir la PR.");
    question.payload = InteractionPayload::Question {
        options: vec![QuestionOption { label: "Repetir ahora".into(), ..opt("retry", vec![]) }, opt("later", vec![])],
        answer: Some(answer_to("retry")),
    };
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "card:i-1", "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("question card") };
    assert_eq!(card_state(card), "Respondida · Repetir ahora");
}

/// The number the Waiting for You header states: every segment of its summary but the last, which
/// counts the nodes working.
fn header_count(summary: &str) -> usize {
    let segments: Vec<&str> = summary.split(" · ").collect();
    segments[..segments.len() - 1]
        .iter()
        .map(|segment| segment.split(' ').next().unwrap().parse::<usize>().unwrap())
        .sum()
}

#[test]
fn the_waiting_header_counts_exactly_the_cards_the_inbox_lists_and_each_one_opens() {
    let mut finished = fixture();
    finished.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    let states = [
        ("fixture", fixture()),
        ("finished task", finished),
        ("pr", pr_fixture()),
        ("golden", golden_fixture()),
        ("plan question", plan_question_fixture()),
        ("across requests", across_requests()),
        ("scheduled run", scheduled_run_fixture(true)),
        ("presented pr", presented_pr()),
        ("failed launch", failed_launch_fixture("boom")),
        ("answered and closed", answered_and_closed_request()),
        ("group", group_state(&[0])),
    ];
    let live = golden_live();
    for (name, state) in &states {
        for lang in [Lang::En, Lang::Es] {
            let home = render(state, &live, Client::Ios, lang, "home", "2026-09-30T10:00:00Z");
            let queue = &home.sections[0];
            assert_eq!(header_count(&home.status.summary), queue.card_ids.len(), "{name} {lang:?}: {}", home.status.summary);
            assert_eq!(queue.card_waits.len(), queue.card_ids.len(), "{name}: every card says what it waits for");
            let questions = queue.card_waits.iter().filter(|wait| *wait == "question").count();
            assert_eq!(home.status.summary.starts_with(&format!("{questions} ")), questions > 0, "{name}: {}", home.status.summary);
            assert_eq!(home.status.badge as usize, queue.card_ids.len(), "{name}");
            for id in &queue.card_ids {
                let card = build(&format!("card:{id}"), RenderContext {
                    domain: &with_cards(state), live: &live, client: Client::Ios, lang,
                    transport: Transport::Loopback, cursor: "cursor-1", rendered_at: "2026-09-30T10:00:00Z",
                });
                assert!(card.is_ok(), "{name}: card {id} is counted but does not open");
            }
        }
    }
}

const TRUST_COMMAND_BODY: &str = "Ejecútalo una vez en el terminal del Mac. Hace una copia de ~/.claude.json:\n\n```\ncp ~/.claude.json ~/.claude.json.bak-$(date +%s) && python3 - <<'PY'\nprint(\"Confiados\")\nPY\n```\n\nSi clonas repos nuevos en ~/Projects, vuelve a ejecutarlo.";

fn know_notice(state: &mut DomainState, need: &str, why: &str, body: &str) {
    let interaction = state.interactions.get_mut(&"i-4".into()).unwrap();
    interaction.payload = InteractionPayload::Information { read_by: BTreeSet::new() };
    interaction.card = spec(crate::domain::CardKind::Know, need, why);
    interaction.body = body.into();
}

#[test]
fn a_know_card_sent_with_only_need_and_body_shows_the_whole_body_with_its_code_block() {
    let mut state = fixture();
    know_notice(&mut state, "Pega este comando en el terminal: confía en todos los repos de ~/Projects", "", TRUST_COMMAND_BODY);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let view = render(&state, &LiveSnapshot::default(), client, Lang::Es, "card:i-4", "");
        assert_eq!(markdown_items(&view).iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), [TRUST_COMMAND_BODY], "{client:?}");
        assert_eq!(card_of(&view).content_row.as_ref().map(|row| row.kind.as_str()), Some("report"), "{client:?}");
    }
}

#[test]
fn a_body_that_only_repeats_the_card_or_holds_alondras_own_data_is_not_shown_again() {
    let mut state = fixture();
    know_notice(&mut state, "The build is green", "All 400 tests pass.", "The build is green\n\nAll 400 tests pass.");
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-4", "");
    assert!(markdown_items(&view).is_empty());
    assert!(card_of(&view).content_row.is_none());
    know_notice(&mut state, "The build is green", "", "{\"machine\":\"data\"}");
    state.interactions.get_mut(&"i-4".into()).unwrap().author = Author::System;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-4", "");
    assert!(markdown_items(&view).is_empty());
}

const KNOW_COMMAND_CARD: &str = "cc2d034b-249e-4487-a89b-57de38c26372";
const KNOW_COMMAND_CASES: [(&str, Client, Lang); 3] = [
    ("card-know-command-es.json", Client::Ios, Lang::Es),
    ("card-know-command-es-mac.json", Client::Mac, Lang::Es),
    ("card-know-command-es-tui.json", Client::Tui, Lang::Es),
];

/// The orchestrator's notice as it reached the user: a headline and a body holding a command in a
/// fenced block, with no separate content.
fn know_command_state() -> DomainState {
    let mut state = golden_fixture();
    let interaction = state.interactions.get_mut(&KNOW_COMMAND_CARD.into()).unwrap();
    interaction.card = spec(crate::domain::CardKind::Know, "Pega este comando en el terminal: confía en todos los repos de ~/Projects", "");
    interaction.body = TRUST_COMMAND_BODY.into();
    state
}

#[test]
fn know_command_cards_match_goldens() {
    let state = know_command_state();
    for ((filename, client, lang), expected) in KNOW_COMMAND_CASES.into_iter().zip([
        include_str!("../../../../fixtures/views/cards/card-know-command-es.json"),
        include_str!("../../../../fixtures/views/cards/card-know-command-es-mac.json"),
        include_str!("../../../../fixtures/views/cards/card-know-command-es-tui.json"),
    ]) {
        let actual = render(&state, &golden_live(), client, lang, &format!("card:{KNOW_COMMAND_CARD}"), "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

#[test]
#[ignore = "explicit fixture generation after Bazel authorization"]
fn generate_know_command_goldens() {
    let dir = std::env::var("ALONDRA_GOLDEN_OUT").expect("ALONDRA_GOLDEN_OUT required");
    let state = know_command_state();
    for (filename, client, lang) in KNOW_COMMAND_CASES {
        let view = render(&state, &golden_live(), client, lang, &format!("card:{KNOW_COMMAND_CARD}"), "2026-09-30T10:00:00Z");
        std::fs::write(format!("{dir}/{filename}"), format!("{}\n", serde_json::to_string_pretty(&view).unwrap())).unwrap();
    }
}
