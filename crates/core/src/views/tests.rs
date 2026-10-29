use super::*;
use crate::domain::{
    Background, NodePhase, Pause, PauseCleanup, PauseReason,
    Agent, Author, Document, DocumentRevision, DomainState, Interaction, InteractionPayload,
    InteractionSubject, PrState, PullRequest, QuestionOption, Recipient, Request, RequestOrigin, Scope, Task,
    TaskState, ModelChooser, TaskModel, inbox_items,
};
use std::collections::{BTreeMap, BTreeSet};

fn inbox_badge(state: &DomainState) -> u32 {
    super::inbox_badge(&crate::domain::ReadIndex::build(state))
}

fn push_content(state: &DomainState, card_id: &str, lang: Lang) -> Result<PushContent, BuildError> {
    super::push_content(&crate::domain::ReadSnapshot::build(std::sync::Arc::new(state.clone())), card_id, lang)
}

fn push_actions(state: &DomainState, card_id: &str, lang: Lang) -> Result<Vec<PushAction>, BuildError> {
    Ok(push_content(state, card_id, lang)?.actions)
}

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
            phase: NodePhase::Working,
            since: 0,
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
            delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: Default::default(), priority: Default::default(), ending: None, stalls: 0, stall_closing: None, worktrees: Default::default(), provider: Default::default(), selection: None, pending_selection: None, account: None, allowed: Default::default(), project: None,
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
    state.settle_nodes();
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
    render_in(domain, live, client, lang, key, time, None)
}

/// `render`, with task t-1 in `in_state` when one is given.
fn render_in(
    domain: &DomainState,
    live: &LiveSnapshot,
    client: Client,
    lang: Lang,
    key: &str,
    time: &str,
    in_state: Option<&alondra_domain::LiveState>,
) -> View {
    let mut domain = with_cards(domain);
    // What older fixtures said with the flags of a live node, as the phase the node machine keeps.
    let unregistered: Vec<_> = domain.tasks.values().filter_map(|task| task.agent_id.clone())
        .filter(|agent| !domain.agents.contains_key(agent)).collect();
    for agent in unregistered {
        domain.agents.insert(agent.clone(), crate::domain::Agent { name: agent.0.clone(), id: agent, phase: NodePhase::Working, since: 0 });
    }
    for paused in live.pacing.iter().flat_map(|pacing| &pacing.paused).filter(|paused| paused.session.is_some()) {
        let reason = match paused.reason {
            LivePauseReason::User => crate::domain::PauseReason::ByUser,
            LivePauseReason::Usage => crate::domain::PauseReason::Usage,
            LivePauseReason::Memory => crate::domain::PauseReason::Memory,
            LivePauseReason::Machine => crate::domain::PauseReason::Declared { reason: paused.waits_for.clone().unwrap_or_default() },
            LivePauseReason::ApiError | LivePauseReason::ApiOutage => crate::domain::PauseReason::ApiError(crate::domain::ApiRetry {
                error: "server_error".into(), detail: None, attempt: paused.retry.as_ref().map_or(1, |retry| retry.attempt), at: 0,
                outage: paused.reason == LivePauseReason::ApiOutage,
            }),
        };
        let agent = domain.tasks.get(&paused.task_id.as_str().into()).and_then(|task| task.agent_id.clone());
        if let Some(agent) = agent.and_then(|agent| domain.agents.get_mut(&agent)) {
            agent.phase = NodePhase::Paused(crate::domain::Pause { reason, since: 0, cut: None, turn_since: None, cleanup: Default::default(), released: false });
        }
    }
    for node in &live.nodes {
        let Some(agent) = domain.agents.get_mut(&node.agent_id.as_str().into()).filter(|agent| agent.phase.live()) else { continue };
        if node.disconnected {
            agent.phase = NodePhase::Closed;
        }
    }
    domain.settle_nodes();
    let mut index = crate::domain::ReadIndex::build(&domain);
    if let Some(state) = in_state {
        index.put_live(&"t-1".into(), state.clone());
    }
    build(
        key,
        RenderContext {
            domain: &domain,
            index: &index,
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
    assert_eq!(inbox_badge(&state), 4, "the orchestrator's question to the user is the user's");
    assert_eq!(inbox_badge(&state), home.status.badge);
    assert_eq!(inbox_badge(&state), home.sections[0].card_ids.len() as u32);
    assert_eq!(inbox_badge(&DomainState::new("empty")), 0);
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
        task.account = Some(crate::accounts::AccountId::parse("work").unwrap());
        task.project = Some("Demo Garden".into());
    }
    state.accounts = golden_accounts().into_iter().map(|account| crate::accounts::AccountStatus {
        provider: crate::accounts::Provider::Claude, id: account.id, name: account.name, state: crate::accounts::AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: account.billing, sign_ins: 0,
    }).collect();
    if let Some(mut agent) = state.agents.remove(&"a-1".into()) {
        agent.id = agent_id.into();
        agent.name = "Alondra · iOS".into();
        agent.phase = NodePhase::WaitingUser { in_turn: false };
        state.agents.insert(agent.id.clone(), agent);
    }
    if let Some(interaction) = state.interactions.get_mut(&"i-1".into()) {
        interaction.body = "The new empty state is ready. Which copy should we ship in the beta?".into();
        interaction.payload = InteractionPayload::Question {
            options: vec![
                QuestionOption {
                    id: "clear".into(),
                    label: "Ship the concise copy".into(),
                    description: Some("Use a short title and one explanatory sentence.".into()),
                },
                QuestionOption {
                    id: "warm".into(),
                    label: "Keep the warmer copy".into(),
                    description: Some("Preserve the more conversational explanation.".into()),
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
                    QuestionOption { id: "ship".into(), label: "Ship it".into(), description: None },
                    QuestionOption { id: "hold".into(), label: "Hold it back".into(), description: None },
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
    add_request(&mut state, "r-new", None, "Import lists from Notes. Keep the order and the checked items.");
    add_request(&mut state, "r-message", None, "[From Claude, for David · 09:58] Launch two more from the queue.");
    state.requests.get_mut(&"r-message".into()).unwrap().message = true;
    add_request(&mut state, "r-answer", Some("Is main green?"), "Is main green right now?");
    let answered = state.requests.get_mut(&"r-answer".into()).unwrap();
    answered.closed = true;
    answered.answer = Some("Yes, since Monday".into());
    add_request(&mut state, "r-scheduled", None, "This run belongs to a scheduled task of Alondra. Run the weekly improvement cycle for Lumo.");
    add_task(&mut state, "t-scheduled-lumo", "r-scheduled", "Lumo · weekly improvement", TaskState::Running, &[]);
    scheduled_run(&mut state, "r-scheduled", "t-scheduled-lumo");
    add_request(&mut state, "r-blurcamera", Some("BlurCamera · weekly improvement"), "Run the weekly improvement cycle for BlurCamera.");
    add_task(&mut state, "t-hf-share-hint", "r-blurcamera", "Photos extension hint after the first blur", TaskState::Running, &[]);
    add_task(&mut state, "t-hf-registry", "r-blurcamera", "Link the website in app.json", TaskState::NotStarted, &[]);
    add_task(&mut state, "t-hf-paywall", "r-blurcamera", "Lifetime offer when the paywall closes", TaskState::Done, &[]);
    add_task(&mut state, "t-hf-post", "r-blurcamera", "Post: blur faces in a video on iPhone", TaskState::Done, &[]);
    for (task, project) in [("t-scheduled-lumo", Some("Lumo")), ("t-hf-share-hint", Some("BlurCamera")), ("t-hf-registry", Some("BlurCamera")),
        ("t-hf-paywall", Some("BlurCamera")), ("t-hf-post", Some("BlurCamera")), ("t-legacy-editor", None), ("t-legacy-notes", None)] {
        state.tasks.get_mut(&task.into()).unwrap().project = project.map(Into::into);
    }
    let paused = Pause { reason: PauseReason::ByUser, since: 0, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
    state.agents.insert("a-hf".into(), Agent { id: "a-hf".into(), name: "BlurCamera hint".into(), phase: NodePhase::Paused(paused), since: 0 });
    for (task, agent) in [("t-badge-count", "a-badge"), ("t-scheduled-lumo", "a-lumo")] {
        state.agents.insert(agent.into(), Agent { id: agent.into(), name: agent.into(), phase: NodePhase::Working, since: 0 });
        state.tasks.get_mut(&task.into()).unwrap().agent_id = Some(agent.into());
    }
    state.tasks.get_mut(&"t-hf-share-hint".into()).unwrap().agent_id = Some("a-hf".into());
    live.nodes.push(LiveNode {
        agent_id: "a-hf".into(), name: "BlurCamera hint".into(),
        state_since: Some("2026-09-30T07:00:00.000Z".into()),
        last_message: Some("Waiting for the snapshot run to finish.".into()),
        last_message_at: Some("2026-09-30T07:00:00.000Z".into()),
        ..LiveNode::default()
    });
    live.launch_refusals.insert("t-hf-registry".into(), LiveLaunchRefusal {
        reason: "workspace_untrusted".into(), detail: Some("~/Projects/indie-apps".into()),
    });
    live.start_waits.insert("t-legacy-notes".into(), LiveStartWait::Retry { at: "10:15".into() });
    for (id, at) in [
        ("r-9f2156b5a8894e53901f0af576dff6a2", "2026-09-30T09:40:00.000Z"),
        ("r-badge", "2026-09-30T09:52:00.000Z"),
        ("r-legacy", "2026-09-29T17:10:00.000Z"),
        ("r-new", "2026-09-30T09:59:00.000Z"),
    ] {
        live.request_times.insert(id.into(), at.into());
    }
    for (id, since, until, ran_secs) in [
        ("t-badge-count", "2026-09-30T09:20:00.000Z", None, None),
        ("t-scheduled-lumo", "2026-09-30T09:48:00.000Z", None, None),
        ("t-legacy-editor", "2026-09-29T17:15:00.000Z", None, None),
        ("t-hf-paywall", "2026-09-29T10:00:00.000Z", Some("2026-09-29T11:05:00.000Z"), Some(3_900)),
    ] {
        live.task_started.insert(id.into(), LiveTaskStart { since: since.into(), until: until.map(Into::into), ran_secs });
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
    let view = render(&state, &live, Client::Ios, Lang::En, "tasks?section=finished&after=0", "");
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
    let all = state.tasks.values().filter(|task| task.active()).count() as u32;
    assert_eq!(all, state.tasks.values().filter(|task| !task.state.terminal()).count() as u32);
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
fn a_message_to_the_orchestrator_is_listed_only_once_it_made_tasks() {
    let (mut state, live) = tasks_fixture();
    let listed = |state: &DomainState| -> Vec<String> {
        fn ids(rows: Vec<Row>) -> Vec<String> {
            rows.into_iter().flat_map(|row| std::iter::once(row.id.clone()).chain(ids(row.children))).collect()
        }
        ["tasks", "tasks?section=finished&after=0"].into_iter()
            .flat_map(|key| render(state, &live, Client::Ios, Lang::En, key, "2026-09-30T10:00:00Z").sections)
            .flat_map(|section| section.items)
            .flat_map(|item| if let Item::Row(row) = item { ids(vec![row]) } else { vec![] })
            .map(|id| id.strip_prefix("group:").map(str::to_owned).unwrap_or(id))
            .collect()
    };
    let counted = |state: &DomainState| -> u32 {
        render(state, &live, Client::Ios, Lang::En, "tasks", "2026-09-30T10:00:00Z").sections.iter()
            .filter(|section| ["active", "finished"].contains(&section.id.as_str()))
            .filter_map(|section| section.count).sum()
    };
    let before = counted(&state);
    assert!(!listed(&state).contains(&"r-message".to_string()), "a message still being handled is no request");
    assert!(listed(&state).contains(&"r-new".to_string()), "a request the user wrote shows before it has tasks");
    state.requests.get_mut(&"r-message".into()).unwrap().closed = true;
    assert!(!listed(&state).contains(&"r-message".to_string()), "nor once it was handled");
    assert_eq!(counted(&state), before, "and no section counts it");
    let mut task = state.tasks.values().next().unwrap().clone();
    (task.id, task.request_id, task.title, task.state) = ("t-from-message".into(), "r-message".into(), "Launch the queue".into(), TaskState::NotStarted);
    state.requests.get_mut(&"r-message".into()).unwrap().closed = false;
    state.tasks.insert(task.id.clone(), task);
    assert!(listed(&state).contains(&"t-from-message".to_string()), "a message that made a task is work like any other");
}

#[test]
fn a_new_request_without_a_title_is_named_by_its_text() {
    let (state, live) = tasks_fixture();
    for lang in [Lang::En, Lang::Es] {
        let (_, row) = active_row(&state, &live, Client::Ios, lang, "r-new");
        assert_eq!(row.title, "Import lists from Notes");
    }
    let detail = render(&state, &live, Client::Ios, Lang::Es, "task:t-legacy-notes", "2026-09-30T10:00:00Z");
    let request = detail.sections.iter().flat_map(|section| &section.items)
        .find_map(|item| if let Item::Facts(facts) = item { facts.items.iter().find(|fact| fact.label == "Solicitud") } else { None })
        .expect("the task's request");
    assert_eq!(request.value, "Look at the schedule editor and write the notes for the beta");
    assert_eq!(super::tasks::summary("  \nFix the crash when the schedule list is empty? It happens on launch."),
        "Fix the crash when the schedule list is empty?");
    assert_eq!(super::tasks::summary("En iOS en la pantalla de nuevo de Alondra deberíamos hacer dismiss del teclado al enviar y además"),
        "En iOS en la pantalla de nuevo de Alondra deberíamos hacer…");
    assert_eq!(super::tasks::summary("v0.2.57 is out"), "v0.2.57 is out");
}

#[test]
fn pr_handling_says_what_happens_to_the_pr() {
    let mut state = fixture();
    let live = LiveSnapshot::default();
    let handling = |state: &DomainState| -> String {
        render(state, &live, Client::Ios, Lang::Es, "task:t-1", "").sections.iter().flat_map(|section| &section.items)
            .find_map(|item| if let Item::Facts(facts) = item {
                facts.items.iter().find(|fact| fact.label == "Gestión de PR").map(|fact| fact.value.clone())
            } else { None })
            .expect("PR handling")
    };
    state.tasks.get_mut(&"t-1".into()).unwrap().review_prs = true;
    assert_eq!(handling(&state), "La revisas tú; al aceptarla, Alondra hace el merge");
    state.tasks.get_mut(&"t-1".into()).unwrap().review_prs = false;
    assert_eq!(handling(&state), "Merge automático: Alondra hace el merge al pasar los checks");
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
    for (filename, client, lang, expected) in [
        ("tasks-ios-finished.json", Client::Ios, Lang::En, include_str!("../../../../fixtures/views/cards/tasks-ios-finished.json")),
        ("tasks-ios-es-finished.json", Client::Ios, Lang::Es, include_str!("../../../../fixtures/views/cards/tasks-ios-es-finished.json")),
        ("tasks-mac-finished.json", Client::Mac, Lang::En, include_str!("../../../../fixtures/views/cards/tasks-mac-finished.json")),
    ] {
        let actual = render(&state, &live, client, lang, "tasks?section=finished&after=0", "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

/// A task in every state the user meets, each in its own project, and not one live node: what the
/// list shows of a task is the task's, never its node's.
#[test]
fn every_task_shows_its_project_title_and_state_without_a_live_node() {
    let mut state = golden_fixture();
    let base = state.tasks[&NODE_CLOSED_TASK.into()].clone();
    let mut request = Request::new("r-states".into(), "Work in every state".into());
    request.title = Some("Every state".into());
    state.requests.insert(request.id.clone(), request);
    state.agents.insert("a-working".into(), Agent { id: "a-working".into(), name: "Worker".into(), phase: NodePhase::Working, since: 0 });
    let cases: [(&str, &str, TaskState, Option<&str>, bool, &str, &str); 7] = [
        ("t-queued", "Queue the import", TaskState::NotStarted, None, false, "Kiosk", "Stopped"),
        ("t-working", "Parse the lists", TaskState::Running, Some("a-working"), false, "Quill", "Working"),
        ("t-waiting", "Wait for the parser", TaskState::WaitingDependency, None, false, "Lumo", "Waiting"),
        ("t-delivered", "Deliver the export", TaskState::ForYourReview, None, false, "Redraft", "Delivered"),
        ("t-done", "Ship the beta", TaskState::Done, None, false, "MealScale", "Finished"),
        ("t-cancelled", "Drop the old sync", TaskState::Cancelled, None, false, "BlurCamera", "Cancelled"),
        ("t-archived", "Clean up the logs", TaskState::Done, None, true, "Matte", "Finished"),
    ];
    for (id, title, task_state, agent, archived, project, _) in cases {
        let mut task = base.clone();
        (task.id, task.request_id, task.title, task.state, task.archived) = (id.into(), "r-states".into(), title.into(), task_state, archived);
        (task.agent_id, task.final_presentation, task.pr_id, task.started) = (agent.map(Into::into), None, None, task_state != TaskState::NotStarted);
        task.dependencies = if id == "t-waiting" { ["t-working".into()].into() } else { BTreeSet::new() };
        task.project = Some(project.into());
        state.tasks.insert(task.id.clone(), task);
    }
    let live = LiveSnapshot::default();
    assert!(live.nodes.is_empty());
    fn find<'v>(rows: &'v [Row], id: &str) -> Option<&'v Row> {
        rows.iter().find_map(|row| if row.id == id { Some(row) } else { find(&row.children, id) })
    }
    let row_of = |view: &View, id: &str| -> Option<Row> {
        let rows: Vec<Row> = view.sections.iter().flat_map(|section| &section.items)
            .filter_map(|item| if let Item::Row(row) = item { Some(row.clone()) } else { None }).collect();
        find(&rows, id).cloned()
    };
    let mut wrong = vec![];
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let open = render(&state, &live, client, Lang::En, "tasks", "2026-09-30T10:00:00Z");
        let finished = render(&state, &live, client, Lang::En, "tasks?section=finished&after=0", "2026-09-30T10:00:00Z");
        for (id, title, _, _, archived, project, shown) in cases {
            let detail = render(&state, &live, client, Lang::En, &format!("task:{id}"), "2026-09-30T10:00:00Z");
            let facts: Vec<(String, String)> = detail.sections.iter().flat_map(|section| &section.items)
                .filter_map(|item| if let Item::Facts(facts) = item { Some(&facts.items) } else { None }).flatten()
                .map(|fact| (fact.label.clone(), fact.value.clone())).collect();
            assert!(facts.contains(&("Project".into(), project.into())), "{client:?}: the detail of {id} names its project: {facts:?}");
            if archived {
                assert!(row_of(&open, id).is_none() && row_of(&finished, id).is_none(), "{client:?}: an archived task is listed nowhere");
                continue;
            }
            let row = row_of(&open, id).or_else(|| row_of(&finished, id)).unwrap_or_else(|| panic!("{client:?}: {id} is listed"));
            assert_eq!(row.title, title, "{client:?}: {id}");
            let badges: Vec<&str> = row.badges.iter().map(|badge| badge.text.as_str()).collect();
            assert!(badges.contains(&project), "{client:?}: {id} shows its project: {badges:?}");
            let said = row.subtitle.as_deref().is_some_and(|text| text.to_lowercase().contains(&shown.to_lowercase())
                || (id == "t-waiting" && text == "waits for Parse the lists"));
            if !badges.contains(&shown) && !said {
                wrong.push(format!("{client:?}: {id} shows {shown}: {badges:?} {:?}", row.subtitle));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_task_shows_the_same_title_folder_cost_model_and_state_with_its_node_gone_closed_or_relaunched() {
    let mut base = fixture();
    let task = base.tasks.get_mut(&"t-1".into()).unwrap();
    task.model = Some(TaskModel { model: "opus".into(), chosen_by: ModelChooser::Orchestrator });
    task.project = Some("Alondra".into());
    task.worktrees.insert("/work/alondra/.claude/worktrees/subagent".into(), crate::domain::TaskWorktree {
        origin: crate::domain::WorktreeOrigin::Subagent, branch: None, discard: false, end: None });
    task.worktrees.insert("/work/alondra/.claude/worktrees/t-1".into(), crate::domain::TaskWorktree {
        origin: crate::domain::WorktreeOrigin::Task, branch: None, discard: false, end: None });
    let mut live = LiveSnapshot::default();
    live.task_costs.insert("t-1".into(), 4_200);
    let node = |agent: &str, disconnected: bool| LiveNode {
        agent_id: agent.into(), cwd: Some(format!("/elsewhere/{agent}")), cost_cents: Some(13), model: Some("claude-opus-5-5".into()),
        project: Some("Elsewhere".into()), disconnected, ..Default::default()
    };

    let no_node = base.clone();
    let mut closed = base.clone();
    closed.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let mut closed_live = live.clone();
    closed_live.nodes.push(node("a-1", true));
    let mut relaunched = base.clone();
    relaunched.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-2".into());
    relaunched.agents.insert("a-2".into(), Agent { id: "a-2".into(), name: "Node Two".into(), phase: NodePhase::Working, since: 0 });
    let mut relaunched_live = live.clone();
    relaunched_live.nodes.push(node("a-2", false));

    let shown = |state: &DomainState, live: &LiveSnapshot| {
        let detail = render(state, live, Client::Ios, Lang::En, "task:t-1", "2026-09-30T10:00:00Z");
        let facts: BTreeMap<String, String> = detail.sections.iter().flat_map(|section| &section.items)
            .filter_map(|item| if let Item::Facts(facts) = item { Some(&facts.items) } else { None }).flatten()
            .filter(|fact| ["Folder", "Cost", "Model", "Project"].contains(&fact.label.as_str()))
            .map(|fact| (fact.label.clone(), fact.value.clone())).collect();
        let view = render(state, live, Client::Ios, Lang::En, "card:i-1", "2026-09-30T10:00:00Z");
        let Item::Card(card) = &view.sections[0].items[0] else { panic!("the question's card") };
        let line = card.context.iter().find(|line| line.icon == Icon::Task).expect("the card names its task");
        (facts, card.meta.clone(), card.state.as_ref().map(|state| state.text.clone()), line.text.clone(),
            line.state.as_ref().map(|state| state.text.clone()))
    };
    let expected = shown(&no_node, &live);
    assert_eq!(expected.0, BTreeMap::from([
        ("Folder".into(), "/work/alondra/.claude/worktrees/t-1".into()),
        ("Cost".into(), "$42.00".into()),
        ("Model".into(), "Opus · chosen by the orchestrator".into()),
        ("Project".into(), "Alondra".into()),
    ]), "the detail reads the task, not a node");
    assert_eq!(expected.1.as_deref(), Some("Build a feature · Alondra"), "the card speaks for its task");
    assert_eq!(expected.2.as_deref(), Some("Blocks Build a feature"));
    assert_eq!(expected.3, "Build a feature");
    assert!(expected.4.is_some(), "the card says where its task stands without a node");
    assert_eq!(shown(&closed, &closed_live), expected, "with its node closed");
    assert_eq!(shown(&relaunched, &relaunched_live), expected, "relaunched on another node");
}

const SETTLED_GOLDENS: [(&str, Client, Lang); 4] = [
    ("card-settled-ios.json", Client::Ios, Lang::En),
    ("card-settled-ios-es.json", Client::Ios, Lang::Es),
    ("card-settled-mac.json", Client::Mac, Lang::En),
    ("card-settled-mac-es.json", Client::Mac, Lang::Es),
];
const SETTLED_CARD: &str = "a98d034b-249e-4487-a89b-57de38c26372";

/// The golden question once answered from another device: its key renders only why it went.
fn settled_fixture() -> DomainState {
    let mut state = golden_fixture();
    let question = state.interactions.get_mut(&SETTLED_CARD.into()).unwrap();
    let InteractionPayload::Question { options, answer } = &mut question.payload else { panic!("the golden question") };
    *answer = Some(crate::domain::Answer {
        attachments: vec![], option_id: options[0].id.clone(), inputs: Default::default(), responder: "device:2".into(), at_revision: 1, decision: None,
    });
    state
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

/// Principal and Trabajo on subscriptions, and API paid per use.
fn golden_accounts() -> Vec<crate::accounts::ProviderAccount> {
    use crate::accounts::{AccountBilling, AccountId, ProviderAccount};
    let account = |id: &str, name: &str, billing| ProviderAccount { id: AccountId::parse(id).unwrap(), name: name.into(), enabled: true, billing };
    vec![account("default", "Principal", AccountBilling::default()), account("work", "Trabajo", AccountBilling::default()),
        account("api", "API", AccountBilling::PayPerUse)]
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
        fetched: true,
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
    live.task_started.insert("t-651c38484cd2f34e74d5107ba05f7f15".into(), LiveTaskStart {
        since: "2026-09-30T08:00:00Z".into(), until: None, ran_secs: None,
    });
    live.projects.push(crate::options::ProjectEntry {
        name: "Demo Garden".into(),
        path: "/demo/Demo Garden".into(),
        last_used: Some(1_759_226_280_000),
        nodes: 1,
    });
    let accounts = crate::accounts::AccountsConfig { claude: golden_accounts(), ..Default::default() };
    live.options = crate::options::OptionsFile { accounts, ..Default::default() }.catalog(None);
    live.gauges = vec![
        Gauge { id: "memory".into(), label: "Memory".into(), value: 0.38,
            text: "6.1 GB of 16 GB".into(), detail: None, level: Level::Calm, parts: vec![] },
        Gauge { id: "disk".into(), label: "Disk available".into(), value: 0.72,
            text: "120 GB".into(), detail: None, level: Level::Calm, parts: vec![] },
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
        fetched: true, conflicts: true, stacked_on: None, url: None,
        ..Default::default()
    });
    let view = render(&state, &live, Client::Ios, Lang::En, &format!("card:{pr_id}"), "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("the card remains visible") };
    assert!(!card.actions.iter().any(|action| action.label == "Request changes"));
    assert!(card.more.iter().any(|action| action.label == "Close without merging"),
        "a valid action after the invalid action should still render");
    let result = build(&format!("card:{pr_id}"), RenderContext {
        domain: &state,
        index: &crate::domain::ReadIndex::build(&state),
        live: &live,
        client: Client::Ios,
        lang: Lang::En,
        transport: Transport::Loopback,
        cursor: "test",
        rendered_at: "",
    });
    assert!(result.is_ok());
}

/// `ALONDRA_GOLDEN_OUT=<dir>` writes what the views render now there, to review and copy over the
/// goldens, and compares nothing, so one run writes every golden.
fn assert_golden(filename: &str, view: &View, expected: &str) {
    if let Ok(dir) = std::env::var("ALONDRA_GOLDEN_OUT") {
        std::fs::write(format!("{dir}/{filename}"), format!("{}\n", serde_json::to_string_pretty(view).unwrap())).unwrap();
        return;
    }
    let actual = serde_json::to_value(view).unwrap();
    let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
    assert_eq!(actual, expected, "{filename}");
}

#[test]
fn view_keys_are_addressable_and_bound_to_epoch() {
    let mut state = fixture();
    state.agents.get_mut(&"a-1".into()).unwrap().phase = NodePhase::Working;
    let live = LiveSnapshot::default();
    for (key, client) in [
        ("sidebar", Client::Mac),
        ("tasks", Client::Ios),
        ("task:t-1", Client::Ios),
        ("request:r-1", Client::Mac),
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
                index: &crate::domain::ReadIndex::build(&state),
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
fn search_finds_tasks_by_title_and_by_what_their_node_said() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let keys = |view: &View| view.sections[0].items.iter().filter_map(|item| match item {
        Item::Row(row) => row.link.as_ref().map(|link| match link { Link::View { key } => key.clone(), _ => String::new() }),
        _ => None,
    }).collect::<Vec<_>>();
    let found = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "search?q=feature", "");
    assert_eq!(found.sections[0].title.as_deref(), Some("1 result found"));
    assert_eq!(keys(&found), ["task:t-1"]);
    let said = LiveSnapshot { search_matches: Some(vec!["a-1".into()]), ..LiveSnapshot::default() };
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let view = render(&state, &said, client, Lang::En, "search?q=what+it+said", "");
        assert_eq!(keys(&view), ["task:t-1"], "{client:?}: the session that matched is its task");
    }
    let missing = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "search?q=NoSuchTask", "");
    assert_eq!(missing.empty.as_ref().map(|notice| notice.text.as_str()), Some("No results found"));
}

#[test]
fn no_view_links_to_a_node() {
    let state = golden_in_progress();
    let live = golden_live();
    for (key, client) in [
        ("home", Client::Ios), ("home", Client::Tui),
        ("card:a98d034b-249e-4487-a89b-57de38c26372", Client::Ios),
        ("card:a98d034b-249e-4487-a89b-57de38c26372", Client::Mac),
        ("card:a98d034b-249e-4487-a89b-57de38c26372", Client::Tui),
        ("card:f12d034b-249e-4487-a89b-57de38c26372", Client::Ios),
        ("tasks", Client::Ios), ("tasks", Client::Mac), ("tasks", Client::Tui),
        ("task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Ios),
        ("task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Mac),
        ("task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Tui),
        ("request:r-9f2156b5a8894e53901f0af576dff6a2", Client::Mac),
        ("sidebar", Client::Mac),
        ("search?q=Alondra", Client::Ios), ("search?q=Alondra", Client::Mac),
        ("orchestrator", Client::Tui),
    ] {
        let json = serde_json::to_string(&render(&state, &live, client, Lang::En, key, "2026-09-30T10:00:00Z")).unwrap();
        assert!(!json.contains("\"node:"), "{key} on {client:?} links to a node: {json}");
    }
}

/// The golden domain with its task worked on by its node right now.
fn golden_in_progress() -> DomainState {
    let mut state = golden_fixture();
    let agent = "d887a34b-249e-4487-a89b-57de38c26372";
    let task = state.tasks.get_mut(&"t-651c38484cd2f34e74d5107ba05f7f15".into()).unwrap();
    task.agent_id = Some(agent.into());
    task.state = TaskState::Running;
    state.agents.get_mut(&agent.into()).unwrap().phase = NodePhase::Working;
    state
}

#[test]
fn the_mac_sidebar_lists_the_tasks_in_progress_by_project() {
    let state = golden_in_progress();
    let mut live = golden_live();
    let agent = "d887a34b-249e-4487-a89b-57de38c26372";
    let node = live.nodes.iter_mut().find(|node| node.agent_id == agent).unwrap();
    node.background_badge = Some("2 subagents".into());
    let project = node.project.clone().unwrap();
    let view = render(&state, &live, Client::Mac, Lang::En, "sidebar", "");
    let section = view.sections.iter().find(|section| section.id == format!("project:{project}")).expect("the project's section");
    let [Item::Row(row)] = section.items.as_slice() else { panic!("one task in progress: {:?}", section.items) };
    assert_eq!(row.link, Some(Link::View { key: "task:t-651c38484cd2f34e74d5107ba05f7f15".into() }));
    assert_eq!(row.title, "Restore the native review experience");
    assert_eq!(row.badges[0], Label { text: "Needs you".into(), tone: Tone::Warning, elapsed: None }, "its task's state");
    assert_eq!(row.badges[1], Label { text: "2 subagents".into(), tone: Tone::Neutral, elapsed: None }, "and what runs in its background");
    assert!(row.children.is_empty());
    assert!(view.status.summary.ends_with(" · 1 in progress"), "{}", view.status.summary);
}

#[test]
fn the_status_counts_the_tasks_in_progress() {
    let state = golden_in_progress();
    let live = golden_live();
    for (lang, label) in [(Lang::En, "In progress"), (Lang::Es, "En marcha")] {
        let view = render(&state, &live, Client::Tui, lang, "home", "");
        let count = view.status.counts.iter().find(|count| count.id == "agents").unwrap();
        assert_eq!((count.label.as_str(), count.value), (label, 1));
        assert!(view.status.counts.iter().all(|count| !count.label.to_lowercase().contains("agent")));
    }
    let mut done = state.clone();
    for task in done.tasks.values_mut() { task.state = TaskState::Done; }
    let view = render(&done, &live, Client::Tui, Lang::En, "home", "");
    assert_eq!(view.status.counts.iter().find(|count| count.id == "agents").unwrap().value, 0,
        "a node whose task finished is not a task in progress");
}

#[test]
fn the_orchestrator_key_holds_only_the_message_to_alondra() {
    let state = fixture();
    let live = LiveSnapshot::default();
    let view = render(&state, &live, Client::Tui, Lang::En, "orchestrator", "");
    assert_eq!(view.key, "orchestrator");
    let actions: Vec<_> = view.sections.iter().flat_map(|section| &section.items).filter_map(|item|
        if let Item::Actions(bar) = item { Some(bar.actions.iter().map(|action| action.id.clone())) } else { None }).flatten().collect();
    assert_eq!(actions, [format!("msg:view@{}", state.revision)]);
}

#[test]
fn no_view_answers_the_fleet_or_a_node() {
    for key in ["fleet", "node:a-1", "node:orchestrator"] {
        assert_eq!(Family::of(key), None, "{key}");
    }
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
    assert_eq!(document_links(&view), ["document:revision-1"], "the card links the document under review");
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "document:revision-1", "");
    let content = view.sections.iter().flat_map(|section| &section.items).find_map(|item|
        if let Item::ContentRef(content) = item { Some(content) } else { None })
        .expect("the document's screen exposes the revision content");
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
            description: None }],
        answer: None,
    };
    question.subject = Some(InteractionSubject::DocumentReview { document_id: "plan".into(), revision_id: "plan-r1".into() });
    state
}

fn document_links(view: &View) -> Vec<&str> {
    view.sections.iter().filter(|section| section.id == "documents").flat_map(|section| &section.items).flat_map(|item| match item {
        Item::Facts(facts) => facts.items.iter().filter_map(|fact| match &fact.link {
            Some(Link::View { key }) => Some(key.as_str()),
            _ => None,
        }).collect(),
        _ => vec![],
    }).collect()
}

fn document_content(view: &View) -> Option<&ContentRef> {
    view.sections.iter().flat_map(|section| &section.items).find_map(|item|
        if let Item::ContentRef(content) = item { Some(content) } else { None })
}

#[test]
fn a_question_about_a_plan_opens_the_plan_for_review_with_its_options_and_request_changes() {
    let state = plan_question_fixture();
    let document = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "document:plan-r1", "");
    let content = document_content(&document).expect("the plan's own screen shows it");
    assert_eq!((content.kind, content.fetch.as_str(), content.commentable),
        (ContentKind::Document, "/document-revisions/plan-r1/content", true));
    let bar: Vec<&str> = document.sections.iter().flat_map(|section| &section.items).flat_map(|item|
        if let Item::Actions(bar) = item { bar.actions.iter().map(|action| action.id.as_str()).collect() } else { vec![] }).collect();
    assert!(bar.iter().any(|id| id.starts_with("changes:i-1@")), "its screen asks for changes with comments: {bar:?}");
    for key in ["card:plan-r1", "card:i-1"] {
        let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, key, "");
        assert!(document_content(&view).is_none(), "{key} links the plan instead of holding it");
        assert_eq!(document_links(&view), ["document:plan-r1"], "{key}");
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
    let mut item = inbox_items(&domain).into_iter().find(|item| item.id == format!("interaction:{interaction}")).expect("the card exists");
    for action in &mut item.available_actions {
        action.enabled = true;
        action.disabled_reason = None;
    }
    let index = crate::domain::ReadIndex::build(&domain);
    let context = RenderContext { domain: &domain, index: &index, live: &live, client: Client::Ios, lang, transport: Transport::Loopback,
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
    assert!(matches!(&reply.input, Input::Text { placeholder, required: true, .. } if placeholder == "Reply to Build a feature…"));
    assert_eq!(reply.stamp.as_deref(), Some("Sent"));
}

#[test]
fn delivery_reply_placeholder_is_localized_and_names_the_task() {
    let state = fixture();
    let spanish = delivery_card(&state, Lang::Es, "card:i-3");
    let reply = reply_to_node(&spanish);
    assert_eq!(reply.label, "Responder");
    assert!(matches!(&reply.input, Input::Text { placeholder, .. } if placeholder == "Responder a Build a feature…"));
    let english = delivery_card(&state, Lang::En, "card:i-3");
    assert!(matches!(&reply_to_node(&english).input, Input::Text { placeholder, .. } if placeholder == "Reply to Build a feature…"));
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
fn a_card_writes_only_in_its_own_field_and_every_option_is_a_button_that_answers_when_pressed() {
    let view = render(&fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("question card") };
    assert!(card.actions.iter().chain(&card.more).all(|action| !action.id.starts_with("ans:")), "no button opens a text box");
    let reply = card.reply.as_ref().expect("the card's own field answers it");
    assert!(reply.id.starts_with("reply:i-1@"), "{}", reply.id);
    assert!(matches!(&reply.input, Input::Text { multiline: true, .. }));
    assert!(reply.attachments);

    let view = render(&golden_fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En,
        "card:a98d034b-249e-4487-a89b-57de38c26372", "");
    let Item::Card(card) = &view.sections[0].items[0] else { panic!("decision card") };
    assert_eq!(card.kind, Some(crate::domain::CardKind::Decide));
    assert_eq!(card.actions.iter().map(|a| a.option_id.as_deref()).collect::<Vec<_>>(), [Some("clear"), Some("warm")]);
    assert!(card.actions.iter().chain(&card.more).filter(|action| action.id.starts_with("ans:"))
        .all(|action| action.input == Input::None && !action.attachments));
    assert!(card.more.iter().all(|action| !action.id.starts_with("ans:")), "its own words go in the card's field, not behind More");
    assert!(card.reply.as_ref().is_some_and(|reply| reply.id.starts_with("reply:")));
}

#[test]
fn an_open_card_whose_options_asked_for_text_shows_them_as_buttons_that_answer_at_once() {
    let mut state = fixture();
    let legacy: Vec<QuestionOption> = serde_json::from_value(serde_json::json!([
        {"id": "done", "label": "Done", "inputs": [{"id": "note", "kind": "text", "required": false}]},
        {"id": "other", "label": "Other", "inputs": [{"id": "which", "kind": "text", "required": true}]},
    ])).unwrap();
    let question = state.interactions.get_mut(&"i-1".into()).unwrap();
    question.card = spec(crate::domain::CardKind::Do, "Rotate the key", "The old one leaked.");
    question.payload = InteractionPayload::ActionRequest { options: legacy, answer: None };
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let view = render(&state, &LiveSnapshot::default(), client, Lang::Es, "card:i-1", "");
        let card = card_of(&view);
        let buttons = answers(card);
        assert_eq!(buttons.iter().map(|a| a.option_id.as_deref().unwrap()).collect::<Vec<_>>(), ["done", "other"], "{client:?}");
        assert!(buttons.iter().all(|action| action.input == Input::None && !action.attachments), "{client:?}: {buttons:?}");
    }
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
        base_ref: Some("main".into()), conflicts: false,
        stacked_on: None, url: None,
        ..Default::default()
    });
    let view = render(&state, &live, Client::Tui, Lang::En, "pull_request:pr-1", "");
    assert_eq!(view.title.as_deref(), Some("Readable task editor"));
    let facts = section_of(&view, "facts").unwrap().items.iter().find_map(|item| {
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
    state.tasks.get_mut(&"t-1".into()).unwrap().project = Some("Alondra".into());
    let mut live = LiveSnapshot::default();
    live.task_options.insert("t-1".into(), LiveTaskOptions { effort: Some("high".into()) });
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

fn task_state_text(state: &DomainState, live: &LiveSnapshot, lang: Lang) -> String {
    let row = active_row(state, live, Client::Mac, lang, "t-1").1;
    row.badges.first().map(|badge| badge.text.clone()).unwrap()
}

/// The task's state and the one fact that explains it, as the Mac shows them: the state as its badge, the fact
/// under the title.
fn task_line(state: &DomainState, live: &LiveSnapshot, lang: Lang) -> String {
    let row = active_row(state, live, Client::Mac, lang, "t-1").1;
    [row.badges.first().map(|badge| badge.text.clone()), row.subtitle].into_iter().flatten().collect::<Vec<_>>().join(" · ")
}

#[test]
fn without_a_claude_session_the_banner_and_the_queue_say_so_and_never_that_usage_ran_out() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::NotStarted;
    let waiting = LiveSnapshot { waits_for_session: true, start_waits: [("t-1".into(), LiveStartWait::Room)].into(), ..LiveSnapshot::default() };
    let status = |live: &LiveSnapshot, lang| render(&state, live, Client::Ios, lang, "tasks", "").status;
    assert_eq!(status(&waiting, Lang::Es).session_wait.map(|notice| notice.text).as_deref(),
        Some("No hay ninguna cuenta de proveedor disponible · revisa Cuentas"));
    assert_eq!(status(&waiting, Lang::En).session_wait.map(|notice| notice.text).as_deref(),
        Some("No provider account is available · check Accounts"));
    assert_eq!(status(&waiting, Lang::Es).usage_exhausted, None, "no quota banner");
    assert_eq!(task_line(&state, &waiting, Lang::Es), "En cola · Esperando una sesión de proveedor · arranca cuando una cuenta la tenga");
    assert_eq!(task_line(&state, &waiting, Lang::En), "Queued · Waiting for a provider session · starts once an account has one");
    let mut refused = waiting.clone();
    refused.start_waits.clear();
    refused.launch_refusals.insert("t-1".into(), LiveLaunchRefusal { reason: "usage_exhausted".into(), detail: None });
    assert_eq!(task_line(&state, &refused, Lang::Es), "No arrancó · Esperando una sesión de proveedor");
}

#[test]
fn with_usage_out_the_banner_and_the_queue_still_name_the_quota_and_its_time() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::NotStarted;
    let out = LiveSnapshot {
        usage_exhausted: Some(live::LiveUsageExhausted { since: "2026-10-24T10:00:00.000+02:00".into(), until: "2026-10-24T14:30:00.000+02:00".into(), paused_by_user: false }),
        start_waits: [("t-1".into(), LiveStartWait::Room)].into(),
        ..LiveSnapshot::default()
    };
    let status = render(&state, &out, Client::Ios, Lang::Es, "tasks", "").status;
    assert_eq!(status.usage_exhausted.map(|usage| usage.until).as_deref(), Some("2026-10-24T14:30:00.000+02:00"));
    assert_eq!(status.session_wait, None, "usage that ran out is not a missing session");
    assert_eq!(task_line(&state, &out, Lang::Es), "En cola · Límite de uso alcanzado · arranca cuando vuelva");
}

#[test]
fn a_task_that_has_not_started_says_what_refused_its_launch() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::NotStarted;
    let mut live = LiveSnapshot::default();
    assert_eq!(task_line(&state, &live, Lang::Es), "Detenida · Su sesión terminó sin que entregara",
        "launched before and its session gone: never «Esperando a que el orquestador la lance»");
    assert_eq!(task_line(&state, &live, Lang::En), "Stopped · Its session ended before it delivered");
    live.start_waits.insert("t-1".into(), LiveStartWait::Retry { at: "14:05".into() });
    live.launch_refusals.insert("t-1".into(), LiveLaunchRefusal { reason: "launch_failed".into(), detail: Some("claude exited".into()) });
    assert_eq!(task_line(&state, &live, Lang::Es), "No arrancó · Reintento a las 14:05");
    assert_eq!(task_line(&state, &live, Lang::En), "Didn't start · Trying again at 14:05");
    assert_eq!(active_row(&state, &live, Client::Mac, Lang::Es, "t-1").0, "block:waiting", "it tries again by itself");
    live.launch_refusals.clear();
    live.start_waits.insert("t-1".into(), LiveStartWait::Room);
    assert_eq!(task_line(&state, &live, Lang::Es), "En cola · Arranca cuando haya sitio", "with a plan it is queued, not «Arrancando»");
    assert_eq!(active_row(&state, &live, Client::Mac, Lang::Es, "t-1").0, "block:waiting");
    live.start_waits.clear();
    let mut base = state.tasks[&"t-1".into()].clone();
    base.id = "t-base".into();
    base.title = "Build the base".into();
    base.state = TaskState::Running;
    state.tasks.insert(base.id.clone(), base);
    state.tasks.get_mut(&"t-1".into()).unwrap().dependencies = ["t-base".into()].into();
    let detail = serde_json::to_string(&render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "")).unwrap();
    assert!(detail.contains("Espera a «Build the base»"), "{detail}");
    state.tasks.remove(&"t-base".into());
    state.tasks.get_mut(&"t-1".into()).unwrap().dependencies = Default::default();
    live.launch_refusals.insert("t-1".into(), LiveLaunchRefusal {
        reason: "workspace_untrusted".into(), detail: Some("~/Projects/indie-apps".into()),
    });
    assert_eq!(task_line(&state, &live, Lang::Es), "No arrancó · Claude Code no confía en ~/Projects/indie-apps");
    assert_eq!(task_line(&state, &live, Lang::En), "Didn't start · Claude Code doesn't trust ~/Projects/indie-apps");
    assert_eq!(active_row(&state, &live, Client::Mac, Lang::Es, "t-1").0, "block:waiting", "no card asks for it, so it is not «Para ti»");
    live.launch_refusals.insert("t-1".into(), LiveLaunchRefusal { reason: "usage_exhausted".into(), detail: None });
    assert_eq!(task_line(&state, &live, Lang::Es), "No arrancó · Límite de uso alcanzado");
    live.launch_refusals.insert("t-1".into(), LiveLaunchRefusal {
        reason: "launch_failed".into(), detail: Some("claude exited with 1\nmore".into()),
    });
    assert_eq!(task_line(&state, &live, Lang::Es), "No arrancó · claude exited with 1");
    assert!(!task_line(&state, &live, Lang::Es).contains("Sin empezar"));
}

/// 14 October: t-holepunch-iroh-prototype ended its turn with no card and said «Sin actividad» on
/// every screen. A running task's node works or waits on a card in front of the user: no phase of
/// it reads as doing nothing, and one at rest is in «For you» with the card that holds its words.
#[test]
fn a_running_task_never_says_no_activity_whatever_its_node_does() {
    let work = Background { work: crate::model::BackgroundKind::Shell, label: "./listen-at-home.sh".into(), since: 0, others: 0, gone: None };
    for phase in [NodePhase::Working, NodePhase::WaitingBackground(work), NodePhase::Resuming { idle_since: None, after_wait: false },
        NodePhase::Resuming { idle_since: Some(0), after_wait: false }, NodePhase::WaitingUser { in_turn: false }, NodePhase::WaitingUser { in_turn: true }]
    {
        let mut state = fixture();
        node_in(&mut state, phase.clone());
        let mut live = LiveSnapshot::default();
        live.nodes.push(LiveNode { agent_id: "a-1".into(), name: "Node One".into(), ..LiveNode::default() });
        for (screen, text) in every_screen(&state, &live) {
            for wrong in ["Sin actividad", "No activity", "Detenido"] {
                assert!(!text.contains(wrong), "{phase:?} on {screen} says {wrong}: {text}");
            }
        }
    }
}

#[test]
fn a_running_task_whose_node_session_closed_says_it_stopped_never_working_nor_no_connection() {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    task.state = TaskState::Running;
    task.agent_id = Some("a-1".into());
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode { agent_id: "a-1".into(), name: "Node One".into(), disconnected: true, ..LiveNode::default() });
    assert_eq!(task_state_text(&state, &live, Lang::Es), "Detenida");
    assert_eq!(task_state_text(&state, &live, Lang::En), "Stopped");
    let (block, row) = active_row(&state, &live, Client::Mac, Lang::Es, "t-1");
    assert_eq!((block.as_str(), row.subtitle.as_deref()), ("block:for-you", Some("Su sesión se cerró sin que entregara")));
    live.nodes[0].disconnected = false;
    let mut reviving = state.clone();
    reviving.agents.insert("a-1".into(), Agent { id: "a-1".into(), name: "Node One".into(), phase: NodePhase::Starting { reviving: true, idle_since: None }, since: 0 });
    assert_eq!(task_state_text(&reviving, &live, Lang::Es), "Reconectando");
}

/// 13 October: quill's session ended and its restart waited at the admission gate for memory while
/// its task said «Reconectando». A lost node waits at the gate like any start: queued, with what
/// holds it, and «Reconectando» only once the gate let it back.
#[test]
fn a_running_task_whose_node_waits_at_the_gate_to_come_back_says_it_is_queued_not_reconnecting() {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    task.state = TaskState::Running;
    task.agent_id = Some("a-1".into());
    state.agents.insert("a-1".into(), Agent { id: "a-1".into(), name: "Node One".into(), phase: NodePhase::Lost, since: 0 });
    let held = pacing_live(vec![LiveQueued { pausable: true, task_id: "t-1".into(), position: 1, of: 1, hold: LiveHold::Memory { free: Some(12) } }], vec![], None);
    let (block, row) = active_row(&state, &held, Client::Mac, Lang::Es, "t-1");
    assert_eq!(row.badges[0].text, "En cola · 1.º de 1");
    assert_eq!(row.subtitle.as_deref(), Some("El Mac va justo de memoria (12 % libre)"));
    assert_eq!(block, "block:waiting");
    let detail = serde_json::to_string(&render(&state, &held, Client::Tui, Lang::Es, "task:t-1", "")).unwrap();
    assert!(detail.contains("En cola") && !detail.contains("Reconectando"), "{detail}");
    assert_eq!(task_state_text(&state, &LiveSnapshot::default(), Lang::Es), "Esperando máquina", "before it reaches the gate");

    state.agents.get_mut(&"a-1".into()).unwrap().phase = NodePhase::Starting { reviving: true, idle_since: None };
    let admitted = pacing_live(vec![], vec![], None);
    assert_eq!(task_state_text(&state, &admitted, Lang::Es), "Reconectando");
    let detail = serde_json::to_string(&render(&state, &admitted, Client::Tui, Lang::Es, "task:t-1", "")).unwrap();
    assert!(detail.contains("Reconectando") && !detail.contains("Esperando máquina"), "{detail}");
}

/// 15 October: a task reopened after its node closed said «Detenida · Su sesión se cerró sin que
/// entregara» while it waited for its turn. Reopened, it waits in the queue: every client
/// says it is queued, its place and what holds it, even though its old node was closed.
#[test]
fn a_reopened_task_waiting_in_the_queue_says_its_place_and_why_never_that_it_stopped() {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    (task.state, task.started, task.agent_id, task.final_presentation) = (TaskState::NotStarted, false, None, None);
    let mut live = pacing_live(vec![LiveQueued { pausable: true, task_id: "t-1".into(), position: 2, of: 5, hold: LiveHold::Slots { working: 4, limit: 4 } }], vec![], None);
    live.closed_tasks.push("t-1".into());
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let (block, row) = active_row(&state, &live, client, Lang::Es, "t-1");
        let shown = serde_json::to_string(&row).unwrap();
        assert!(shown.contains("En cola · 2.º de 5") && shown.contains("Hay 4 de 4 trabajando"), "{client:?}: {shown}");
        assert!(!shown.contains("Detenida") && !shown.contains("sin que entregara"), "{client:?}: {shown}");
        assert_eq!(block, "block:waiting", "{client:?}");
    }
    let held = pacing_live(vec![LiveQueued { pausable: true, task_id: "t-1".into(), position: 1, of: 1, hold: LiveHold::Memory { free: Some(9) } }], vec![], None);
    let shown = serde_json::to_string(&active_row(&state, &held, Client::Ios, Lang::Es, "t-1").1).unwrap();
    assert!(shown.contains("El Mac va justo de memoria (9 % libre)"), "{shown}");
}

fn node_in(state: &mut DomainState, phase: NodePhase) {
    state.interactions.clear();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    task.state = TaskState::Running;
    task.agent_id = Some("a-1".into());
    state.agents.insert("a-1".into(), Agent { id: "a-1".into(), name: "Node One".into(), phase, since: 0 });
}

fn every_screen(state: &DomainState, live: &LiveSnapshot) -> Vec<(String, String)> {
    let mut out = vec![];
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for key in ["tasks", "sidebar", "task:t-1"] {
            if key == "sidebar" && client != Client::Mac {
                continue;
            }
            out.push((format!("{client:?} {key}"), serde_json::to_string(&render(state, live, client, Lang::Es, key, "")).unwrap()));
        }
    }
    out
}

/// 13 October, the release of 0.2.67: the node ran the release script in the background and ended
/// its turn to wait for it. Its task says, on every screen, that it waits for that work and which
/// command it is (23 October: «Trabajando» hid a `sleep 5900` for 26 minutes); it is still in
/// progress, never stopped or idle.
#[test]
fn a_task_whose_node_waits_for_its_own_command_says_so_and_names_it_on_every_screen() {
    let mut state = fixture();
    let work = Background { work: crate::model::BackgroundKind::Shell, label: "./deploy/brew/release.sh 0.2.67".into(), since: 0, others: 0, gone: None };
    node_in(&mut state, NodePhase::WaitingBackground(work));
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode { agent_id: "a-1".into(), name: "Node One".into(), ..LiveNode::default() });
    assert_eq!(task_state_text(&state, &live, Lang::Es), "En segundo plano");
    assert_eq!(task_state_text(&state, &live, Lang::En), "In the background");
    let (block, row) = active_row(&state, &live, Client::Mac, Lang::Es, "t-1");
    assert_eq!((block.as_str(), row.subtitle.as_deref()), ("block:in-progress", Some("./deploy/brew/release.sh 0.2.67")));
    for (screen, text) in every_screen(&state, &live) {
        assert!(text.contains("En segundo plano"), "{screen}: {text}");
        if !screen.ends_with("sidebar") {
            assert!(text.contains("./deploy/brew/release.sh 0.2.67"), "{screen}: {text}");
        }
        for wrong in ["Esperando su comando", "Sin conexión", "Sin actividad", "Detenido"] {
            assert!(!text.contains(wrong), "{screen} says {wrong}: {text}");
        }
    }
}

/// 13 October: the node of «Sesión desaparecida con nodo “conectado”» sat paused for memory from
/// 12:02 to 14:53 while its task said it was working. A task whose node the machine paused says it
/// is paused and why on every screen, with the gate's word or without it.
#[test]
fn a_task_whose_node_the_machine_paused_never_says_it_works_on_any_screen() {
    let mut state = fixture();
    let pause = Pause { reason: PauseReason::Memory, since: 0, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
    node_in(&mut state, NodePhase::Paused(pause));
    let gate = LivePaused { task_id: "t-1".into(), session: Some("a-1".into()), at: "12:02".into(), reason: LivePauseReason::Memory,
        fine_minutes: None, next: false, waits_for: None, retry: None };
    for (live, fact) in [
        (pacing_live(vec![], vec![gate], None), "Por memoria desde las 12:02 · se reanuda sola"),
        (LiveSnapshot::default(), "Por memoria · se reanuda sola"),
    ] {
        assert_eq!(task_state_text(&state, &live, Lang::Es), "Pausada");
        let (block, row) = active_row(&state, &live, Client::Mac, Lang::Es, "t-1");
        assert_eq!((block.as_str(), row.subtitle.as_deref()), ("block:waiting", Some(fact)));
        for (screen, text) in every_screen(&state, &live) {
            assert!(!text.contains("Trabajando"), "{screen} says the task works: {text}");
        }
        let sidebar = serde_json::to_string(&render(&state, &live, Client::Mac, Lang::Es, "sidebar", "")).unwrap();
        assert!(sidebar.contains("Pausada"), "{sidebar}");
    }
}

/// What a node's hooks deduced is never a state: a task whose old node reads stalled or failed
/// says it works, in every client and language.
#[test]
fn no_view_says_a_node_stalled_or_failed_whatever_its_hooks_deduced() {
    let mut state = fixture();
    node_in(&mut state, NodePhase::Working);
    for key in ["stalled", "failed"] {
        let mut live = LiveSnapshot::default();
        live.nodes.push(LiveNode { agent_id: "a-1".into(), name: "Node One".into(), state_key: Some(key.into()), ..LiveNode::default() });
        assert_eq!(task_state_text(&state, &live, Lang::Es), "Trabajando");
        let screens = [Client::Tui, Client::Ios, Client::Mac].into_iter().flat_map(|client| ["task:t-1", "tasks"].map(move |screen| (client, screen)))
            .chain([(Client::Mac, "sidebar")]);
        for (client, lang, screen) in screens.flat_map(|(client, screen)| [Lang::Es, Lang::En].map(move |lang| (client, lang, screen))) {
            let shown = serde_json::to_string(&render(&state, &live, client, lang, screen, "")).unwrap();
            for word in ["Parado", "Stalled", "Falló", "Failed"] {
                assert!(!shown.contains(word), "{key} {client:?} {lang:?} {screen}: {shown}");
            }
        }
    }
}

#[test]
fn the_ended_tasks_of_a_request_still_under_way_fold_away_from_the_active_list() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Weekly improvement".into());
    for (id, task_state, archived) in [("t-2", TaskState::Running, false), ("t-3", TaskState::Done, false),
        ("t-4", TaskState::Cancelled, true)] {
        let mut task = state.tasks[&"t-1".into()].clone();
        task.id = id.into();
        task.title = format!("Task {id}");
        task.state = task_state;
        task.archived = archived;
        task.dependencies = Default::default();
        state.tasks.insert(task.id.clone(), task);
    }
    state.tasks.get_mut(&"t-2".into()).unwrap().dependencies = ["t-3".into()].into();
    fn ids(rows: &[Row]) -> Vec<String> {
        rows.iter().flat_map(|row| std::iter::once(row.id.clone()).chain(ids(&row.children))).collect()
    }
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let view = render(&state, &LiveSnapshot::default(), client, Lang::Es, "tasks", "");
        let page = |id: &str| render(&state, &LiveSnapshot::default(), client, Lang::Es, &format!("tasks?section={id}&after=0"), "");
        let rows_of = |view: &View, id: &str| view.sections.iter().find(|section| section.id == id).unwrap().items.iter()
            .filter_map(|item| if let Item::Row(row) = item { Some(row.clone()) } else { None }).collect::<Vec<_>>();
        let section = |id: &str| view.sections.iter().find(|section| section.id == id).unwrap();
        let rows = |id: &str| rows_of(&page(id), id);
        let active = ids(&rows_of(&view, "active"));
        assert!(active.contains(&"t-1".into()) && active.contains(&"t-2".into()), "{client:?}: {active:?}");
        assert!(!active.contains(&"t-3".into()) && !active.contains(&"t-4".into()), "{client:?}: {active:?}");
        assert!(section("finished").items.is_empty(), "{client:?}: the main view carries no completed work");
        assert_eq!(section("finished").more.as_ref().map(|more| (more.key.as_str(), more.remaining)),
            Some(("tasks?section=finished&after=0", 1)), "{client:?}");
        let finished = rows("finished");
        assert_eq!(finished.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["r-1:ended"], "{client:?}");
        assert_eq!(finished[0].title, "Weekly improvement");
        assert!(ids(&finished).contains(&"t-3".into()) || finished[0].link == Some(Link::View { key: "task:t-3".into() }),
            "{client:?}");
        assert!(section("finished").collapsed);
        assert_eq!(section("finished").count, Some(1));
        assert!(!ids(&finished).contains(&"t-4".into()), "{client:?}: a cleared task is in no section");
        if client != Client::Tui {
            assert!(every_text(&finished).iter().all(|text| text != "El resto sigue en marcha"), "{client:?}");
        }
    }
}

#[test]
fn a_tui_task_list_shows_a_request_with_one_task_as_that_task_and_nests_only_several() {
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let mut live = LiveSnapshot::default();
    live.request_times.insert("r-1".into(), "2026-10-03T19:49:18.945+02:00".into());
    let single = active_row(&state, &live, Client::Tui, Lang::Es, "t-1").1;
    assert!(single.children.is_empty());
    assert_eq!(single.title, "Build a feature");
    assert_eq!(single.badges.first().map(|badge| badge.text.as_str()), Some("Trabajando"));
    assert!(!matches!(single.trailing, Some(Trailing::Count { .. })));
    assert!(active_rows(&state, &live, Client::Tui, Lang::Es).iter().all(|(_, row)| row.id != "r-1" && row.id != "group:r-1"),
        "a request with one task is that task, with no header");
    let mut second = state.tasks[&"t-1".into()].clone();
    second.id = "t-2".into();
    second.title = "Write the docs".into();
    second.state = TaskState::Running;
    let mut third = second.clone();
    third.id = "t-3".into();
    third.state = TaskState::Done;
    state.tasks.insert(second.id.clone(), second);
    state.tasks.insert(third.id.clone(), third);
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Docs and feature".into());
    let several = active_row(&state, &live, Client::Tui, Lang::Es, "group:r-1").1;
    assert_eq!(several.children.iter().map(|task| task.id.as_str()).collect::<Vec<_>>(), ["t-1", "t-2"]);
    assert_eq!(several.badges.first().map(|badge| badge.text.as_str()), Some("2 tareas"));
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
        (TaskState::Done, false, false, &["Reopen"]),
        (TaskState::Cancelled, false, false, &["Reopen"]),
        (TaskState::Done, true, false, &["Reopen"]),
        (TaskState::Done, false, true, &[]),
        (TaskState::Cancelled, true, true, &[]),
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
                .filter(|label| !["Message", "Priority"].contains(label)).collect();
            assert_eq!(labels, expected, "{case}");
            assert_eq!(actions.iter().any(|action| action.key.is_some()), client == Client::Tui && !actions.is_empty(), "{case}: keys are a terminal thing");
        }
    }
}

#[test]
fn rows_count_cards_from_the_render_index_without_reading_the_inbox_again() {
    let mut state = with_cards(&fixture());
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let live = LiveSnapshot::default();
    let counted = |index: &crate::domain::ReadIndex, key: &str| {
        let view = build(key, RenderContext {
            domain: &state, index, live: &live, client: Client::Ios, lang: Lang::En,
            transport: Transport::Loopback, cursor: "cursor-1", rendered_at: "",
        }).unwrap();
        fn count(row: &Row) -> u32 {
            let own = match row.trailing { Some(Trailing::Count { value }) => value, _ => 0 };
            own + row.children.iter().map(count).sum::<u32>()
        }
        view.sections.iter().flat_map(|section| &section.items)
            .map(|item| if let Item::Row(row) = item { count(row) } else { 0 }).sum::<u32>()
    };
    assert!(counted(&crate::domain::ReadIndex::build(&state), "tasks") > 0, "the fixture has a card waiting on its task");
    let nothing_waiting = crate::domain::ReadIndex::build(&DomainState::new("test-epoch"));
    assert_eq!(counted(&nothing_waiting, "tasks"), 0);
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
fn tasks_hide_zero_scheduled_count_and_clear_completed_until_completed_work_exists() {
    let mut state = fixture();
    state.revision = 5;
    let clear = |state: &DomainState, client: Client| render(state, &LiveSnapshot::default(), client, Lang::Es, "tasks", "")
        .sections.into_iter().find(|section| section.id == "finished").unwrap()
        .actions.into_iter().find(|action| action.id.starts_with("clear_completed:view@"));
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "tasks", "");
    let scheduled = view.sections.iter().find(|section| section.id == "scheduled").unwrap();
    assert_eq!(scheduled.count, None);
    assert!(clear(&state, Client::Tui).is_none(), "nothing completed, nothing to clear");

    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Cancelled;
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let view = render(&state, &LiveSnapshot::default(), client, Lang::Es, "tasks", "");
        let finished = view.sections.iter().find(|section| section.id == "finished").unwrap();
        assert_eq!(finished.title.as_deref(), Some("Completadas"), "{client:?}");
        assert_eq!(finished.count, Some(1), "{client:?}: a cancelled task is completed work");
        let action = clear(&state, client).unwrap_or_else(|| panic!("{client:?}: {:?}", finished.actions));
        assert_eq!(action.label, "Limpiar completadas");
        assert!(action.resolves_card, "{client:?}: the section empties the moment it is sent");
        assert_eq!(action.key.as_deref(), (client == Client::Tui).then_some("A"));
        assert!(!view.sections.iter().any(|section| section.id == "archived" || section.id == "actions"), "{client:?}");
    }
    assert_eq!(render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "tasks", "")
        .sections.iter().find(|section| section.id == "finished").unwrap().title.as_deref(), Some("Completed"));

    state.tasks.get_mut(&"t-1".into()).unwrap().archived = true;
    let mut request = state.requests[&"r-1".into()].clone();
    (request.id, request.closed) = ("r-answered".into(), true);
    state.requests.insert(request.id.clone(), request);
    let finished = |state: &DomainState| render(state, &LiveSnapshot::default(), Client::Ios, Lang::En, "tasks?section=finished&after=0", "")
        .sections.into_iter().find(|section| section.id == "finished").unwrap();
    let rows: Vec<String> = finished(&state).items.iter().filter_map(|item| match item { Item::Row(row) => Some(row.id.clone()), _ => None }).collect();
    assert!(rows.iter().all(|id| id.contains("r-answered")) && !rows.is_empty(), "the cleared task is gone, the request answered without tasks stays: {rows:?}");
    assert!(clear(&state, Client::Ios).is_some(), "a request answered without tasks is still there to clear");
    state.requests.get_mut(&"r-answered".into()).unwrap().cleared = true;
    assert_eq!(finished(&state).count, Some(0), "a cleared request is in no section");
    assert!(clear(&state, Client::Ios).is_none());
}

#[test]
fn the_archived_page_of_tasks_no_longer_exists() {
    let state = fixture();
    let live = LiveSnapshot::default();
    let index = crate::domain::ReadIndex::build(&state);
    let context = |key| build(key, RenderContext { domain: &state, index: &index, live: &live, client: Client::Tui, lang: Lang::En,
        transport: Transport::Loopback, cursor: "cursor-1", rendered_at: "2026-09-30T10:00:00Z" });
    assert!(context("tasks?section=finished&after=0").is_ok());
    assert_eq!(context("tasks?section=archived&after=0").err(), Some(BuildError::InvalidKey));
}

fn tasks_items(state: &DomainState, client: Client, lang: Lang) -> Vec<Row> {
    active_blocks(state, &LiveSnapshot::default(), client, lang)
}

/// The active section's blocks: For you, In progress, Waiting.
fn active_blocks(state: &DomainState, live: &LiveSnapshot, client: Client, lang: Lang) -> Vec<Row> {
    let view = render(state, live, client, lang, "tasks", "");
    let active = view.sections.into_iter().find(|section| section.id == "active").unwrap();
    active.items.into_iter().filter_map(|item| if let Item::Row(row) = item { Some(row) } else { None }).collect()
}

/// Every row of the active list, depth first, with the block each sits in.
fn active_rows(state: &DomainState, live: &LiveSnapshot, client: Client, lang: Lang) -> Vec<(String, Row)> {
    fn walk(block: &str, rows: &[Row], out: &mut Vec<(String, Row)>) {
        for row in rows {
            out.push((block.to_owned(), row.clone()));
            walk(block, &row.children, out);
        }
    }
    let mut out = Vec::new();
    for block in active_blocks(state, live, client, lang) {
        walk(&block.id, &block.children, &mut out);
    }
    out
}

fn active_row(state: &DomainState, live: &LiveSnapshot, client: Client, lang: Lang, id: &str) -> (String, Row) {
    active_rows(state, live, client, lang).into_iter().find(|(_, row)| row.id == id)
        .unwrap_or_else(|| panic!("{id} in the active list"))
}

fn two_tasks_with_body(body: &str) -> DomainState {
    let mut state = fixture();
    let mut second = state.tasks[&"t-1".into()].clone();
    second.id = "t-2".into();
    second.title = "Second thing".into();
    state.tasks.insert("t-2".into(), second);
    let mut asked = state.interactions[&"i-1".into()].clone();
    asked.id = "i-1-second".into();
    asked.scope = Scope::Task { task_id: "t-2".into() };
    state.interactions.insert(asked.id.clone(), asked);
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
            let blocks = tasks_items(&state, client, lang);
            assert_eq!(blocks.len(), 1);
            let group = &blocks[0].children[0];
            assert_eq!((group.id.as_str(), group.title.as_str(), group.depth), ("group:r-1", "Push without a card", 1));
            assert_eq!(group.children.iter().filter(|row| row.id.starts_with("t-")).map(|row| (row.id.as_str(), row.depth)).collect::<Vec<_>>(),
                [("t-1", 2), ("t-2", 2)], "{client:?}");
        }
    }
}

#[test]
fn an_untitled_request_with_several_tasks_has_no_header_and_its_tasks_stand_loose() {
    let body = "Can Alondra detect conflicts before sending the PR push? I got the push and then there was no card";
    let state = two_tasks_with_body(body);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let rows = tasks_items(&state, client, Lang::En);
        let mut loose: Vec<_> = rows.iter().flat_map(|block| &block.children).map(|row| (row.id.as_str(), row.depth)).collect();
        loose.sort();
        assert_eq!(loose, [("t-1", 1), ("t-2", 1)], "{client:?}");
        assert!(every_text(&rows).iter().all(|text| !text.contains("Can Alondra")), "the prompt is never a title");
    }
}

#[test]
fn an_untitled_request_with_one_task_is_that_task_and_one_with_none_is_its_text() {
    let mut state = fixture();
    state.requests.insert("r-2".into(), Request::new("r-2".into(), "Export the logs from settings. Today it needs a cable.".into()));
    for lang in [Lang::En, Lang::Es] {
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            let title = |id: &str| active_row(&state, &LiveSnapshot::default(), client, lang, id).1.title;
            assert_eq!(title("t-1"), "Build a feature");
            assert_eq!(title("r-2"), "Export the logs from settings");
        }
    }
}

#[test]
fn a_request_answered_without_tasks_leaves_the_list_and_one_closed_without_an_answer_stays() {
    let mut state = fixture();
    state.tasks.clear();
    let request = state.requests.get_mut(&"r-1".into()).unwrap();
    request.closed = true;
    request.answer = Some("Yes, since Monday".into());
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let view = render(&state, &LiveSnapshot::default(), client, Lang::En, "tasks?section=finished&after=0", "");
        assert!(rows_with_parents(&view).iter().all(|(_, row)| row.id != "r-1"), "{client:?}: its answer is on its card in the inbox");
    }
    state.requests.get_mut(&"r-1".into()).unwrap().answer = None;
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "tasks?section=finished&after=0", "");
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
    state.interactions.clear();
    for task in state.tasks.values_mut() {
        (task.state, task.agent_id, task.final_presentation) = (TaskState::Running, Some("a-1".into()), None);
    }
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Three tasks".into());
    state.requests.insert("r-2".into(), Request::new("r-2".into(), "Still splitting".into()));

    let link = |id: &str| active_row(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, id).1.link;
    assert_eq!(link("group:r-1"), Some(Link::View { key: "task:t-2".into() }));
    assert_eq!(link("r-2"), None);
}

#[test]
fn a_working_task_says_only_working_or_its_own_step_and_no_time() {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    (task.agent_id, task.state, task.final_presentation) = (Some("a-1".into()), TaskState::Running, None);
    let mut live = LiveSnapshot::default();
    live.task_since.insert("t-1".into(), "2026-09-30T06:58:00Z".into());
    live.nodes.push(LiveNode {
        agent_id: "a-1".into(), name: "Node One".into(), working: true,
        tool: Some("Bash: f=/private/tmp/out.txt; until grep -q done $f; do sleep 5; done".into()),
        state_since: Some("2026-09-30T06:58:00Z".into()),
        ..LiveNode::default()
    });
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let (block, row) = active_row(&state, &live, client, Lang::Es, "t-1");
        assert_eq!((block.as_str(), row.badges[0].text.as_str()), ("block:in-progress", "Trabajando"), "{client:?}");
        assert!(row.children.is_empty(), "{client:?}: no line inferred from the command it runs");
        assert!(!matches!(row.trailing, Some(Trailing::Time { .. })), "{client:?}: a task at work shows no time");
        assert!(row.subtitle.as_deref().is_none_or(|subtitle| subtitle == "Trabajando"), "{client:?}: {:?}", row.subtitle);
        assert!(every_text(&[row]).iter().all(|text| !text.contains("grep") && !text.contains("Bash")), "{client:?}");
    }
    live.nodes[0].steps = ["Plan", "Build", "Generate iOS screenshots", "Ship"].iter().enumerate().map(|(index, subject)| LiveStep {
        subject: (*subject).into(), active_form: None,
        state: match index { 0 | 1 => StepState::Completed, 2 => StepState::InProgress, _ => StepState::Pending },
    }).collect();
    let ios = active_row(&state, &live, Client::Ios, Lang::Es, "t-1").1.children;
    assert_eq!(ios.iter().map(|line| (line.id.as_str(), line.title.as_str(), line.trailing.is_none())).collect::<Vec<_>>(),
        [("activity", "Paso 3 de 4 · Generate iOS screenshots", true)], "its own plan is a fact; still no time");
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
fn task_message_keeps_task_context_and_offers_no_terminal() {
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
        background_badge: None, working: true,
        waiting: false, state_since: None, last_message: None, last_message_at: None, ..Default::default()
    });
    let attaches = |view: &View| view.sections.iter().flat_map(|section| &section.items).any(|item| match item {
        Item::Row(row) => matches!(&row.link, Some(Link::Local { local: LocalLink::Attach { .. } })),
        Item::Card(card) => card.more.iter().chain(&card.actions).any(|action| matches!(&action.link, Some(Link::Local { local: LocalLink::Attach { .. } }))),
        Item::Actions(bar) => bar.actions.iter().any(|action| matches!(&action.link, Some(Link::Local { local: LocalLink::Attach { .. } }))),
        _ => false,
    });
    let task = render(&state, &live, Client::Tui, Lang::En,
        "task:t-1", "2026-09-30T10:00:00Z");
    assert!(!attaches(&task), "a task's view never opens its node's terminal");
    let phone_task = render(&state, &live, Client::Ios, Lang::En,
        "task:t-1", "2026-09-30T10:00:00Z");
    assert!(!phone_task.sections.iter().flat_map(|section| &section.items).any(|item| {
        matches!(item, Item::Row(row) if matches!(&row.link, Some(Link::Local { .. })))
    }));
    let card = render(&state, &live, Client::Tui, Lang::En,
        "card:i-1", "2026-09-30T10:00:00Z");
    assert!(!attaches(&card), "a card never opens its node's terminal");
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
        "search?q=Build+a",
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

/// The orchestrator's Decide card David answered in words on 13 October: its reply field is the
/// answer to it, so every client takes it out on send.
fn orchestrator_decide_fixture() -> DomainState {
    use crate::domain::{CardKind, ContentKind as Content, QuestionOption};
    let mut state = golden_fixture();
    let option = |id: &str, label: &str, description: &str| QuestionOption {
        id: id.into(), label: label.into(), description: Some(description.into()),
    };
    state.interactions.insert("memory-pause-livelock-ask".into(), Interaction {
        attachments: vec![], id: "memory-pause-livelock-ask".into(), scope: Scope::Orchestrator { request_id: None },
        author: Author::Orchestrator { generation: 1 }, recipient: Recipient::User,
        body: "Who fixes the memory pauses that cut the same build in a loop?".into(), blocking: false, subject: None,
        payload: InteractionPayload::Question {
            options: vec![
                option("mine", "I'll fold it into my change", "No task is created."),
                option("new_task", "Create a task", "A node measures the loop and changes the pause rule."),
            ],
            answer: None,
        },
        revision: 1, order: 20, withdrawn: false,
        card: spec(CardKind::Decide, "Decide who fixes the memory pauses that cut the same build in a loop",
            "A node found a loop in the memory rule that leaves a task stuck.")
            .content(Content::Diagnosis, "Resumed, the node relaunches the build; memory turns Tight in 50 s and it is paused again."),
        clarification: None,
    });
    state
}

#[test]
fn an_orchestrator_decide_card_matches_its_golden() {
    let state = orchestrator_decide_fixture();
    for (client, filename, expected) in [
        (Client::Ios, "card-orchestrator-decide.json", include_str!("../../../../fixtures/views/cards/card-orchestrator-decide.json")),
        (Client::Mac, "card-orchestrator-decide-mac.json", include_str!("../../../../fixtures/views/cards/card-orchestrator-decide-mac.json")),
        (Client::Tui, "card-orchestrator-decide-tui.json", include_str!("../../../../fixtures/views/cards/card-orchestrator-decide-tui.json")),
    ] {
        let actual = render(&state, &golden_live(), client, Lang::En, "card:memory-pause-livelock-ask", "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

/// The beta candidate delivered against the plan its node had approved: every point with what
/// proves it, the commit linked to the PR's repository.
fn checklist_delivery_fixture() -> DomainState {
    use crate::domain::{Checklist, ChecklistItem, ChecklistMark, Evidence, EvidenceKind, InteractionId};
    let mut state = golden_fixture();
    let delivery = InteractionId::from("hf-weekly-2026-09-30");
    let task_id = state.interactions[&delivery].scope.clone();
    let Scope::Task { task_id } = task_id else { unreachable!() };
    let point = |id: &str, text: &str| ChecklistItem { id: id.into(), text: text.into() };
    let plan = InteractionId::from("hold-plan-1");
    let mut held = state.interactions[&delivery].clone();
    held.id = plan.clone();
    held.recipient = crate::domain::Recipient::Orchestrator;
    held.payload = crate::domain::InteractionPayload::Question { options: vec![], answer: None };
    state.interactions.insert(plan.clone(), held);
    state.holds.insert(plan, crate::domain::Hold::Plan);
    state.checklists.insert(task_id, Checklist { plan: "hold-plan-1".into(), items: vec![
        point("1", "Restore the native review layouts"),
        point("2", "Verify the ARM simulator build"),
        point("3", "Snapshot the review screens in light and dark"),
    ] });
    let evidence = |kind, value: &str, detail: Option<&str>| Evidence { kind, value: value.into(), detail: detail.map(Into::into) };
    state.checklist_marks.insert(delivery, vec![
        ChecklistMark { item: "1".into(), evidence: vec![evidence(EvidenceKind::Commit, "3f2a1c9e5b7d4a1f8c6e2b9d0a7f5c3e1b8d6a4f", None)] },
        ChecklistMark { item: "2".into(), evidence: vec![evidence(EvidenceKind::Command, "make build-sim ARCH=arm64", Some("Build succeeded in 2 min 14 s"))] },
        ChecklistMark { item: "3".into(), evidence: vec![
            evidence(EvidenceKind::Test, "make snapshots-verify", Some("42 snapshots match")),
            evidence(EvidenceKind::Link, "https://github.com/example/repo/pull/12#snapshots", None),
        ] },
    ]);
    state.validate().expect("the fixture is a state the domain could reach");
    state
}

#[test]
fn a_final_delivery_shows_each_point_of_the_approved_plan_with_its_evidence() {
    let state = checklist_delivery_fixture();
    let live = golden_live();
    for (client, lang, filename, expected) in [
        (Client::Ios, Lang::En, "card-delivery-checklist.json", include_str!("../../../../fixtures/views/cards/card-delivery-checklist.json")),
        (Client::Ios, Lang::Es, "card-delivery-checklist-es.json", include_str!("../../../../fixtures/views/cards/card-delivery-checklist-es.json")),
        (Client::Mac, Lang::En, "card-delivery-checklist-mac.json", include_str!("../../../../fixtures/views/cards/card-delivery-checklist-mac.json")),
        (Client::Mac, Lang::Es, "card-delivery-checklist-es-mac.json", include_str!("../../../../fixtures/views/cards/card-delivery-checklist-es-mac.json")),
    ] {
        let actual = render(&state, &live, client, lang, "card:hf-weekly-2026-09-30", "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
    let view = render(&state, &live, Client::Tui, Lang::En, "card:hf-weekly-2026-09-30", "");
    let plan = view.sections.iter().find(|section| section.id == "plan").unwrap();
    assert_eq!(plan.title.as_deref(), Some("Approved plan · 3 of 3 done"));
    let Item::Row(first) = &plan.items[0] else { panic!("a point is a row") };
    assert_eq!(first.title, "Restore the native review layouts");
    assert_eq!(first.children[0].title, "Commit · 3f2a1c9");
    assert!(matches!(&first.children[0].link, Some(Link::Url { url }) if url.ends_with("/commit/3f2a1c9e5b7d4a1f8c6e2b9d0a7f5c3e1b8d6a4f")));
    assert_eq!(view.sections.iter().position(|section| section.id == "plan"), Some(1), "the plan comes right after the card");
    let without = render(&golden_fixture(), &live, Client::Tui, Lang::En, "card:hf-weekly-2026-09-30", "");
    assert!(without.sections.iter().all(|section| section.id != "plan"), "a delivery without an approved plan shows no plan");
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
fn a_settled_card_matches_its_goldens_in_every_apple_client() {
    let state = settled_fixture();
    let live = golden_live();
    let expected = [
        include_str!("../../../../fixtures/views/cards/card-settled-ios.json"),
        include_str!("../../../../fixtures/views/cards/card-settled-ios-es.json"),
        include_str!("../../../../fixtures/views/cards/card-settled-mac.json"),
        include_str!("../../../../fixtures/views/cards/card-settled-mac-es.json"),
    ];
    for ((filename, client, lang), expected) in SETTLED_GOLDENS.into_iter().zip(expected) {
        let actual = render(&state, &live, client, lang, &format!("card:{SETTLED_CARD}"), "2026-09-30T10:00:00Z");
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
    ] {
        let actual = render(&state, &live, Client::Mac, Lang::En, key, "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
    let spanish = render(&state, &live, Client::Mac, Lang::Es, "sidebar", "2026-09-30T10:00:00Z");
    assert_golden("sidebar-mac-es.json", &spanish, include_str!("../../../../fixtures/views/cards/sidebar-mac-es.json"));
    let working = render(&golden_in_progress(), &live, Client::Mac, Lang::En, "sidebar", "2026-09-30T10:00:00Z");
    assert_golden("sidebar-in-progress-mac.json", &working, include_str!("../../../../fixtures/views/cards/sidebar-in-progress-mac.json"));
}

/// The golden fixture with the node of its task stopped after writing to the user in its
/// terminal: its last words wait for the user in a card.
fn unheard_fixture() -> (DomainState, LiveSnapshot, String) {
    let mut state = golden_fixture();
    let task = crate::domain::TaskId::from("t-651c38484cd2f34e74d5107ba05f7f15");
    let agent = crate::domain::AgentId::from("d887a34b-249e-4487-a89b-57de38c26372");
    state.tasks.get_mut(&task).unwrap().agent_id = Some(agent.clone());
    let said = "Han llegado las dos tandas, pero no sirven: el fallo estaba en mi montaje, no en tu red. Repite la tanda en móvil.";
    let mut card = state.interactions.values().find(|i| matches!(i.payload, InteractionPayload::Question { answer: None, .. })
        && i.recipient == Recipient::User && !i.withdrawn).expect("the fixture asks the user something").clone();
    let id = format!("{}{task}-90", crate::domain::UNHEARD_PREFIX);
    (card.id, card.scope, card.author, card.body) = (id.as_str().into(), Scope::Task { task_id: task.clone() }, Author::Agent { agent_id: agent.clone() }, said.into());
    card.payload = InteractionPayload::Question {
        options: vec![QuestionOption { id: crate::domain::CARRY_ON.into(), label: crate::domain::CARRY_ON_LABEL.into(), description: None }],
        answer: None,
    };
    card.card = crate::domain::CardSpec::new(crate::domain::CardKind::Decide, "stopped and wrote to you").why(said);
    (card.subject, card.blocking) = (None, false);
    state.interactions.retain(|_, i| !(i.recipient == Recipient::User && matches!(i.scope, Scope::Task { ref task_id } if *task_id == task)));
    state.interactions.insert(card.id.clone(), card);
    state.agents.get_mut(&agent).unwrap().phase = NodePhase::WaitingUser { in_turn: false };
    (state, golden_live(), id)
}

const UNHEARD_GOLDENS: [(&str, Client, &str); 2] = [
    ("card-unheard-ios-es.json", Client::Ios, include_str!("../../../../fixtures/views/cards/card-unheard-ios-es.json")),
    ("card-unheard-es-mac.json", Client::Mac, include_str!("../../../../fixtures/views/cards/card-unheard-es-mac.json")),
];

/// The card with a node's last words, in Spanish, as the iPhone and the Mac draw it.
#[test]
fn unheard_card_views_match_goldens() {
    let (state, live, id) = unheard_fixture();
    for (filename, client, expected) in UNHEARD_GOLDENS {
        let actual = render(&state, &live, client, Lang::Es, &format!("card:{id}"), "2026-09-30T10:00:00Z");
        let shown = serde_json::to_string(&actual).unwrap();
        assert!(shown.contains("se ha parado y te escribe") && shown.contains("Que siga"), "{filename}: {shown}");
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
        ("new-request-mac.json", "new_request", Client::Mac, Lang::En),
        ("new-request-ios-es.json", "new_request", Client::Ios, Lang::Es),
        ("new-request-mac-es.json", "new_request", Client::Mac, Lang::Es),
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
    ];
    let mut with_pending = state.clone();
    let pending = Request::new("r-4b7e2c1d9a0f4e3b8c6d5a4f3e2b1c0d".into(), "Import lists from Notes".into());
    with_pending.requests.insert(pending.id.clone(), pending);
    let mut with_pending_es = state.clone();
    let pendiente = Request::new("r-4b7e2c1d9a0f4e3b8c6d5a4f3e2b1c0d".into(), "Importar las listas de Notas".into());
    with_pending_es.requests.insert(pendiente.id.clone(), pendiente);
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
    let (working_state, working_live) = task_working_fixture();
    for (filename, client, lang) in TASK_WORKING_GOLDENS {
        let view = render(&working_state, &working_live, client, lang, "task:t-1", "2026-09-30T10:00:00Z");
        std::fs::write(format!("{dir}/{filename}"), format!("{}\n", serde_json::to_string_pretty(&view).unwrap())).unwrap();
    }
    for (filename, key, client, lang) in cases {
        let state = match (key, lang) {
            ("new_request", Lang::Es) => &with_pending_es,
            ("new_request", _) => &with_pending,
            _ => &state,
        };
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
fn a_task_that_waits_for_another_folds_under_it_from_one() {
    let mut state = fixture();
    let mut child = state.tasks.get(&"t-1".into()).unwrap().clone();
    child.id = "t-2".into();
    child.title = "Follow-up".into();
    child.state = TaskState::WaitingDependency;
    child.dependencies.insert("t-1".into());
    state.tasks.insert(child.id.clone(), child);
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Two steps".into());
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let rows = rows_of(&state, client, Lang::En);
        let head = find(&rows, "t-1");
        let fold = find(&head.children, "chain:t-1");
        assert_eq!((fold.title.as_str(), fold.folded, fold.depth, fold.link.is_none()),
            ("+1 waiting for this", true, 2, true), "{client:?}: one waiting task already folds");
        let waiting = find(&fold.children, "t-2");
        assert_eq!((waiting.subtitle.as_deref(), waiting.depth), (Some("waits for Build a feature"), 3), "{client:?}");
        assert_eq!(occurrences(&rows, "group:r-1"), 0, "{client:?}: one task left loose needs no header");
    }
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

fn rows_of(state: &DomainState, client: Client, lang: Lang) -> Vec<Row> {
    active_rows(state, &LiveSnapshot::default(), client, lang).into_iter().map(|(_, row)| row).collect()
}

/// How many times a row is in a flattened list.
fn occurrences(rows: &[Row], id: &str) -> usize {
    rows.iter().filter(|row| row.id == id).count()
}

#[test]
fn a_task_that_waits_for_another_requests_task_is_in_that_tasks_chain_and_nowhere_else() {
    let state = across_requests();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let rows = rows_of(&state, client, Lang::En);
        let fold = find(&find(&rows, "t-1").children, "chain:t-1");
        let waiting = find(&fold.children, "t-4");
        assert_eq!((waiting.title.as_str(), waiting.subtitle.as_deref()), ("Decrement the badge", Some("waits for Build a feature")), "{client:?}");
        assert_eq!(occurrences(&rows, "t-4"), 1, "{client:?}: in the chain only");
        assert!(rows.iter().all(|row| !row.id.starts_with("waits:") && row.id != "from"), "{client:?}: no «waits for» or «from» rows");
        assert_eq!(occurrences(&rows, "t-3"), 1, "{client:?}");
    }
}

#[test]
fn the_spanish_labels_of_a_chain() {
    let rows = rows_of(&across_requests(), Client::Tui, Lang::Es);
    let fold = find(&find(&rows, "t-1").children, "chain:t-1");
    assert_eq!(fold.title, "+1 esperando a esta");
    assert_eq!(find(&fold.children, "t-4").subtitle.as_deref(), Some("espera a Build a feature"));
}

#[test]
fn a_task_whose_dependency_ended_stands_on_its_own() {
    let mut state = across_requests();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    state.requests.get_mut(&"r-1".into()).unwrap().closed = true;
    let rows = rows_of(&state, Client::Ios, Lang::En);
    assert!(rows.iter().all(|row| !row.id.starts_with("chain:")), "nothing open to wait for");
    assert_eq!(occurrences(&rows, "t-4"), 1);
    assert!(!rows.iter().any(|row| row.id == "t-1"), "the finished task is not in the active list");
}

#[test]
fn a_task_that_waits_for_two_names_both_and_is_shown_once() {
    let mut state = across_requests();
    state.tasks.get_mut(&"t-4".into()).unwrap().dependencies.insert("t-3".into());
    let rows = rows_of(&state, Client::Ios, Lang::En);
    assert_eq!(occurrences(&rows, "t-4"), 1);
    let fold = find(&find(&rows, "t-1").children, "chain:t-1");
    assert_eq!(find(&fold.children, "t-4").subtitle.as_deref(), Some("waits for Build a feature and Count the pushes"));
}

#[test]
fn tasks_that_wait_on_each_other_across_requests_are_each_shown_once() {
    let mut state = across_requests();
    state.tasks.get_mut(&"t-1".into()).unwrap().dependencies.insert("t-4".into());
    let rows = rows_of(&state, Client::Ios, Lang::En);
    assert_eq!((occurrences(&rows, "t-1"), occurrences(&rows, "t-4")), (1, 1));
}

fn guided_install() -> DomainState {
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().title = "Install PR 1: setup".into();
    for (id, dependencies) in [
        ("t-2b", &["t-1"][..]), ("t-3", &["t-1"][..]), ("t-4", &["t-3"][..]), ("t-5", &["t-4"][..]),
        ("t-6", &["t-2b"][..]), ("t-7", &["t-6"][..]), ("t-8", &["t-4", "t-5", "t-7"][..]), ("t-9", &["t-8"][..]),
    ] {
        let mut task = state.tasks[&"t-1".into()].clone();
        task.id = id.into();
        task.title = format!("Install PR {}: step", &id[2..]);
        task.state = TaskState::WaitingDependency;
        task.dependencies = dependencies.iter().map(|id| (*id).into()).collect();
        state.tasks.insert(task.id.clone(), task);
    }
    state
}

#[test]
fn a_chain_folds_flat_under_its_first_open_task_each_after_what_it_waits_for() {
    let state = guided_install();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let rows = rows_of(&state, client, Lang::En);
        let fold = find(&find(&rows, "t-1").children, "chain:t-1");
        assert_eq!(fold.title, "+8 waiting for this", "{client:?}");
        let ids: Vec<_> = fold.children.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["t-2b", "t-6", "t-7", "t-3", "t-4", "t-5", "t-8", "t-9"], "{client:?}: each after what it waits for");
        assert!(fold.children.iter().all(|row| row.depth == 3 && row.children.is_empty()), "{client:?}: one level, no staircase");
        for id in ids { assert_eq!(occurrences(&rows, id), 1, "{client:?}: {id} once"); }
    }
    let spanish = rows_of(&state, Client::Ios, Lang::Es);
    let fold = find(&find(&spanish, "t-1").children, "chain:t-1");
    assert_eq!(fold.title, "+8 esperando a esta");
    assert_eq!(find(&fold.children, "t-8").subtitle.as_deref(), Some("espera a Install PR 4, 5 y 7"),
        "what the names share is said once");
    assert_eq!(find(&fold.children, "t-6").subtitle.as_deref(), Some("espera a Install PR 2b"));
}

fn orchestrator_waiting(can_retry: bool) -> LiveSnapshot {
    LiveSnapshot { orchestrator: Some(live::LiveOrchestrator {
        working: false, summary: Some("Claude exited without a successful terminal result".into()), failure: None,
        queue_depth: 1, active_run_id: Some("run-1".into()), can_retry, can_cancel: !can_retry, waits_for_sign_in: false,
    }), ..Default::default() }
}

#[test]
fn a_machine_card_offers_its_fix_and_no_reply_to_the_orchestrator_on_any_client() {
    use crate::domain::CardKind;
    use crate::machine_incident::{MACHINE_FIXED_LABEL, MACHINE_FIXED_OPTION, MachineCard, MachineCause};
    let mut state = fixture();
    let disk = MachineCard { cause: MachineCause::Disk, disk_free: 3_000_000_000, nodes: vec![], reason: None, minutes: None, disk: None, still: false, purgeable: 0 };
    let (need, why) = disk.text(Lang::En);
    state.interactions.insert("machine-1000".into(), Interaction {
        attachments: vec![], id: "machine-1000".into(), scope: Scope::Orchestrator { request_id: None },
        author: Author::System, recipient: Recipient::User, body: disk.to_body(),
        blocking: false, subject: None,
        payload: InteractionPayload::Question {
            options: vec![QuestionOption { id: MACHINE_FIXED_OPTION.into(), label: MACHINE_FIXED_LABEL.into(), description: None }],
            answer: None,
        },
        revision: 1, order: 9, withdrawn: false,
        card: spec(CardKind::Problem, &need, &why),
        clarification: None,
    });
    let live = LiveSnapshot::default();
    for (client, lang) in [(Client::Ios, Lang::Es), (Client::Mac, Lang::En), (Client::Tui, Lang::Es)] {
        let view = render(&state, &live, client, lang, "card:machine-1000", "");
        let card = card_of(&view);
        assert!(card.reply.is_none(), "{client:?}: Alondra raised it, nobody to reply to");
        assert_eq!(card.actions.iter().filter_map(|action| action.option_id.as_deref()).collect::<Vec<_>>(), [MACHINE_FIXED_OPTION]);
        let text = serde_json::to_string(card).unwrap();
        assert!(!text.contains("orchestrator") && !text.contains("orquestador"), "{client:?}: {text}");
    }
}

#[test]
fn an_orchestrator_problem_is_a_card_like_any_other_and_comes_first() {
    use crate::domain::{CardKind, ContentKind as Content};
    let mut state = fixture();
    let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
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

#[test]
fn a_batch_that_stopped_halfway_reads_in_spanish_with_what_it_did_and_how_to_choose() {
    use crate::domain::CardKind;
    use crate::orchestrator_batch::*;
    let mut state = fixture();
    let problem = BatchProblem {
        stop: BatchStop::Interrupted, calls: vec!["create_task".into()],
        subjects: vec!["Make the editor readable".into()], error: "Claude exited without a successful terminal result".into(),
    };
    let (need, why) = problem.text(Lang::En);
    let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
    let mut card = spec(CardKind::Problem, &need, &why);
    card.recommended = Some(BATCH_CONTINUE.into());
    state.interactions.insert("problem-orchestrator-run-1".into(), Interaction {
        attachments: vec![], id: "problem-orchestrator-run-1".into(), scope: Scope::Orchestrator { request_id: None },
        author: Author::System, recipient: Recipient::User, body: problem.to_body(),
        blocking: false, subject: None,
        payload: InteractionPayload::Question {
            options: vec![option(BATCH_CONTINUE, BATCH_CONTINUE_LABEL), option(BATCH_RETRY, BATCH_RETRY_LABEL)],
            answer: None,
        },
        revision: 1, order: 9, withdrawn: false, card, clarification: None,
    });
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:problem-orchestrator-run-1", "");
    let card = card_of(&view);
    assert_eq!(card.need.as_deref(), Some("El orquestador se cortó a medias: decide si lo repite"));
    assert!(card.why.as_deref().is_some_and(|why| why.starts_with("Se detuvo antes de terminar")), "{:?}", card.why);
    assert_eq!(card.actions.iter().map(|a| (a.option_id.as_deref().unwrap(), a.label.as_str())).collect::<Vec<_>>(),
        [("continue", "★ Seguir sin repetirlo"), ("retry", "Repetirlo desde el principio")]);
    let detail = view.sections.iter().find(|section| section.id == "content").expect("the detail is the content");
    assert!(matches!(&detail.items[0], Item::Markdown(text) if text.text.contains("**Estaba atendiendo:**\n- Make the editor readable")
        && text.text.contains("**Ya había hecho:** crear una tarea.") && text.text.contains("Error técnico")), "{:?}", detail.items);
    let english = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:problem-orchestrator-run-1", "");
    assert_eq!(card_of(&english).need.as_deref(), Some(need.as_str()));
}

#[test]
fn a_request_not_split_yet_says_it_waits_for_the_users_decision() {
    let mut state = fixture();
    state.requests.insert("r-2".into(), Request::new("r-2".into(), "Still splitting".into()));
    let subtitle = |live: &LiveSnapshot| {
        let (block, row) = active_row(&state, live, Client::Ios, Lang::Es, "r-2");
        (block, row.subtitle.unwrap())
    };
    assert_eq!(subtitle(&orchestrator_waiting(true)), ("block:for-you".into(), "Esperando tu decisión".into()));
    assert_eq!(subtitle(&orchestrator_waiting(false)), ("block:in-progress".into(), "Preparando sus tareas".into()));
}

#[test]
fn a_request_not_split_yet_says_it_waits_for_a_claude_sign_in_while_no_account_has_one() {
    let mut state = fixture();
    state.requests.insert("r-2".into(), Request::new("r-2".into(), "Still splitting".into()));
    let live = |waits_for_sign_in| LiveSnapshot { orchestrator: Some(live::LiveOrchestrator {
        working: false, summary: None, failure: None, queue_depth: 1, active_run_id: None, can_retry: false, can_cancel: false, waits_for_sign_in,
    }), ..Default::default() };
    let subtitle = |live: &LiveSnapshot, lang| active_row(&state, live, Client::Ios, lang, "r-2").1.subtitle.unwrap();
    assert_eq!(subtitle(&live(true), Lang::Es), "Esperando una sesión de proveedor");
    assert_eq!(subtitle(&live(true), Lang::En), "Waiting for a provider session");
    assert_eq!(subtitle(&live(false), Lang::Es), "Preparando sus tareas", "once an account has a session it says what it does again");
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
fn a_tasks_last_message_keeps_its_time_next_to_the_message() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode {
        agent_id: "a-1".into(), short_id: None, cwd: None, name: "Node One".into(),
        project: None, state_key: None, state_label: None, summary: None, needs: None,
        background: vec![], background_badge: None,
        working: false, waiting: false,
        last_message: Some("Ready for review".into()),
        state_since: None,
        last_message_at: Some("2026-09-30T09:58:00Z".into()), ..Default::default()
    });
    let view = render(&state, &live, Client::Ios, Lang::En, "task:t-1", "");
    let section = view.sections.iter().find(|section| section.id == "last_message").unwrap();
    assert!(section.items.iter().any(|item| matches!(item, Item::Markdown(markdown) if markdown.text == "Ready for review")));
    assert!(section.items.iter().any(|item| matches!(item,
        Item::Row(row) if matches!(&row.trailing, Some(Trailing::Time { at, .. }) if at == "2026-09-30T09:58:00Z"))));
}

#[test]
fn a_task_launched_without_a_model_never_shows_its_nodes() {
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode { agent_id: "a-1".into(), name: "Build a feature".into(), model: Some("claude-opus-5-5[1m]".into()), ..Default::default() });
    let detail = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    let facts: Vec<_> = detail.sections.iter().flat_map(|section| &section.items)
        .flat_map(|item| match item { Item::Facts(facts) => facts.items.clone(), _ => vec![] }).collect();
    assert!(facts.iter().all(|fact| fact.label != "Modelo"), "{facts:?}");
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
                state: RunState::Ended { outcome: RunOutcome::Finished, ran_secs: 125 }, ..Default::default() },
        ],
        workflows: vec![LiveRun { label: "review-loop".into(), agents: Some(4), since: "2026-09-30T09:50:00Z".into(), ..Default::default() },
            LiveRun { label: "release".into(), since: "2026-09-30T09:20:00Z".into(),
                state: RunState::Ended { outcome: RunOutcome::Failed, ran_secs: 3_700 }, ..Default::default() },
            LiveRun { label: "migrate".into(), since: "2026-09-30T09:10:00Z".into(),
                state: RunState::Ended { outcome: RunOutcome::Stopped, ran_secs: 30 }, ..Default::default() }],
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
fn a_task_detail_shows_its_nodes_steps_subagents_and_workflows() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    let mut live = LiveSnapshot::default();
    live.nodes.push(working_node());
    let view = render(&state, &live, Client::Ios, Lang::En, "task:t-1", "");
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
    assert_eq!(workflows.iter().map(|row| badges(row)).collect::<Vec<_>>(), [["Running"], ["Failed"], ["Stopped"]]);
    assert!(matches!(&workflows[1].trailing, Some(Trailing::Text { text }) if text == "1h 01m"));

    let spanish = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    assert_eq!(section_facts(&spanish, "steps"), [("Progreso".into(), "2 de 5".into()), ("Ahora".into(), "Running the tests".into())]);
    assert_eq!(spanish.sections.iter().filter_map(|section| section.title.as_deref())
        .filter(|title| ["Pasos", "Subagentes", "Flujos de trabajo"].contains(title)).count(), 3);
    assert_eq!(badges(node_work_rows(&spanish, "steps")[2]), ["En curso"]);
    assert_eq!(badges(node_work_rows(&spanish, "subagents")[1]), ["Terminado"]);
    assert_eq!(node_work_rows(&spanish, "workflows").iter().map(|row| badges(row)).collect::<Vec<_>>(), [["En marcha"], ["Falló"], ["Detenido"]]);
    assert_eq!(node_work_rows(&spanish, "workflows")[0].subtitle.as_deref(), Some("4 agentes"));

    let quiet = render(&state, &LiveSnapshot { nodes: vec![LiveNode { agent_id: "a-1".into(), ..Default::default() }], ..Default::default() },
        Client::Ios, Lang::En, "task:t-1", "");
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
    assert_eq!(node_work_rows(&view, "workflows").len(), 3);
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    let done = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    assert!(done.sections.iter().all(|section| section.id != "steps"));
}

fn node_on_its_task() -> LiveNode {
    LiveNode {
        agent_id: "a-1".into(), name: "Build a feature".into(), working: true,
        cwd: Some("/work/alondra/.claude/worktrees/t-1".into()),
        tool: Some("Bash: cargo test".into()), background_badge: Some("2 subagents · 1 shell".into()),
        context_percent: Some(84), context_window: Some(200_000), cost_cents: Some(120),
        needs: Some("The staging key".into()),
        background: vec![
            LiveBackground { kind: "shell".into(), label: "npm run dev".into(), since: "2026-09-30T09:40:00Z".into(), state: RunState::Running },
            LiveBackground { kind: "shell".into(), label: "swift build".into(), since: "2026-09-30T09:30:00Z".into(),
                state: RunState::Ended { outcome: RunOutcome::Failed, ran_secs: 42 } },
        ],
        last_message: Some("Tests pass; opening the PR".into()), last_message_at: Some("2026-09-30T09:58:00Z".into()),
        ..Default::default()
    }
}

fn task_working_fixture() -> (DomainState, LiveSnapshot) {
    let mut state = fixture();
    node_in(&mut state, NodePhase::Working);
    let mut live = LiveSnapshot::default();
    live.nodes.push(LiveNode { model: Some("claude-opus-5-5".into()), ..node_on_its_task() });
    live.nodes[0].steps = working_node().steps;
    live.task_costs.insert("t-1".into(), 120);
    state.tasks.get_mut(&"t-1".into()).unwrap().worktrees.insert("/work/alondra/.claude/worktrees/t-1".into(),
        crate::domain::TaskWorktree { origin: crate::domain::WorktreeOrigin::Task, branch: None, discard: false, end: None });
    live.activity.insert("a-1".into(), vec![LiveActivity { agent_id: None, agent_type: None, task: None,
        tool: Some("Edit".into()), summary: Some("Editing views/tasks.rs".into()), since: "2026-09-30T09:59:00Z".into() }]);
    (state, live)
}

const TASK_WORKING_GOLDENS: [(&str, Client, Lang); 4] = [
    ("task-working-ios.json", Client::Ios, Lang::En),
    ("task-working-ios-es.json", Client::Ios, Lang::Es),
    ("task-working-mac.json", Client::Mac, Lang::En),
    ("task-working-tui.json", Client::Tui, Lang::En),
];

#[test]
fn task_working_views_match_goldens() {
    let (state, live) = task_working_fixture();
    for (filename, client, lang) in TASK_WORKING_GOLDENS {
        let expected = match filename {
            "task-working-ios.json" => include_str!("../../../../fixtures/views/cards/task-working-ios.json"),
            "task-working-ios-es.json" => include_str!("../../../../fixtures/views/cards/task-working-ios-es.json"),
            "task-working-mac.json" => include_str!("../../../../fixtures/views/cards/task-working-mac.json"),
            _ => include_str!("../../../../fixtures/views/cards/task-working-tui.json"),
        };
        assert_golden(filename, &render(&state, &live, client, lang, "task:t-1", "2026-09-30T10:00:00Z"), expected);
    }
}

/// A call that runs for hours is still work: the task says what runs and since when, never that
/// the node stalled, until the call, its own timeout or Claude ends it.
#[test]
fn a_long_call_reads_running_since_its_start() {
    let (state, mut live) = task_working_fixture();
    live.nodes[0].tool_since = Some("09:12".into());
    let english = render(&state, &live, Client::Tui, Lang::En, "task:t-1", "");
    assert_eq!(section_facts(&english, "readings")[0], ("Running".into(), "Bash: cargo test · since 09:12".into()));
    let spanish = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    assert_eq!(section_facts(&spanish, "readings")[0], ("Ejecutando".into(), "Bash: cargo test · desde las 09:12".into()));
}

/// The task is the unit: what the node's screen showed about the node working on it is on the
/// task's own screen, for every client, while that task is the node's current one.
#[test]
fn a_task_detail_shows_what_its_node_does_while_it_works_on_it() {
    let (mut state, mut live) = task_working_fixture();
    live.nodes[0].model = None;
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let view = render(&state, &live, client, Lang::En, "task:t-1", "");
        assert!(section_facts(&view, "header").contains(&("Folder".into(), "/work/alondra/.claude/worktrees/t-1".into())), "{client:?}");
        assert_eq!(section_facts(&view, "readings"), [
            ("Running".into(), "Bash: cargo test".into()),
            ("In the background".into(), "2 subagents · 1 shell".into()),
            ("Context".into(), "84% of 200K tokens".into()),
            ("Cost".into(), "$1.20".into()),
        ], "{client:?}");
        let ids: Vec<&str> = view.sections.iter().map(|section| section.id.as_str()).collect();
        for id in ["activity", "needs", "background", "last_message"] {
            assert!(ids.contains(&id), "{client:?} shows {id}: {ids:?}");
        }
        assert_eq!(node_work_rows(&view, "activity")[0].title, "Editing views/tasks.rs");
        assert_eq!(node_work_rows(&view, "background").iter().map(|row| badges(row)).collect::<Vec<_>>(), [["Running"], ["Failed"]]);
        assert!(action_ids(&view).iter().any(|id| id == "interrupt:a-1"), "{client:?}: {:?}", action_ids(&view));
    }
    let spanish = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    assert_eq!(section_facts(&spanish, "readings")[3], ("Coste".into(), "1,20 $".into()));

    state.agents.get_mut(&"a-1".into()).unwrap().phase = NodePhase::WaitingUser { in_turn: false };
    live.nodes[0].last_turn_aborted = true;
    let resting = render(&state, &live, Client::Ios, Lang::En, "task:t-1", "");
    assert!(section_facts(&resting, "readings").contains(&("Last turn".into(), "Interrupted".into())));
    assert!(resting.sections.iter().all(|section| section.id != "activity"), "nothing runs right now once its turn ended");
    assert!(action_ids(&resting).iter().all(|id| !id.starts_with("interrupt:")), "nothing to interrupt once its turn ended");

    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    let done = render(&state, &live, Client::Ios, Lang::En, "task:t-1", "");
    assert!(done.sections.iter().all(|section| !["needs", "background", "last_message"].contains(&section.id.as_str())));
    assert_eq!(section_facts(&done, "readings"), [("Cost".into(), "$1.20".into())], "an ended task keeps only its cost");
    assert!(section_facts(&done, "header").contains(&("Folder".into(), "/work/alondra/.claude/worktrees/t-1".into())), "an ended task keeps its folder");
}

#[test]
fn a_task_detail_says_the_model_its_node_runs_on_when_it_is_not_the_one_it_was_launched_with() {
    let mut state = fixture();
    node_in(&mut state, NodePhase::Working);
    let model = |state: &DomainState, running: Option<&str>| {
        let live = LiveSnapshot { nodes: vec![LiveNode { agent_id: "a-1".into(), model: running.map(Into::into), ..Default::default() }], ..Default::default() };
        section_facts(&render(state, &live, Client::Ios, Lang::En, "task:t-1", ""), "options").into_iter()
            .find(|(label, _)| label == "Model").map(|(_, value)| value)
    };
    assert_eq!(model(&state, Some("claude-opus-5-5[1m]")), None, "the model line is the task's, never only its node's");
    state.tasks.get_mut(&"t-1".into()).unwrap().model = Some(TaskModel { model: "sonnet".into(), chosen_by: ModelChooser::User });
    assert_eq!(model(&state, Some("claude-sonnet-5-5")).as_deref(), Some("Sonnet · from your Effort choice"));
    assert_eq!(model(&state, Some("claude-opus-5-5")).as_deref(), Some("Sonnet · from your Effort choice (now: Opus 5.5)"));
}

#[test]
fn a_task_detail_says_what_its_node_still_does_after_the_merge() {
    let mut state = fixture();
    node_in(&mut state, NodePhase::Closing);
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Closing;
    state.tasks.get_mut(&"t-1".into()).unwrap().wrap_up.post_merge_work =
        Some(crate::domain::PostMergeWork { reason: "release.sh publishes 0.2.54".into(), declared_at: 1, turn_since: false });
    let now = |lang| {
        let view = render(&state, &LiveSnapshot::default(), Client::Ios, lang, "task:t-1", "");
        node_work_rows(&view, "now").first().map(|row| row.title.clone())
    };
    assert_eq!(now(Lang::En).as_deref(), Some("finishing after the merge: release.sh publishes 0.2.54"));
    assert_eq!(now(Lang::Es).as_deref(), Some("terminando tras el merge: release.sh publishes 0.2.54"));
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
    node.workflows.truncate(1);
    let mut agents = vec![workflow_agent("review-auth", "Review", Some("haiku"), true),
        workflow_agent("review-db", "Review", Some("claude-sonnet-5-5"), false)];
    agents.extend((0..6).map(|index| workflow_agent(&format!("scan-{index}"), "Scan", None, false)));
    node.workflows[0].agents = Some(10);
    node.workflows[0].workflow = Some(LiveWorkflow {
        phase: Some("Review".into()), phase_index: Some(2), phase_count: Some(4), running: 1, done: 9, agents, ..Default::default()
    });
    let live = LiveSnapshot { nodes: vec![node], ..Default::default() };
    {
        let english = render(&state, &live, Client::Ios, Lang::En, "task:t-1", "");
        assert_eq!(section_facts(&english, "workflows"), [("Phase".into(), "Review (2 of 4)".into()), ("Agents".into(), "1 running · 9 done".into())]);
        let rows = node_work_rows(&english, "workflows");
        assert_eq!(rows.len(), 1 + 8 + 1);
        assert_eq!((rows[1].title.as_str(), rows[1].subtitle.as_deref(), badges(rows[1])), ("review-auth", Some("Review · Haiku"), vec!["Running"]));
        assert_eq!((rows[2].subtitle.as_deref(), badges(rows[2])), (Some("Review · Sonnet 5.5"), vec!["Done"]));
        assert_eq!(rows[3].subtitle.as_deref(), Some("Scan"));
        assert_eq!(rows[9].title, "+2 more");
        assert!(rows.iter().all(|row| row.link.is_none()));
    }

    let spanish = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    assert_eq!(section_facts(&spanish, "workflows"), [("Fase".into(), "Review (2 de 4)".into()), ("Agentes".into(), "1 en marcha · 9 terminados".into())]);
    let rows = node_work_rows(&spanish, "workflows");
    assert_eq!((badges(rows[1]), badges(rows[2]), rows[9].title.as_str()), (vec!["En marcha"], vec!["Hecho"], "+2 más"));

    let mut finished = live.clone();
    let workflow = finished.nodes[0].workflows[0].workflow.as_mut().unwrap();
    *workflow = LiveWorkflow { phase: Some("Second".into()), running: 0, done: 3, total_tokens: Some(88_896),
        agents: vec![workflow_agent("count-c", "Second", Some("claude-haiku-4-5-20251001"), false)], ..Default::default() };
    let view = render(&state, &finished, Client::Ios, Lang::En, "task:t-1", "");
    assert_eq!(section_facts(&view, "workflows"), [("Phase".into(), "Second".into()), ("Agents".into(), "0 running · 3 done".into()), ("Tokens".into(), "88K".into())]);
    assert_eq!(node_work_rows(&view, "workflows").last().unwrap().title, "+2 more");
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
fn a_task_detail_names_the_claude_account_its_node_runs_on_once_there_are_several() {
    use crate::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, Provider};
    let mut state = fixture();
    let account = |state: &DomainState, lang| {
        let view = render(state, &LiveSnapshot::default(), Client::Ios, lang, "task:t-1", "");
        view.sections.iter().flat_map(|section| &section.items)
            .flat_map(|item| match item { Item::Facts(facts) => facts.items.clone(), _ => vec![] })
            .find(|fact| fact.label == "Claude account" || fact.label == "Cuenta de Claude").map(|fact| (fact.label, fact.value))
    };
    state.tasks.get_mut(&"t-1".into()).unwrap().account = Some(AccountId::default_account());
    assert_eq!(account(&state, Lang::En), None, "with the one default account there is nothing to tell apart");
    let status = |id: &str, name: &str, billing| AccountStatus { provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: name.into(),
        state: AccountState::SignedIn, native_identity: None, email: None, plan: None, billing, sign_ins: 0 };
    state.accounts = vec![status("default", "Principal", AccountBilling::default()), status("work", "Trabajo", AccountBilling::default()),
        status("api", "API", AccountBilling::PayPerUse)];
    assert_eq!(account(&state, Lang::En), Some(("Claude account".into(), "Principal".into())));
    state.tasks.get_mut(&"t-1".into()).unwrap().account = Some(AccountId::parse("work").unwrap());
    assert_eq!(account(&state, Lang::Es), Some(("Cuenta de Claude".into(), "Trabajo".into())));
    state.tasks.get_mut(&"t-1".into()).unwrap().account = Some(AccountId::parse("api").unwrap());
    assert_eq!(account(&state, Lang::En).unwrap().1, "API · paid per use");
    assert_eq!(account(&state, Lang::Es).unwrap().1, "API · de pago por uso");
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
            Item::Row(row) => vec![row.title.clone()],
            _ => vec![],
        }).collect();
        assert!(texts.iter().any(|text| text == expected), "{lang:?}: the task says why it waits: {texts:?}");
        assert!(texts.iter().any(|text| text.starts_with(match lang { Lang::En => "Needs you · ", Lang::Es => "Te necesita · " })), "{lang:?}: {texts:?}");
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
                let index = crate::domain::ReadIndex::build(&domain);
                let context = RenderContext { domain: &domain, index: &index, live: &live, client, lang, transport: Transport::Loopback,
                    cursor: "cursor-1", rendered_at: "" };
                for item in inbox_items(&domain) {
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
fn every_card_can_be_replied_to_in_text_with_attachments() {
    let state = fixture();
    for id in ["i-1", "i-2", "i-3", "i-4"] {
        let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, &format!("card:{id}"), "2026-09-30T10:00:00Z");
        let Item::Card(card) = &view.sections[0].items[0] else { panic!("card") };
        let reply = card.reply.as_ref().unwrap_or_else(|| panic!("{id} has a reply"));
        let file_alone = card.kind == Some(crate::domain::CardKind::Provide);
        assert!(matches!(&reply.input, Input::Text { required, multiline: true, .. } if *required != file_alone),
            "{id}: a Provide card is answered with a file alone too");
        assert!(reply.attachments, "{id}");
        assert!(reply.enabled, "{id}");
    }
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-2", "");
    let Item::Card(information) = &view.sections[0].items[0] else { panic!("card") };
    assert!(information.reply.as_ref().unwrap().id.starts_with("reply:i-2@"), "a task's card replies to its node");
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-4", "");
    let Item::Card(orchestrator) = &view.sections[0].items[0] else { panic!("card") };
    let answer = orchestrator.reply.as_ref().unwrap();
    assert!(answer.id.starts_with("reply:i-4@"), "words under the orchestrator's open question answer it: {}", answer.id);
    assert!(answer.resolves_card, "the answered question leaves the queue");
    assert_eq!(answer.stamp.as_deref(), Some("You answered"));
}

#[test]
fn words_under_an_orchestrator_decide_answer_it_on_every_client() {
    use crate::domain::{CardKind, QuestionOption};
    let mut state = fixture();
    let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
    let decide = state.interactions.get_mut(&"i-4".into()).unwrap();
    decide.scope = Scope::Orchestrator { request_id: Some("r-1".into()) };
    decide.payload = InteractionPayload::Question { options: vec![option("mine", "I'll do it"), option("new_task", "Create a task")], answer: None };
    decide.card = spec(CardKind::Decide, "Decide who fixes the memory pauses", "A node found a loop.");
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let view = render(&state, &LiveSnapshot::default(), client, Lang::Es, "card:i-4", "");
        let Item::Card(card) = &view.sections[0].items[0] else { panic!("card") };
        let reply = card.reply.as_ref().expect("the decide card has a reply field");
        assert!(reply.id.starts_with("reply:i-4@"), "{client:?}: {}", reply.id);
        assert!(reply.resolves_card, "{client:?}");
        assert_eq!(reply.stamp.as_deref(), Some("Respondiste"), "{client:?}");
    }
    state.holds.insert("i-4".into(), crate::domain::Hold::Permission);
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-4", "");
    let Item::Card(held) = &view.sections[0].items[0] else { panic!("card") };
    assert!(held.reply.as_ref().unwrap().id.starts_with("reply:i-4@"), "words under a held question answer it too");
    state.holds.clear();
    let InteractionPayload::Question { answer, .. } = &mut state.interactions.get_mut(&"i-4".into()).unwrap().payload else { unreachable!() };
    *answer = Some(crate::domain::Answer { option_id: "mine".into(), inputs: Default::default(), attachments: vec![], responder: "u".into(), at_revision: Default::default(), decision: None });
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-4", "");
    if let Some(Item::Card(answered)) = view.sections.first().and_then(|section| section.items.first()) {
        assert!(answered.reply.as_ref().is_none_or(|reply| reply.id.starts_with("msg:view@")), "an answered question is not answered twice");
    }
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
fn the_mac_gets_the_pending_requests_by_the_id_it_sent_them_with_and_the_terminal_does_not() {
    let mut state = fixture();
    state.requests.insert("r-from-the-mac".into(), Request::new("r-from-the-mac".into(), "Polish the beta".into()));
    let sent = |client| {
        render(&state, &LiveSnapshot::default(), client, Lang::En, "new_request", "")
            .sections.into_iter().find(|section| section.id == "sent")
    };
    let mac = sent(Client::Mac).expect("the Mac shows the queue under the composer");
    assert!(mac.items.iter().any(|item| matches!(item, Item::Row(row) if row.id == "r-from-the-mac")));
    assert!(sent(Client::Tui).is_none());
}

#[test]
fn the_mac_new_request_reads_in_spanish_like_the_iphone() {
    let mut state = golden_fixture();
    state.requests.insert("r-es".into(), Request::new("r-es".into(), "Pule la beta".into()));
    let mac = render(&state, &golden_live(), Client::Mac, Lang::Es, "new_request", "");
    let form = new_request_form(&mac);
    let pr = form.fields.iter().find(|field| field.id == "pr").expect("the PR option");
    assert_eq!(pr.label, "PRs");
    let FieldKind::Choice { items } = &pr.kind else { panic!("a choice") };
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["Revisar cada una", "Merge automático"]);
    let sent = mac.sections.iter().find(|section| section.id == "sent").expect("the queue");
    assert_eq!(sent.title.as_deref(), Some("Solicitudes pendientes"));
    assert!(sent.items.iter().all(|item| matches!(item, Item::Row(row) if row.badges.iter().all(|badge| badge.text == "En cola"))));
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
fn an_answered_request_shows_its_answer() {
    let mut state = fixture();
    let mut answered = Request::new("r-answered".into(), "Does make e2e-sdk pass?".into());
    (answered.closed, answered.answer, answered.origin) = (true, Some("It passes again since PR #1153".into()), RequestOrigin::Orchestrator);
    state.requests.insert("r-answered".into(), answered);
    for (lang, title) in [(Lang::En, "Answer"), (Lang::Es, "Respuesta")] {
        let view = render(&state, &LiveSnapshot::default(), Client::Tui, lang, "request:r-answered", "");
        let section = view.sections.iter().find(|section| section.id == "answer").expect("an answer section");
        assert_eq!(section.title.as_deref(), Some(title));
        assert!(matches!(section.items.as_slice(), [Item::Markdown(m)] if m.text == "It passes again since PR #1153"), "{:?}", section.items);
    }
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "request:r-1", "");
    assert!(view.sections.iter().all(|section| section.id != "answer"), "an open request has no answer yet");
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
    let folded = render(&state, &live, Client::Ios, Lang::En, "tasks", "");
    let finished = folded.sections.iter().find(|section| section.id == "finished").unwrap();
    assert!(finished.items.is_empty(), "the list only says how many finished requests there are");
    let more = finished.more.as_ref().unwrap();
    assert_eq!((more.key.as_str(), more.remaining, finished.count), ("tasks?section=finished&after=0", 25, Some(25)));
    let first = render(&state, &live, Client::Ios, Lang::En, &more.key, "");
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
        "Squash and merge", "Merge commit", "Rebase and merge", "Merge", "Alondra", "Alondra · iOS", "Alondra · iOS · Demo Garden", "Alondra · Demo Garden", "Demo Garden", "PRs", "squash", "rebase", "Polish the Alondra beta", "Restore the native review experience", "PR #12", "1 · PR #12", "example/repo #12", "Base",
        "Memory", "Disk available", "6.1 GB of 16 GB", "120 GB", "Waiting for you",
        "Keep the warmer copy", "Ship the concise copy", "Ship it", "Hold it back", "Normal",
        "A decision on the welcome copy", "Reviewing the native beta experience",
        "The simulator pass completed successfully.", "Started", "Waiting for a decision",
        "Reading the localization catalog", "API", "Principal", "Trabajo", "Claude · Principal", "Claude · Trabajo",
    ];
    const FIXTURE_TEXT_PREFIXES: &[&str] = &[
        "## Beta candidate ready", "All required checks passed", "Prepare the next Alondra beta",
        "The iPhone simulator pass", "The new empty state is ready", "The pull request is ready",
        "Add the compact schedule editor", "1 · Add the compact schedule editor",
        "Replaces the long schedule form", "feature/compact-schedule-editor",
        "Choose the welcome copy", "Concise:", "- Swipe", "Review the beta candidate", "Ship the compact schedule editor",
        "Swipe navigation, inline", "The pull request checks passed", "The review of the pull request",
        "The simulator pass is clean", "★ Ship the concise copy", "Beta candidate ready",
        "Restore the native review experience",
    ];
    let state = golden_fixture();
    let live = golden_live();
    let keys = [
        ("home", Client::Ios), ("home", Client::Tui),
        ("card:a98d034b-249e-4487-a89b-57de38c26372", Client::Ios),
        ("card:f12d034b-249e-4487-a89b-57de38c26372", Client::Ios),
        ("card:github.com/example/repo#12", Client::Ios),
        ("card:hf-weekly-2026-09-30", Client::Ios),
        ("tasks", Client::Ios), ("new_request", Client::Ios),
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

/// Says "fusion" or "fusión" in any case: in Spanish a pull request gets a merge.
fn says_fusionar(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["fusi\u{f3}n", "fusion"].iter().any(|word| lower.contains(word))
}

#[test]
fn spanish_says_merge_for_a_pull_request() {
    let sources = [
        ("accounts.rs", include_str!("accounts.rs")), ("action_id.rs", include_str!("action_id.rs")),
        ("allow_always.rs", include_str!("allow_always.rs")), ("card.rs", include_str!("card.rs")),
        ("common.rs", include_str!("common.rs")), ("components.rs", include_str!("components.rs")),
        ("details.rs", include_str!("details.rs")), ("task_node.rs", include_str!("task_node.rs")),
        ("home.rs", include_str!("home.rs")), ("live.rs", include_str!("live.rs")),
        ("mcp_servers.rs", include_str!("mcp_servers.rs")), ("messages.rs", include_str!("messages.rs")),
        ("misc.rs", include_str!("misc.rs")), ("mod.rs", include_str!("mod.rs")), ("pacing.rs", include_str!("pacing.rs")),
        ("resolves.rs", include_str!("resolves.rs")), ("review_export.rs", include_str!("review_export.rs")),
        ("schedules.rs", include_str!("schedules.rs")), ("sidebar.rs", include_str!("sidebar.rs")),
        ("status.rs", include_str!("status.rs")), ("summary.rs", include_str!("summary.rs")),
        ("task_state.rs", include_str!("task_state.rs")), ("tasks.rs", include_str!("tasks.rs")),
    ];
    let mut found: Vec<String> = sources.iter().flat_map(|(file, source)| source.lines().enumerate()
        .filter(|(_, line)| says_fusionar(line)).map(move |(number, line)| format!("{file}:{}: {}", number + 1, line.trim())))
        .collect();
    let live = golden_live();
    for state in [golden_fixture(), golden_in_progress()] {
        for (key, client) in [
            ("home", Client::Ios), ("home", Client::Tui), ("card:github.com/example/repo#12", Client::Ios),
            ("card:github.com/example/repo#12", Client::Mac), ("card:hf-weekly-2026-09-30", Client::Ios),
            ("tasks", Client::Ios), ("new_request", Client::Ios),
            ("task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Ios), ("pull_request:github.com/example/repo#12", Client::Ios),
            ("machine", Client::Ios), ("schedule:new", Client::Ios), ("sidebar", Client::Mac),
        ] {
            let view = render(&state, &live, client, Lang::Es, key, "2026-09-30T10:00:00Z");
            let mut leaves = vec![];
            string_leaves(&serde_json::to_value(view).unwrap(), "", &mut leaves);
            found.extend(leaves.into_iter().filter(|(_, text)| says_fusionar(text)).map(|(path, text)| format!("{key} {path}: {text}")));
        }
    }
    assert!(found.is_empty(), "in Spanish a PR gets a merge, never a fusión: {found:#?}");
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
        Item::Facts(facts) => Some(facts.items.iter().map(|fact| fact.value.clone()).collect()),
        _ => None,
    }).flatten().collect()
}

#[test]
fn scheduled_run_agent_shows_the_task_title_never_the_raw_run_name() {
    let texts = card_context_texts(&scheduled_run_fixture(true));
    assert!(texts.iter().any(|text| text.contains("Build a feature")), "{texts:?}");
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
        Some("example/repo · feature/compact-schedule-editor → main · 4 files · +120 −30 · 2 of 3 passed, 1 running")
    );
    assert!(card.body.is_none(), "the card holds none of it");
    let row = card.content_row.as_ref().expect("content row");
    assert_eq!((row.kind.as_str(), row.label.as_str(), row.summary.as_str()), ("diff", "Changes", "4 files · +120 −30"));
    assert_eq!(
        content_checks(&view).unwrap().items.iter().map(|check| (check.name.as_str(), check.state)).collect::<Vec<_>>(),
        [("Lint", CheckState::Pending), ("Build", CheckState::Pass), ("Unit tests", CheckState::Pass)],
        "the ones still running come before the ones that passed"
    );
    let summary = content_checks(&view).unwrap().summary.clone().unwrap();
    assert_eq!((summary.text.as_str(), summary.tone), ("2 of 3 passed, 1 running", Tone::Info));
    let pill = card.state.as_ref().unwrap();
    assert_eq!((pill.text.as_str(), pill.tone), ("Checks running", Tone::Info));
    let spanish = render(&state, &golden_live(), Client::Ios, Lang::Es, card_key, "");
    assert_eq!(card_of(&spanish).content_row.as_ref().unwrap().summary, "4 archivos · +120 −30");
    assert_eq!(
        card_of(&spanish).meta.as_deref(),
        Some("example/repo · feature/compact-schedule-editor → main · 4 archivos · +120 −30 · 2 de 3 correctos, 1 en marcha")
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
    let state = reviewed_fixture(crate::domain::Verdict::Approved { summary: "4 fixed".into(), open_issues: false, serious_findings: 0 });
    assert!(context_texts(&state, Lang::En).contains(&"Reviewed by “Review the editor”: 4 fixed".to_owned()));
    assert!(context_texts(&state, Lang::Es).contains(&"Revisado por «Review the editor»: 4 fixed".to_owned()));
}

#[test]
fn a_new_delivery_after_the_review_carries_no_line_of_that_review() {
    let state = reviewed_fixture_after(crate::domain::Verdict::Approved { summary: "4 fixed".into(), open_issues: false, serious_findings: 0 }, false);
    assert!(!context_texts(&state, Lang::En).iter().any(|line| line.starts_with("Reviewed by")), "the review saw an older delivery");
}

#[test]
fn one_failure_left_is_said_in_the_singular_in_both_languages() {
    let state = reviewed_fixture(crate::domain::Verdict::Approved { summary: "Empty list crashes".into(), open_issues: true, serious_findings: 0 });
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
    let state = reviewed_fixture(crate::domain::Verdict::Approved { summary: "Empty list crashes\nWrong total".into(), open_issues: true, serious_findings: 0 });
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
fn a_pull_request_without_ci_shows_no_checks_and_its_state_comes_from_merging() {
    let state = golden_fixture();
    let mut live = golden_live();
    live.pull_requests[0] = LivePullRequest { id: "github.com/example/repo#12".into(), fetched: true, ..Default::default() };
    let key = "card:github.com/example/repo#12";
    let view = render(&state, &live, Client::Ios, Lang::En, key, "");
    let card = card_of(&view);
    assert_eq!(card.content_row.as_ref().unwrap().summary, "example/repo #12");
    assert_eq!(card.meta.as_deref(), Some("example/repo · → main"), "no checks, so no line about them");
    assert_eq!(card.title, "Review the beta candidate", "without GitHub's title the node's own is kept");
    assert!(content_checks(&view).is_none(), "no checks section without checks");
    let ready = card.state.clone().unwrap();
    assert_eq!((ready.text.as_str(), ready.tone), ("Ready to merge", Tone::Success));
    live.pull_requests[0].conflicts = true;
    let conflicts = render(&state, &live, Client::Ios, Lang::Es, key, "");
    assert_eq!(card_of(&conflicts).state.as_ref().unwrap().text, "Conflictos");
    let screen = render(&state, &live, Client::Ios, Lang::Es, "pull_request:github.com/example/repo#12", "");
    assert!(section_of(&screen, "checks").is_none());
    let Some(Item::Facts(status)) = section_of(&screen, "status").and_then(|section| section.items.first()) else { panic!("a state") };
    assert_eq!((status.items[0].value.as_str(), status.items[0].tone), ("Conflictos", Some(Tone::Warning)));
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
            attachments: vec![], option_id: "ship".into(), inputs: BTreeMap::new(), responder: "u".into(), at_revision: 1, decision: None,
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
            options: (0..options).map(|n| QuestionOption { id: format!("o{n}"), label: format!("Option {n}"), description: None }).collect(),
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
fn card_task_line_links_to_the_task_with_its_state_next_to_the_project() {
    let view = render(&golden_fixture(), &golden_live(), Client::Ios, Lang::En,
        "card:a98d034b-249e-4487-a89b-57de38c26372", "");
    let line = card_of(&view).context.iter().find(|line| line.icon == Icon::Task).unwrap();
    assert_eq!(line.text, "Restore the native review experience");
    assert_eq!(line.link, Some(Link::View { key: "task:t-651c38484cd2f34e74d5107ba05f7f15".into() }));
    let state = line.state.as_ref().expect("task state");
    assert_eq!((state.text.as_str(), state.tone), ("Needs you", Tone::Warning));
    assert_eq!(line.chip.as_ref().map(|chip| chip.text.as_str()), Some("Demo Garden"));
    let bare = render(&golden_fixture(), &LiveSnapshot::default(), Client::Ios, Lang::En,
        "card:a98d034b-249e-4487-a89b-57de38c26372", "");
    let bare_line = card_of(&bare).context.iter().find(|line| line.icon == Icon::Task).unwrap();
    assert_eq!(bare_line.state.as_ref(), Some(state), "the task's state shows without a live node too");
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
fn task_detail_names_its_folder_and_what_its_node_is_doing_right_now() {
    let agent = "d887a34b-249e-4487-a89b-57de38c26372";
    let key = "task:t-651c38484cd2f34e74d5107ba05f7f15";
    let mut in_turn = golden_in_progress();
    in_turn.tasks.get_mut(&key["task:".len()..].into()).unwrap().worktrees.insert("/demo/Demo Garden/ios".into(),
        crate::domain::TaskWorktree { origin: crate::domain::WorktreeOrigin::Inherited, branch: None, discard: false, end: None });
    in_turn.agents.get_mut(&agent.into()).unwrap().phase = NodePhase::WaitingUser { in_turn: true };
    let mut stopped = golden_in_progress();
    stopped.agents.get_mut(&agent.into()).unwrap().phase = NodePhase::WaitingUser { in_turn: false };
    let stopped = render(&stopped, &golden_live(), Client::Ios, Lang::En, key, "");
    assert!(stopped.sections.iter().all(|section| section.id != "activity"), "a node out of its turn does nothing right now");
    let view = render(&in_turn, &golden_live(), Client::Ios, Lang::En, key, "");
    assert!(header_facts(&view).iter().any(|fact| fact.label == "Folder" && fact.value == "/demo/Demo Garden/ios"));
    let now = view.sections.iter().find(|section| section.id == "activity").expect("right now section");
    assert_eq!(now.title.as_deref(), Some("Right now"));
    let Item::Row(row) = &now.items[0] else { panic!("thread row") };
    assert_eq!(row.title, "Reading the localization catalog");
    let idle = render(&in_turn, &LiveSnapshot::default(), Client::Ios, Lang::En, key, "");
    assert!(idle.sections.iter().all(|section| section.id != "activity"));
    assert!(header_facts(&idle).iter().any(|fact| fact.label == "Folder" && fact.value == "/demo/Demo Garden/ios"), "the folder is the task's");
}

#[test]
fn right_now_names_another_task_by_its_title_and_not_at_all_the_tasks_own() {
    let agent = "d887a34b-249e-4487-a89b-57de38c26372";
    let mut state = golden_in_progress();
    state.agents.get_mut(&agent.into()).unwrap().phase = NodePhase::WaitingUser { in_turn: true };
    let task = "t-651c38484cd2f34e74d5107ba05f7f15".to_owned();
    let mut other = state.tasks[&task.as_str().into()].clone();
    (other.id, other.title, other.agent_id) = ("t-other".into(), "Check the Spanish strings".into(), None);
    state.tasks.insert(other.id.clone(), other);
    let mut live = golden_live();
    let row = |live: &LiveSnapshot| {
        let detail = render(&state, live, Client::Ios, Lang::En, &format!("task:{task}"), "");
        detail.sections.iter().find(|found| found.id == "activity")
            .and_then(|found| match &found.items[0] { Item::Row(row) => Some(row.clone()), _ => None })
            .unwrap_or_else(|| panic!("no «Right now» row"))
    };
    live.activity.get_mut(agent).unwrap()[0].task = Some("t-other".into());
    assert_eq!(row(&live).subtitle.as_deref(), Some("Subagent · general-purpose · Task Check the Spanish strings"), "never the task's id");
    live.activity.get_mut(agent).unwrap()[0].task = Some(task.clone());
    let own = row(&live);
    assert_eq!(own.title, "Reading the localization catalog");
    assert_eq!(own.subtitle.as_deref(), Some("Subagent · general-purpose"), "the detail is that task's already");
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
    assert_eq!(pr[1].as_deref(), Some("Alondra hace merge de la PR cuando pasan sus comprobaciones"));
    assert_eq!(effort[2].as_deref(), Some("Trabajo normal con los perfiles de tarea configurados"));
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
    assert!(buttons.iter().all(|action| action.input == Input::None), "Deny answers at once");
    assert!(card.reply.as_ref().is_some_and(|reply| matches!(&reply.input, Input::Text { placeholder, .. } if placeholder == "Denegar con un motivo…")),
        "a reason is written in the card's field: {:?}", card.reply);
    assert!(card.actions.iter().all(|action| !action.id.starts_with("ctx:")), "a held card is answered, not discussed");
    assert!(card.more.iter().all(|action| !action.id.starts_with("clarify:")), "Claude Code waits on it: no clarification");
    assert_eq!(view.status.summary.split(" · ").next(), Some("2 preguntas"));
    let pushed = push_actions(&state, "i-1", Lang::En).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.option.as_deref(), a.label.as_str(), a.text, a.open)).collect::<Vec<_>>(),
        [(Some("allow"), "Allow", false, false), (Some("deny"), "Deny", false, false), (None, "Reply", true, false)]);
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
        ("request_input", vec![("provide", "Send the file"), ("skip", "I can't provide it")]),
        ("request_input", vec![("provide", "Send the text"), ("skip", "I can't provide it")]),
        ("request_input", vec![("provide", "Send the secret"), ("skip", "I can't provide it")]),
        ("problem", vec![("continue", "Continue without running it again"), ("retry", "Run it again from the start")]),
    ]
}

fn fixed_answers(options: &[(&str, &str)]) -> InteractionPayload {
    InteractionPayload::Question {
        options: options.iter().map(|(id, label)| crate::domain::QuestionOption { id: (*id).into(), label: (*label).into(), description: None }).collect(),
        answer: None,
    }
}

fn option_labels(options: &[(&str, &str)], lang: Lang) -> Vec<String> {
    let mut state = fixture();
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = fixed_answers(options);
    interaction.card = spec(crate::domain::CardKind::Decide, "Fix it", "Because.");
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
fn a_permission_card_that_can_stop_asking_offers_allow_always_saying_exactly_what_stops_asking() {
    use crate::allow_always::AllowAlways;
    use crate::domain::{CardKind, Hold};
    let mut state = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow building", ""));
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = InteractionPayload::Question { options: Hold::Permission.options_offering(true), answer: None };
    interaction.card.allow_always = Some(AllowAlways::Offered { repo: "alondra".into(),
        rules: vec!["make mac-snapshots-record *".into()], quiet: vec!["tail".into(), "git status".into()] });
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!(answers(card).iter().map(|a| (a.option_id.as_deref().unwrap(), a.label.as_str(), a.role)).collect::<Vec<_>>(),
        [("allow", "Permitir", Some(Role::Risk)), ("always", "Permitir siempre", Some(Role::Neutral)),
         ("deny", "Denegar", Some(Role::Neutral))]);
    assert_eq!(answers(card)[1].description.as_deref(), Some("Permite esto ahora y en `alondra` deja de preguntar por:\n\
        · `make mac-snapshots-record` con cualquier argumento: ejecuta lo que diga el `Makefile` del repo, también si el nodo lo cambia\n\
        `tail` y `git status` ya no preguntaban. Puedes quitarlo en Ajustes › Permisos."));
    let view = render(&state, &LiveSnapshot::default(), Client::Mac, Lang::En, "card:i-1", "");
    assert_eq!(answers(card_of(&view))[1].description.as_deref(), Some("Allows this now and stops asking in `alondra` for:\n\
        · `make mac-snapshots-record` with any arguments: runs what the repo's `Makefile` says, even if the node changes it\n\
        `tail` and `git status` already didn't ask. You can remove it in Settings › Permissions."));
    let option = QuestionOption { id: Hold::ALWAYS.into(), label: "Allow always".into(), description: None };
    assert_eq!(answer_label(&state, &state.interactions[&"i-1".into()], &option, Lang::Es),
        "Permitido siempre: `make mac-snapshots-record …` en `alondra`");
}

#[test]
fn a_permission_card_without_allow_always_says_why_in_one_line() {
    use crate::allow_always::{Refusal, Why};
    use crate::domain::{CardKind, Hold};
    let mut state = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow building", ""));
    state.interactions.get_mut(&"i-1".into()).unwrap().card.allow_always = Some(Refusal::new("rm -rf out", Why::Deletes).into());
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!(answers(card).iter().map(|a| a.option_id.as_deref().unwrap()).collect::<Vec<_>>(), ["allow", "deny"],
        "no disabled third option");
    assert!(answers(card).iter().all(|a| a.description.is_none()));
    let text = serde_json::to_string(&view).unwrap();
    assert!(text.contains("No se puede permitir siempre: `rm -rf out` borra o sobrescribe ficheros y siempre pregunta."), "{text}");
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert!(serde_json::to_string(&view).unwrap().contains("Can't allow always: `rm -rf out` deletes or overwrites files and always asks."));
    let mut missing = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow building", ""));
    missing.interactions.get_mut(&"i-1".into()).unwrap().card.allow_always = Some(crate::allow_always::AllowAlways::Offered {
        repo: "app".into(), rules: vec!["make build *".into()], quiet: vec![] });
    let live = LiveSnapshot { allow_always_missing: vec!["i-1".into()], ..LiveSnapshot::default() };
    let view = render(&missing, &live, Client::Ios, Lang::Es, "card:i-1", "");
    assert!(serde_json::to_string(&view).unwrap().contains("Claude Code no guardó estas reglas"));
}

fn held_for_task(offer: crate::allow_for_task::AllowForTask) -> DomainState {
    use crate::domain::{CardKind, Hold};
    let mut state = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow the reset", ""));
    state.tasks.get_mut(&"t-1".into()).unwrap().worktrees.insert("/repo/.claude/worktrees/t-1".into(), crate::domain::TaskWorktree {
        origin: crate::domain::WorktreeOrigin::Task, branch: None, discard: false, end: None });
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = InteractionPayload::Question { options: Hold::Permission.permission_offering(false, offer.offered()), answer: None };
    interaction.card.allow_for_task = Some(offer);
    state
}

#[test]
fn a_permission_card_offers_allow_in_this_task_saying_exactly_what_stays_allowed_and_where() {
    use crate::allow_for_task::AllowForTask;
    use crate::domain::Hold;
    let exact = held_for_task(AllowForTask::Exact { command: "git reset --hard origin/main".into(), cwd: "/repo/.claude/worktrees/t-1".into() });
    let view = render(&exact, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    let card = card_of(&view);
    assert_eq!(answers(card).iter().map(|a| (a.option_id.as_deref().unwrap(), a.label.as_str())).collect::<Vec<_>>(),
        [("allow", "Permitir"), ("task", "Permitir en esta tarea"), ("deny", "Denegar")]);
    assert_eq!(answers(card)[1].description.as_deref(), Some("Solo `git reset --hard origin/main`, lanzado desde el worktree de la tarea, \
        hasta que cierre la tarea. Otro comando, ruta o rama vuelve a preguntar."));
    let option = QuestionOption { id: Hold::FOR_TASK.into(), label: "Allow in this task".into(), description: None };
    assert_eq!(answer_label(&exact, &exact.interactions[&"i-1".into()], &option, Lang::Es), "Permitido en esta tarea: `git reset --hard origin/main`");
    let inside = held_for_task(AllowForTask::Exact { command: "rm -rf out".into(), cwd: "/repo/.claude/worktrees/t-1/crates/core".into() });
    let view = render(&inside, &LiveSnapshot::default(), Client::Mac, Lang::En, "card:i-1", "");
    assert!(answers(card_of(&view))[1].description.as_deref().unwrap().contains("run from `crates/core` in the task's worktree,"));
    let shape = held_for_task(AllowForTask::Shape { command: "bazelisk test //x".into(), cwd: "/repo/.claude/worktrees/t-1".into(), shape: vec!["bazelisk".into(), "test".into()] });
    let view = render(&shape, &LiveSnapshot::default(), Client::Mac, Lang::En, "card:i-1", "");
    assert_eq!(answers(card_of(&view))[1].description.as_deref(), Some("`bazelisk test …` with any arguments, \
        inside the task's worktree and Alondra's folders, until the task ends. Outside them it asks again."));
}

#[test]
fn a_permission_card_without_allow_in_this_task_says_why_in_one_line() {
    use crate::allow_for_task::{AllowForTask, NotForTask};
    let state = held_for_task(AllowForTask::Refused { why: NotForTask::Outside, part: "/Users/me/Desktop/app".into() });
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "card:i-1", "");
    assert_eq!(answers(card_of(&view)).iter().map(|a| a.option_id.as_deref().unwrap()).collect::<Vec<_>>(), ["allow", "deny"]);
    let text = serde_json::to_string(&view).unwrap();
    assert!(text.contains("No se puede permitir en esta tarea: `/Users/me/Desktop/app` está fuera del worktree de la tarea."), "{text}");
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::En, "card:i-1", "");
    assert!(serde_json::to_string(&view).unwrap().contains("Can't allow in this task: `/Users/me/Desktop/app` is outside the task's worktree."));
}

#[test]
fn the_task_screen_lists_what_was_allowed_in_the_task_with_a_revoke_button() {
    use crate::allow_for_task::AllowForTask;
    use crate::domain::TaskGrant;
    let mut state = held_for_task(AllowForTask::Refused { why: crate::allow_for_task::NotForTask::NoFolder, part: String::new() });
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    task.allowed.insert("decision-t-1-1-1".into(), TaskGrant {
        allows: AllowForTask::Exact { command: "git branch -D scratch".into(), cwd: "/repo/.claude/worktrees/t-1".into() }, uses: 3 });
    task.allowed.insert("decision-t-1-1-2".into(), TaskGrant {
        allows: AllowForTask::Shape { command: "cargo test".into(), cwd: "/repo/.claude/worktrees/t-1".into(), shape: vec!["cargo".into(), "test".into()] }, uses: 0 });
    state.revision = state.revision.max(state.tasks[&"t-1".into()].revision);
    let view = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "task:t-1", "");
    let section = view.sections.iter().find(|section| section.id == "allowed_in_task").expect("listed");
    assert_eq!(section.title.as_deref(), Some("Permitido en esta tarea"));
    let rows: Vec<&Row> = section.items.iter().filter_map(|item| match item { Item::Row(row) => Some(row), _ => None }).collect();
    assert_eq!(rows.iter().map(|row| (row.title.as_str(), row.subtitle.as_deref().unwrap())).collect::<Vec<_>>(), [
        ("git branch -D scratch", "comando exacto, desde el worktree de la tarea · usado 3 veces"),
        ("cargo test …", "cualquier argumento, dentro de las carpetas de la tarea · aún sin usar"),
    ]);
    let revoke = &rows[0].actions[0];
    assert_eq!((revoke.label.as_str(), revoke.id.as_str()), ("Revocar", format!("revoke_grant:t-1@{}~decision-t-1-1-1", state.revision).as_str()));
    state.tasks.get_mut(&"t-1".into()).unwrap().allowed.clear();
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "task:t-1", "");
    assert!(view.sections.iter().all(|section| section.id != "allowed_in_task"), "nothing allowed, no section");
}

#[test]
fn a_question_card_never_shows_an_allow_always_a_node_slipped_into_it() {
    use crate::allow_always::AllowAlways;
    use crate::domain::{CardKind, Hold};
    let mut state = held_fixture(Hold::Question, spec(CardKind::Decide, "Which database?", ""));
    let interaction = state.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = InteractionPayload::Question { options: vec![QuestionOption { id: Hold::ALWAYS.into(),
        label: "Always".into(), description: None }], answer: None };
    interaction.card.allow_always = Some(AllowAlways::Offered { repo: "app".into(), rules: vec!["make build *".into()], quiet: vec![] });
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    assert!(!serde_json::to_string(&view).unwrap().contains("stops asking"));
}

#[test]
fn this_macs_settings_list_the_rules_allowed_always_with_a_remove_button() {
    let state = DomainState::new("test-epoch");
    let live = LiveSnapshot { allowed_rules: vec![LiveAllowedRule { id: 7, repo: "alondra".into(), rule: "make build *".into(),
        added: "2026-10-13T10:00:00.000+02:00".into(), task_id: None }], ..LiveSnapshot::default() };
    let view = render(&state, &live, Client::Ios, Lang::Es, "machine", "");
    let section = view.sections.iter().find(|section| section.id == "allowed_rules").expect("listed");
    assert_eq!(section.title.as_deref(), Some("Permisos permitidos siempre"));
    let Item::Row(row) = &section.items[0] else { panic!("row") };
    assert_eq!(row.title, "`make build` con cualquier argumento: ejecuta lo que diga el `Makefile` del repo, también si el nodo lo cambia");
    assert_eq!(row.subtitle.as_deref(), Some("alondra · añadido 13 oct"));
    assert_eq!(row.actions.iter().map(|a| (a.id.as_str(), a.label.as_str())).collect::<Vec<_>>(), [("remove_rule:rule-7@0", "Quitar")]);
    let empty = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "machine", "");
    assert!(empty.sections.iter().all(|section| section.id != "allowed_rules"), "nothing to list, no section");
}

#[test]
fn a_held_ask_user_question_card_shows_the_question_and_its_options() {
    use crate::domain::{CardKind, Hold};
    let mut state = held_fixture(Hold::Question, spec(CardKind::Decide, "Which database?", "The migration depends on it."));
    let options = ["Postgres", "SQLite"].iter().enumerate().map(|(index, label)| QuestionOption {
        id: format!("o{index}"), label: (*label).into(), description: None }).collect();
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

/// The question Claude asked through AskUserQuestion on 19 October, with the words it put under
/// each option.
fn described_question_fixture() -> DomainState {
    use crate::domain::{CardKind, Hold};
    let mut state = held_fixture(Hold::Question, spec(CardKind::Decide, "Which format should the notes use?", ""));
    let options = [("Markdown", "Headings and lists"), ("Plain text", "No markup at all")].iter().enumerate()
        .map(|(index, (label, description))| QuestionOption {
            id: format!("o{index}"), label: (*label).into(), description: Some((*description).into()) }).collect();
    state.interactions.get_mut(&"i-1".into()).unwrap().payload = InteractionPayload::Question { options, answer: None };
    state
}

#[test]
fn a_held_ask_user_question_card_shows_the_words_claude_put_under_each_option() {
    let state = described_question_fixture();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for lang in [Lang::En, Lang::Es] {
            let view = render(&state, &LiveSnapshot::default(), client, lang, "card:i-1", "");
            assert_eq!(answers(card_of(&view)).iter().map(|a| (a.label.as_str(), a.description.as_deref())).collect::<Vec<_>>(),
                [("Markdown", Some("Headings and lists")), ("Plain text", Some("No markup at all"))], "{client:?} {lang:?}");
        }
    }
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
    assert!(buttons.iter().all(|action| action.input == Input::None));
    assert!(card.reply.as_ref().is_some_and(|reply| matches!(&reply.input, Input::Text { placeholder, .. } if placeholder == "Write the Repository")),
        "the value is written in the card's field: {:?}", card.reply);
    let pushed = push_actions(&state, "i-1", Lang::En).unwrap();
    assert_eq!(pushed.iter().map(|a| (a.option.as_deref(), a.label.as_str(), a.text)).collect::<Vec<_>>(),
        [(Some("accept"), "Accept", false), (Some("decline"), "Decline", false), (None, "Reply", true)]);
}

fn presented_pr() -> DomainState {
    use crate::domain::{Actor, Command, DeliveryTarget, MergeMethod};
    let mut state = DomainState::new("test-epoch");
    let agent = Actor::Agent { agent_id: "a".into() };
    let user = Actor::User { user_id: "u".into() };
    for (actor, command) in [
        (Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node A".into(), at: 0 }),
        (user.clone(), Command::CreateRequest { request_id: "r".into(), body: "Fix it".into(), auto_merge: false, maintenance: false, message: false }),
        (Actor::Orchestrator { generation: 0 }.clone(), Command::CreateTask { plan_items: None, task_id: "t".into(), request_id: "r".into(), title: "Fix it".into(),
            agent_id: Some("a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
            launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")), }),
        (agent.clone(), Command::RegisterPr { task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(),
            repository: "owner/repo".into(), number: 1, head: "head1".into(), base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash] }),
        (agent, Command::PresentResult { checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "d".into(),
            body: "Ready".into(), targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }],
            is_final: true, nothing_to_review: false, card: crate::domain::CardSpec::delivery("Review the result", &[], true) }),
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
        (user.clone(), Instruction::Changes, "Making changes", "Corrigiendo"),
        (user.clone(), Instruction::Question, "Answering you", "Respondiéndote"),
        (user.clone(), Instruction::Reply, "Answering you", "Respondiéndote"),
    ] {
        let mut state = presented.clone();
        execute(&mut state, actor, Command::SubmitFeedback {
            attachments: vec![], presentation_id: "d".into(), body: "Back to you".into(), instruction,
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }],
        });
        execute(&mut state, Actor::System, Command::MoveNode { agent_id: "a".into(), event: crate::domain::NodeEvent::TurnStarted, at: 1 });
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
    assert!(english.contains("Couldn't merge") && !english.contains("Resolving conflicts"), "{english}");
    assert!(task(&state, Lang::Es).contains("No se pudo hacer merge"));
    let card = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::En, "card:d", "")).unwrap();
    assert!(card.contains("Alondra couldn't merge #12") && card.contains("its checks failed (lint)")
        && !card.contains("Conflicts were reported"), "{card}");
}

fn pause_for_memory(state: &mut DomainState) {
    let pause = crate::domain::Pause { reason: crate::domain::PauseReason::Memory, since: 2, cut: None, turn_since: Some(1),
        cleanup: Default::default(), released: false };
    execute(state, crate::domain::Actor::System, crate::domain::Command::MoveNode { agent_id: "a".into(), event: crate::domain::NodeEvent::Paused { pause }, at: 2 });
}

/// t-redraft-blurry-mode, 14 October: replies on its results reached the node, but the result
/// stayed in Waiting for You until the node presented again. And the harness-tests result of 12
/// October: its node was paused for memory seconds after the reply and never presented again. A
/// reply takes the delivery away on every client while the node acts on it; the pause brings it back.
#[test]
fn a_reply_on_a_presented_pr_leaves_waiting_for_you_on_every_client_and_a_pause_brings_it_back() {
    use crate::domain::{Actor, Command, DeliveryTarget, Instruction};
    let live = LiveSnapshot::default();
    let waiting = |state: &DomainState, client| match client {
        Client::Ios => render(state, &live, client, Lang::Es, "home", "").sections[0].card_ids == ["d"],
        Client::Mac | Client::Tui => render(state, &live, client, Lang::Es, if client == Client::Mac { "sidebar" } else { "home" }, "")
            .sections.iter().any(|section| section.id == "waiting" && !section.items.is_empty()),
    };
    let mut state = presented_pr();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        assert!(waiting(&state, client), "{client:?}: the presented result waits for the user");
        let presented = render(&state, &live, client, Lang::Es, "card:d", "");
        let presented = card_of(&presented);
        let reply = presented.actions.iter().chain(&presented.more).chain(&presented.reply)
            .find(|action| action.id.starts_with("reply:")).expect("the delivery offers a reply").clone();
        assert!(reply.resolves_card, "{client:?}: the client hides the delivery the moment the reply is sent");
    }
    execute(&mut state, Actor::User { user_id: "u".into() }, Command::SubmitFeedback {
        attachments: vec![], presentation_id: "d".into(), body: "te he pedido cambios pero este card no se va".into(),
        instruction: Instruction::Reply, targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }],
    });
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        assert!(!waiting(&state, client), "{client:?}: the user already answered");
    }
    execute(&mut state, Actor::System, Command::MoveNode { agent_id: "a".into(), event: crate::domain::NodeEvent::TurnStarted, at: 1 });
    assert!(!waiting(&state, Client::Ios), "the node acts on the reply");
    pause_for_memory(&mut state);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        assert!(waiting(&state, client), "{client:?}: the result waits for the user again");
    }
    let card = render(&state, &live, Client::Ios, Lang::Es, "card:d", "");
    assert_eq!(card.view, "card", "never a settled card with nothing to press");
    let merge = card_of(&card).actions.clone().into_iter().find(|action| action.id.starts_with("merge:")).expect("the card offers the merge");
    assert!(merge.enabled, "{merge:?}");
}

#[test]
fn a_task_whose_node_is_paused_says_what_it_owes_the_user_never_no_connection() {
    use crate::domain::{Actor, Command, DeliveryTarget, Instruction};
    let live = LiveSnapshot::default();
    for (instruction, english, spanish) in [
        (Instruction::Changes, "has your changes pending", "tiene pendientes tus cambios"),
        (Instruction::Question, "has your question pending", "tiene pendiente tu pregunta"),
    ] {
        let mut state = presented_pr();
        execute(&mut state, Actor::User { user_id: "u".into() }, Command::SubmitFeedback {
            attachments: vec![], presentation_id: "d".into(), body: "Back to you".into(), instruction,
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }],
        });
        execute(&mut state, Actor::System, Command::MoveNode { agent_id: "a".into(), event: crate::domain::NodeEvent::TurnStarted, at: 1 });
        pause_for_memory(&mut state);
        assert!(render(&state, &live, Client::Ios, Lang::En, "home", "").sections[0].card_ids.is_empty(), "{instruction:?}");
        let task = |lang| serde_json::to_string(&render(&state, &live, Client::Tui, lang, "task:t", "")).unwrap();
        let (en, es) = (task(Lang::En), task(Lang::Es));
        assert!(en.contains("Paused") && en.contains("For memory") && en.contains(english) && !en.contains("No connection"), "{en}");
        assert!(es.contains("Pausada") && es.contains(spanish) && !es.contains("Sin conexión"), "{es}");
    }
}

#[test]
fn a_task_whose_node_works_after_the_merge_says_what_it_does() {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.keys().next().unwrap().clone();
    let held = state.tasks.get_mut(&task).unwrap();
    held.agent_id = Some("a-1".into());
    held.state = TaskState::Closing;
    state.agents.get_mut(&"a-1".into()).unwrap().phase = crate::domain::NodePhase::Closing;
    let detail = |state: &DomainState, lang| serde_json::to_string(&render(state, &LiveSnapshot::default(), Client::Ios, lang, &format!("task:{task}"), "")).unwrap();
    assert!(!detail(&state, Lang::En).contains("finishing after the merge"));
    state.tasks.get_mut(&task).unwrap().wrap_up.post_merge_work = Some(crate::domain::PostMergeWork { reason: "release.sh publishes 0.2.54".into(), declared_at: 1, turn_since: false });
    let english = detail(&state, Lang::En);
    assert!(english.contains("finishing after the merge: release.sh publishes 0.2.54"), "{english}");
    let spanish = detail(&state, Lang::Es);
    assert!(spanish.contains("terminando tras el merge: release.sh publishes 0.2.54"), "{spanish}");
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
        paid_account_grant: None,
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
    assert!(detail.contains("Didn't start"), "{detail}");
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
    assert!(detail.contains("Stopped") && !detail.contains("Didn't start"), "{detail}");
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
    assert!(detail.contains("Stopped") && !detail.contains("Not started") && !detail.contains("Node closed"), "{detail}");
    let (spanish, relaunch) = waiting_for_a_relaunch(&live, Client::Ios, Lang::Es, "task:t-1");
    assert_eq!(relaunch.unwrap().label, "Relanzar");
    assert!(spanish.contains("Detenida") && !spanish.contains("Nodo cerrado"), "{spanish}");
    for (client, lang) in [(Client::Ios, Lang::En), (Client::Mac, Lang::Es), (Client::Tui, Lang::En)] {
        let (list, _) = waiting_for_a_relaunch(&live, client, lang, "tasks");
        assert!(list.contains("\"id\":\"relaunch:t-1@"), "{client:?}: the row carries the action: {list}");
        assert!(list.contains(if lang == Lang::En { "You closed its session before it delivered" } else { "Cerraste su sesión sin que entregara" }),
            "{client:?}: {list}");
    }
}

#[test]
fn a_task_is_not_offered_a_relaunch_unless_it_is_not_started_with_no_node_and_its_node_was_closed() {
    let (detail, relaunch) = waiting_for_a_relaunch(&LiveSnapshot::default(), Client::Tui, Lang::En, "task:t-1");
    assert!(relaunch.is_none() && detail.contains("Its session ended before it delivered"), "a task nobody closed: {detail}");
    let closed = LiveSnapshot { closed_tasks: vec!["t-1".into()], ..LiveSnapshot::default() };
    let mut state = failed_launch_fixture("boom");
    state.schedule.runs.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = Some("a-1".into());
    let detail = serde_json::to_string(&render(&state, &closed, Client::Tui, Lang::En, "task:t-1", "")).unwrap();
    assert!(!detail.contains("relaunch:") && !detail.contains("Stopped"), "a task a new node took: {detail}");
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
        request_id: "a7efd99d".into(), body: "can we encrypt images via R2".into(), auto_merge: false, maintenance: false, message: false,
    });
    let closed = run(&mut state, orchestrator, Command::CloseRequest {
        request_id: "a7efd99d".into(),
        answer: Some("Sí: se cifran en el cliente antes de subirlas a R2".into()),
        withdrawn: false,
        title: Some("Cifrar imágenes vía R2".into()),
        headline: Some("Cifrar imágenes con R2 es posible".into()),
    });
    assert_eq!(closed.effects, vec![DomainEffect::NotifyUser {
        task_id: None, interaction_id: "a7efd99d-answer".into(),
    }]);
    state
}
#[test]
fn an_answer_waits_in_every_inbox_after_its_request_closes() {
    let state = answered_and_closed_request();
    let live = LiveSnapshot::default();
    let ios = render(&state, &live, Client::Ios, Lang::Es, "home", "");
    assert_eq!(ios.sections[0].card_ids, vec!["a7efd99d-answer".to_owned()]);
    assert_eq!(ios.status.badge, 1);
    assert_eq!(inbox_badge(&state), 1);
    let tui = render(&state, &live, Client::Tui, Lang::En, "home", "");
    let waiting = tui.sections.iter().find(|section| section.id == "waiting").unwrap();
    assert!(matches!(waiting.items.as_slice(), [Item::Card(card)] if card.id == "a7efd99d-answer"));
    let sidebar = render(&state, &live, Client::Mac, Lang::En, "sidebar", "");
    assert!(serde_json::to_string(&sidebar).unwrap().contains("a7efd99d-answer"));
    let card = render(&state, &live, Client::Ios, Lang::Es, "card:a7efd99d-answer", "");
    let Some(Item::Card(card)) = card.sections.iter().flat_map(|section| &section.items).next() else {
        panic!("the answer renders as a card");
    };
    assert_eq!(card.title, "Cifrar imágenes con R2 es posible");
    assert_eq!(card.meta.as_deref(), Some("Cifrar imágenes vía R2"), "signed with its request, never with the orchestrator");
    assert!(card.actions.iter().any(|action| action.id.starts_with("read:")));
    assert!(card.reply.as_ref().is_some_and(|reply| reply.id.starts_with("msg:view@")),
        "the answer is replied to the orchestrator");
    let pushed = push_content(&with_cards(&state), "a7efd99d-answer", Lang::Es).unwrap();
    let alert = pushed.alert.unwrap();
    assert_eq!(alert.title, "Cifrar imágenes con R2 es posible");
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
            QuestionOption { id: "o0".into(), label: "First".into(), description: None },
            QuestionOption { id: "o1".into(), label: "Second".into(), description: None },
        ];
        let answer = answered.contains(&at).then(|| crate::domain::Answer {
            attachments: vec![], option_id: "o1".into(), inputs: BTreeMap::new(), responder: "u".into(), at_revision: 1, decision: None,
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

/// Nodes where one needing the user, one stopped behind a dependency with no live entry, a
/// working one and a starting one belong to projects, and two nodes whose sessions closed are gone.
fn nodes_fixture() -> (DomainState, LiveSnapshot) {
    let mut state = golden_fixture();
    let mut live = golden_live();
    let golden_task = state.tasks.get_mut(&"t-651c38484cd2f34e74d5107ba05f7f15".into()).unwrap();
    golden_task.agent_id = Some("d887a34b-249e-4487-a89b-57de38c26372".into());
    let template = golden_task.clone();
    let golden_request = template.request_id.0.clone();
    for (agent, request, title, task_state, phase) in [
        ("queued", golden_request.as_str(), "Wire the badge after the review", TaskState::WaitingDependency, NodePhase::WaitingDependency),
        ("zeta-worker", "r-zeta", "Build the importer", TaskState::Running, NodePhase::Working),
        ("alpha-waiting", "r-alpha", "Plan the migration", TaskState::NotStarted, NodePhase::Starting { reviving: false, idle_since: None }),
        ("finished-yesterday", golden_request.as_str(), "Ship the compact editor", TaskState::Done, NodePhase::Closed),
        ("finished-today", golden_request.as_str(), "Trim the push payload", TaskState::Done, NodePhase::Closed),
    ] {
        state.agents.insert(agent.into(), Agent { id: agent.into(), name: agent.into(), phase, since: 0 });
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

/// The nodes fixture with one node waiting for its own command and one the machine paused.
fn node_states_fixture() -> (DomainState, LiveSnapshot) {
    let (mut state, mut live) = nodes_fixture();
    let template = state.tasks[&"t-zeta-worker".into()].clone();
    let declared = Pause { reason: PauseReason::Declared { reason: "The build queue is full".into() }, since: 0, cut: None,
        turn_since: None, cleanup: PauseCleanup::Done, released: false };
    let release = Background { work: crate::model::BackgroundKind::Shell, label: "./deploy/brew/release.sh 0.2.67".into(), since: 0, others: 0, gone: None };
    for (agent, title, phase) in [
        ("release-runner", "Release the daemon", NodePhase::WaitingBackground(release)),
        ("paused-builder", "Fix the vanished session", NodePhase::Paused(declared)),
    ] {
        state.agents.insert(agent.into(), Agent { id: agent.into(), name: agent.into(), phase, since: 0 });
        let mut task = template.clone();
        (task.id, task.agent_id, task.title) = (format!("t-{agent}").into(), Some(agent.into()), title.into());
        state.tasks.insert(task.id.clone(), task);
        live.nodes.push(LiveNode { agent_id: agent.into(), name: agent.into(), project: Some("Zeta".into()), ..LiveNode::default() });
    }
    live.pacing = Some(LivePacing { working: 2, limit: 6, fixed: None, auto: 6, most: 12, memory_free: Some(40), tight: false, queued: vec![],
        paused: vec![LivePaused { task_id: "t-paused-builder".into(), session: Some("paused-builder".into()), at: "09:40".into(),
            reason: LivePauseReason::Machine, fine_minutes: None, next: false, waits_for: Some("The build queue is full".into()), retry: None }] });
    (state, live)
}

const NODE_STATE_GOLDENS: [(&str, &str, Client, Lang); 4] = [
    ("node-states-tasks-ios.json", "tasks", Client::Ios, Lang::En),
    ("node-states-tasks-ios-es.json", "tasks", Client::Ios, Lang::Es),
    ("node-states-tasks-mac.json", "tasks", Client::Mac, Lang::En),
    ("node-states-tasks-tui.json", "tasks", Client::Tui, Lang::Es),
];

/// Tasks in every client for a node waiting for its own command and one the machine
/// paused: the files the TUI and the apps' tests draw.
#[test]
fn node_state_views_match_goldens() {
    let (state, live) = node_states_fixture();
    for (filename, key, client, lang) in NODE_STATE_GOLDENS {
        let view = render(&state, &live, client, lang, key, "2026-09-30T10:00:00Z");
        let expected = match filename {
            "node-states-tasks-ios.json" => include_str!("../../../../fixtures/views/cards/node-states-tasks-ios.json"),
            "node-states-tasks-ios-es.json" => include_str!("../../../../fixtures/views/cards/node-states-tasks-ios-es.json"),
            "node-states-tasks-mac.json" => include_str!("../../../../fixtures/views/cards/node-states-tasks-mac.json"),
            _ => include_str!("../../../../fixtures/views/cards/node-states-tasks-tui.json"),
        };
        assert_golden(filename, &view, expected);
    }
}

/// 9 October: what is in progress is the tasks Alondra's real nodes work on. The task of a node
/// whose task ended, of one whose session is lost and waits for the gate, of the orchestrator and
/// of the queue stay out of it; that of one the gate let back and Alondra is restarting is in it.
#[test]
fn the_sidebar_lists_only_tasks_whose_node_has_a_session() {
    let (mut state, mut live) = nodes_fixture();
    let template = state.tasks[&"t-zeta-worker".into()].clone();
    for (agent, task_state, phase) in [
        ("reviving", TaskState::Running, NodePhase::Starting { reviving: true, idle_since: None }),
        ("lost", TaskState::Running, NodePhase::Lost),
        ("lingering", TaskState::Done, NodePhase::Working),
        ("cancelled-paused", TaskState::Cancelled, NodePhase::Paused(crate::domain::Pause { reason: crate::domain::PauseReason::ByUser, since: 0, cut: None, turn_since: None, cleanup: Default::default(), released: false })),
        ("taskless", TaskState::Running, NodePhase::Working),
    ] {
        state.agents.insert(agent.into(), Agent { id: agent.into(), name: agent.into(), phase, since: 0 });
        if agent == "taskless" { continue; }
        let mut task = template.clone();
        (task.id, task.agent_id, task.state) = (format!("t-{agent}").into(), Some(agent.into()), task_state);
        state.tasks.insert(task.id.clone(), task);
    }
    state.agents.insert("orchestrator".into(), Agent { id: "orchestrator".into(), name: "orchestrator".into(), phase: NodePhase::Working, since: 0 });
    live.pacing = Some(LivePacing {
        working: 1, limit: 2, fixed: None, auto: 2, most: 12, memory_free: Some(40), tight: false,
        queued: vec![LiveQueued { pausable: true, task_id: "t-alpha-waiting".into(), position: 1, of: 1, hold: LiveHold::Slots { working: 1, limit: 2 } }],
        paused: vec![],
    });
    for lang in [Lang::En, Lang::Es] {
        let view = render(&state, &live, Client::Mac, lang, "sidebar", "");
        let rows: Vec<&str> = view.sections.iter().filter(|section| section.id.starts_with("project:")).flat_map(|section| &section.items)
            .filter_map(|item| if let Item::Row(row) = item { Some(row.id.as_str()) } else { None }).collect();
        assert_eq!(rows.iter().copied().collect::<BTreeSet<_>>(),
            ["t-651c38484cd2f34e74d5107ba05f7f15", "t-queued", "t-zeta-worker", "t-alpha-waiting", "t-reviving"].into_iter().collect::<BTreeSet<_>>());
    }
}

fn now_title(state: &DomainState, lang: Lang) -> Option<String> {
    let view = render(state, &LiveSnapshot::default(), Client::Ios, lang, "task:t-1", "2026-09-30T10:00:00Z");
    let section = view.sections.iter().find(|section| section.id == "now")?;
    let Item::Row(row) = &section.items[0] else { panic!("now row") };
    Some(row.title.clone())
}

#[test]
fn a_cancelling_task_says_what_alondra_still_removes() {
    let mut state = fixture();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    task.state = TaskState::Closing;
    task.ending = Some(crate::domain::TaskEnding::Cancelling);
    task.agent_id = Some("a-1".into());
    assert_eq!(now_title(&state, Lang::En).unwrap(), "cancelled; Alondra closes its node");
    assert_eq!(now_title(&state, Lang::Es).unwrap(), "cancelada; Alondra cierra su nodo");
    state.tasks.get_mut(&"t-1".into()).unwrap().agent_id = None;
    assert_eq!(now_title(&state, Lang::Es).unwrap(), "cancelada; Alondra retira lo que dejó");
}

#[test]
fn a_closing_task_says_whether_it_waits_for_its_merge_or_its_nodes_cleanup() {
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Closing;
    assert_eq!(now_title(&state, Lang::En).unwrap(), "accepted; Alondra merges its pull request");
    assert_eq!(now_title(&state, Lang::Es).unwrap(), "aceptada; Alondra hace merge de su PR");
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

fn opt(id: &str) -> QuestionOption {
    QuestionOption { id: id.into(), label: format!("Option {id}"), description: None }
}

fn answer_to(option_id: &str) -> crate::domain::Answer {
    crate::domain::Answer { attachments: vec![], option_id: option_id.into(), inputs: Default::default(), responder: "device:1".into(), at_revision: 1, decision: None }
}

/// What a card key renders: the card, or only why a settled one went.
fn card_or_notice(view: &View) -> Result<&Card, &str> {
    if view.view == super::SETTLED_VIEW {
        assert!(view.sections.is_empty(), "a settled card renders sections: {:?}", view.sections);
        return Err(view.empty.as_ref().map(|notice| notice.text.as_str()).expect("a settled card says why"));
    }
    match view.sections.first().and_then(|section| section.items.first()) {
        Some(Item::Card(card)) => Ok(card),
        other => panic!("a card view without its card: {other:?}"),
    }
}

#[test]
fn every_open_card_with_options_or_input_can_be_answered_and_a_settled_one_is_never_shown_in_any_client() {
    use crate::domain::{CardKind, Hold};
    let option_sets = [vec![], vec![opt("retry"), opt("later")], vec![opt("a"), opt("b"), opt("c")]];
    let kinds = [CardKind::Decide, CardKind::Problem, CardKind::Propose, CardKind::Do, CardKind::Provide, CardKind::Test];
    let holds = [None, Some(Hold::Question), Some(Hold::Permission), Some(Hold::Elicitation)];
    let task_states = [TaskState::NotStarted, TaskState::Running, TaskState::NeedsYou, TaskState::WaitingDependency, TaskState::Done];
    let (mut open, mut settled) = (0, 0);
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
                                let pending = inbox_items(&with_cards(&state)).iter()
                                    .find(|item| item.id == "interaction:i-1").expect("the card is in the inbox").pending;
                                let why = match settle {
                                    "answered" => "Ya respondida",
                                    "withdrawn" => "Ya no hace falta",
                                    _ => "Su tarea ya terminó",
                                };
                                for client in [Client::Ios, Client::Mac, Client::Tui] {
                                    let view = render(&state, &LiveSnapshot::default(), client, Lang::Es, "card:i-1", "");
                                    let case = format!("{kind:?} {hold:?} {task_state:?} orchestrator={orchestrator} blocking={blocking} {settle} options={} {client:?}", options.len());
                                    match card_or_notice(&view) {
                                        Ok(card) => {
                                            assert!(pending, "a settled card is shown: {case}");
                                            let answers = card.actions.iter().chain(&card.more).any(|action| action.id.starts_with("ans:"))
                                                || card.reply.as_ref().is_some_and(|reply| reply.id.starts_with("reply:"));
                                            assert!(answers, "an open card has no answer: {case}");
                                            open += 1;
                                        }
                                        Err(notice) => {
                                            assert!(!pending, "an open card is hidden: {case}");
                                            assert_eq!(notice, why, "{case}");
                                            settled += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(open > 500 && settled > 500, "{open} open, {settled} settled");
}

#[test]
fn an_open_text_delivery_can_be_accepted_or_sent_back_and_an_accepted_one_is_never_shown_in_any_client() {
    use crate::domain::{Acceptance, DeliveryTarget};
    let mut open = 0;
    let cases = [(TaskState::ForYourReview, false), (TaskState::NeedsYou, false), (TaskState::Running, false), (TaskState::Closing, true), (TaskState::Done, true)];
    for (task_state, accepted) in cases {
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
        let pending = inbox_items(&with_cards(&state)).iter()
            .find(|item| item.id == "interaction:i-3").expect("the delivery is in the inbox").pending;
        open += usize::from(pending);
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            let view = render(&state, &LiveSnapshot::default(), client, Lang::Es, "card:i-3", "");
            let case = format!("{task_state:?} accepted={accepted} {client:?}");
            match card_or_notice(&view) {
                Ok(card) => {
                    assert!(!accepted, "an accepted delivery is shown: {case}");
                    let ids: Vec<&str> = card.actions.iter().chain(&card.more).map(|action| action.id.as_str()).collect();
                    if pending {
                        assert!(ids.iter().any(|id| id.starts_with("accept:")), "an open delivery has no accept: {case} {ids:?}");
                        assert!(ids.iter().any(|id| id.starts_with("changes:")), "an open delivery cannot be sent back: {case} {ids:?}");
                    }
                }
                Err(notice) => {
                    assert!(accepted, "an open delivery is hidden: {case}");
                    assert_eq!(notice, "Ya aceptada", "{case}");
                }
            }
        }
    }
    assert!(open > 0);
}

#[test]
fn a_settled_card_says_only_why_it_went_in_english_and_spanish() {
    use crate::domain::{CardKind, Feedback, Instruction};
    let notice = |state: &DomainState, key: &str, lang| {
        card_or_notice(&render(state, &LiveSnapshot::default(), Client::Ios, lang, key, "")).err().map(str::to_owned)
    };
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    let question = state.interactions.get_mut(&"i-1".into()).unwrap();
    question.card = spec(CardKind::Decide, "Decide si repito el grabado de snapshots", "Sin ellos no puedo abrir la PR.");
    question.payload = InteractionPayload::Question { options: vec![opt("retry"), opt("later")], answer: None };
    assert_eq!(notice(&state, "card:i-1", Lang::Es), None);

    let question = state.interactions.get_mut(&"i-1".into()).unwrap();
    question.payload = InteractionPayload::Question { options: vec![opt("retry"), opt("later")], answer: Some(answer_to("retry")) };
    assert_eq!(notice(&state, "card:i-1", Lang::Es).as_deref(), Some("Ya respondida"));
    assert_eq!(notice(&state, "card:i-1", Lang::En).as_deref(), Some("Already answered"));

    state.interactions.get_mut(&"i-1".into()).unwrap().withdrawn = true;
    assert_eq!(notice(&state, "card:i-1", Lang::Es).as_deref(), Some("Ya no hace falta"));
    assert_eq!(notice(&state, "card:i-1", Lang::En).as_deref(), Some("No longer needed"));

    let information = state.interactions.get_mut(&"i-1".into()).unwrap();
    information.withdrawn = false;
    information.card = spec(CardKind::Know, "El tag ya está publicado", "Por si lo buscas.");
    information.payload = InteractionPayload::Information { read_by: ["device:1".to_owned()].into() };
    assert_eq!(notice(&state, "card:i-1", Lang::Es).as_deref(), Some("Ya leída"));
    assert_eq!(notice(&state, "card:i-1", Lang::En).as_deref(), Some("Already read"));

    let question = state.interactions.get_mut(&"i-1".into()).unwrap();
    question.card = spec(CardKind::Decide, "Decide si repito el grabado de snapshots", "Sin ellos no puedo abrir la PR.");
    question.payload = InteractionPayload::Question { options: vec![opt("retry")], answer: None };
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    assert_eq!(notice(&state, "card:i-1", Lang::Es).as_deref(), Some("Su tarea ya terminó"));
    assert_eq!(notice(&state, "card:i-1", Lang::En).as_deref(), Some("Its task is over"));

    let mut state = fixture();
    state.interactions.retain(|id, _| id.0 == "i-3");
    state.revision = 10;
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    let delivery = state.interactions.get_mut(&"i-3".into()).unwrap();
    delivery.payload = InteractionPayload::Delivery {
        targets: vec![crate::domain::DeliveryTarget::Text { text: "No hay reservas acumuladas.".into() }],
        is_final: true,
        acceptance: None,
        feedback: vec![Feedback { attachments: vec![], user_id: "device:1".into(), body: "Falta el caso vacío".into(), at_revision: 9, instruction: Instruction::Changes }],
    };
    assert_eq!(notice(&state, "card:i-3", Lang::Es).as_deref(), Some("Ya respondida"));

    let InteractionPayload::Delivery { feedback, .. } = &mut state.interactions.get_mut(&"i-3".into()).unwrap().payload else { unreachable!() };
    feedback[0].instruction = Instruction::Conflicts;
    assert_eq!(notice(&state, "card:i-3", Lang::Es), None, "a delivery Alondra sent back itself still says why");
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
                let domain = with_cards(state);
                let index = crate::domain::ReadIndex::build(&domain);
                let card = build(&format!("card:{id}"), RenderContext {
                    domain: &domain, index: &index, live: &live, client: Client::Ios, lang,
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

const ALLOWED_TASK: &str = "t-651c38484cd2f34e74d5107ba05f7f15";
const ALLOWED_WORKTREE: &str = "/demo/Demo Garden/.claude/worktrees/t-651c38484cd2f34e74d5107ba05f7f15";
const ALLOWED_CASES: [(&str, &str, Client, Lang); 6] = [
    ("card-permission-task-mac.json", "card:cc2d034b-249e-4487-a89b-57de38c26372", Client::Mac, Lang::En),
    ("card-permission-task-es-mac.json", "card:cc2d034b-249e-4487-a89b-57de38c26372", Client::Mac, Lang::Es),
    ("task-allowed-mac.json", "task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Mac, Lang::En),
    ("task-allowed-es-mac.json", "task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Mac, Lang::Es),
    ("task-allowed-ios.json", "task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Ios, Lang::En),
    ("task-allowed-ios-es.json", "task:t-651c38484cd2f34e74d5107ba05f7f15", Client::Ios, Lang::Es),
];

/// A node of the demo task asks to reset its worktree, offered «allow in this task», and the task
/// already allows a branch deletion and its tests.
fn allowed_state() -> DomainState {
    use crate::allow_for_task::AllowForTask;
    use crate::domain::{CardKind, ContentKind as Content, Hold, TaskGrant, TaskWorktree, WorktreeOrigin};
    let mut state = golden_fixture();
    let task = state.tasks.get_mut(&ALLOWED_TASK.into()).unwrap();
    task.worktrees.insert(ALLOWED_WORKTREE.into(), TaskWorktree { origin: WorktreeOrigin::Task, branch: None, discard: false, end: None });
    task.allowed.insert("decision-t-651c-1-3".into(), TaskGrant {
        allows: AllowForTask::Exact { command: "git branch -D scratch/empty-state".into(), cwd: ALLOWED_WORKTREE.into() }, uses: 2 });
    task.allowed.insert("decision-t-651c-1-4".into(), TaskGrant {
        allows: AllowForTask::Shape { command: "bazelisk test //apple:AlondraiOSTests".into(), cwd: ALLOWED_WORKTREE.into(),
            shape: vec!["bazelisk".into(), "test".into()] }, uses: 5 });
    let reset = AllowForTask::Exact { command: "git reset --hard origin/main".into(), cwd: ALLOWED_WORKTREE.into() };
    let interaction = state.interactions.get_mut(&KNOW_COMMAND_CARD.into()).unwrap();
    interaction.card = spec(CardKind::Authorize, "Run `git reset --hard origin/main`", "").content(Content::Command, "git reset --hard origin/main");
    interaction.card.allow_for_task = Some(reset);
    interaction.body = "Run `git reset --hard origin/main`".into();
    interaction.blocking = true;
    interaction.payload = InteractionPayload::Question { options: Hold::Permission.permission_offering(false, true), answer: None };
    state.holds.insert(KNOW_COMMAND_CARD.into(), Hold::Permission);
    state.revision = state.revision.max(state.tasks[&ALLOWED_TASK.into()].revision);
    state
}

#[test]
fn allowed_in_task_screens_match_goldens() {
    let state = allowed_state();
    let differ: Vec<&str> = ALLOWED_CASES.into_iter().zip([
        include_str!("../../../../fixtures/views/cards/card-permission-task-mac.json"),
        include_str!("../../../../fixtures/views/cards/card-permission-task-es-mac.json"),
        include_str!("../../../../fixtures/views/cards/task-allowed-mac.json"),
        include_str!("../../../../fixtures/views/cards/task-allowed-es-mac.json"),
        include_str!("../../../../fixtures/views/cards/task-allowed-ios.json"),
        include_str!("../../../../fixtures/views/cards/task-allowed-ios-es.json"),
    ]).filter_map(|((filename, key, client, lang), expected)| {
        let actual = render(&state, &golden_live(), client, lang, key, "2026-09-30T10:00:00Z");
        std::panic::catch_unwind(|| assert_golden(filename, &actual, expected)).is_err().then_some(filename)
    }).collect();
    assert!(differ.is_empty(), "{differ:?} differ from their goldens");
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

#[test]
fn a_queued_task_says_its_place_and_why_in_its_task() {
    let mut state = fixture();
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation) = (TaskState::NotStarted, None);
    let live = LiveSnapshot {
        pacing: Some(LivePacing {
            working: 6, limit: 6, fixed: None, auto: 6, most: 12, memory_free: Some(12), tight: true,
            queued: vec![LiveQueued { pausable: true, task_id: "t-1".into(), position: 2, of: 3, hold: LiveHold::Slots { working: 6, limit: 6 } }],
            paused: vec![],
        }),
        ..LiveSnapshot::default()
    };
    let task = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::En, "task:t-1", "")).unwrap();
    assert!(task.contains("Queued · 2nd of 3 · 6 of 6 working"), "{task}");
    let memory = LiveQueued { pausable: true, task_id: "t-1".into(), position: 1, of: 1, hold: LiveHold::Memory { free: Some(12) } };
    assert_eq!(common::queued_label(&memory, Lang::Es), "En cola · 1.º de 1 · el Mac va justo de memoria (12 % libre)");
    let load = LiveQueued { hold: LiveHold::Cpu { load: 52, cpus: 11 }, ..memory };
    assert_eq!(common::queued_label(&load, Lang::En), "Queued · 1st of 1 · the Mac is very busy (load 52 on 11 cores)");
}

/// 25 October: the disk filled with builds. A task the gate holds for room on the disk reads
/// «Waiting for capacity» with the free space and what one more build needs, on every client, in
/// its row and on its screen, whether it never started or its node waits to come back.
#[test]
fn a_task_held_for_the_disk_waits_for_capacity_with_the_free_space_and_what_is_needed() {
    let held = LiveQueued { pausable: true, task_id: "t-1".into(), position: 1, of: 2, hold: LiveHold::Disk { free: 3_500_000_000, needed: 13_000_000_000 } };
    let mut unstarted = fixture();
    let task = unstarted.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation) = (TaskState::NotStarted, None);
    let live = pacing_live(vec![held.clone()], vec![], None);
    for (state, in_state) in [(&unstarted, None), (&sdk_task(), Some(alondra_domain::LiveState::WaitingCapacity(alondra_domain::Capacity::Machine)))] {
        for (lang, said, fact) in [(Lang::En, "Waiting for capacity", "Disk · 3.5 GB free of 13.0 GB needed"),
            (Lang::Es, "Esperando capacidad", "Disco · 3,5 GB libres de 13,0 GB necesarios")] {
            for client in [Client::Ios, Client::Mac, Client::Tui] {
                let list = render_in(state, &live, client, lang, "tasks", "", in_state.as_ref());
                let row = rows_with_parents(&list).into_iter().map(|(_, row)| row).find(|row| row.id == "t-1").unwrap_or_else(|| panic!("{client:?} lists t-1"));
                let [badge] = row.badges.as_slice() else { panic!("{client:?}: one badge: {:?}", row.badges) };
                assert_eq!(badge.text, said, "{client:?} {lang:?}");
                let screen = serde_json::to_string(&render_in(state, &live, client, lang, "task:t-1", "", in_state.as_ref())).unwrap();
                assert!(screen.contains(said) && screen.contains(fact), "{client:?} {lang:?}: {screen}");
            }
        }
    }
    assert_eq!(common::queued_label(&held, Lang::Es), "Esperando capacidad · Disco · 3,5 GB libres de 13,0 GB necesarios");
    let earlier: LiveHold = serde_json::from_str(r#"{"kind":"disk"}"#).unwrap();
    assert_eq!(earlier, LiveHold::Disk { free: 0, needed: 0 }, "a view of an earlier daemon still reads");
}

#[test]
fn the_terminal_shows_only_the_new_plan_after_the_first_was_sent_back() {
    use crate::domain::{Actor, CardKind, CardSpec, Command, ContentKind, Hold, InputValue};
    let mut state = DomainState::new("test-epoch");
    let user = Actor::User { user_id: "u".into() };
    let plan = |id: &str, title: &str| Command::HoldHook {
        interaction_id: id.into(), task_id: "t".into(), agent_id: "a".into(), hold: Hold::Plan,
        body: format!("# {title}\n\n- [ ] Do it"), options: Hold::Plan.options(),
        card: CardSpec::new(CardKind::Review, title).content(ContentKind::Plan, format!("# {title}\n\n- [ ] Do it")),
    };
    execute(&mut state, Actor::System, Command::RegisterAgent { agent_id: "a".into(), name: "Node A".into(), at: 0 });
    execute(&mut state, user.clone(), Command::CreateRequest { request_id: "r".into(), body: "Weekly cycle".into(), auto_merge: false, maintenance: false, message: false });
    execute(&mut state, Actor::Orchestrator { generation: 0 }.clone(), Command::CreateTask { plan_items: None, task_id: "t".into(), request_id: "r".into(), title: "Weekly cycle".into(),
        agent_id: Some("a".into()), review_prs: false, dependencies: BTreeSet::new(), stack: None,
        launch: Some(crate::domain::LaunchPlan::new("node", "/tmp")), });
    execute(&mut state, Actor::System, plan("p1", "First weekly plan"));
    execute(&mut state, user, Command::AnswerQuestion { attachments: vec![], interaction_id: "p1".into(), option_id: Hold::REQUEST_CHANGES.into(),
        inputs: [("text".to_owned(), InputValue::Text("Drop the plates idea".into()))].into() });
    execute(&mut state, Actor::System, plan("p2", "Second weekly plan"));
    let live = LiveSnapshot::default();
    for key in ["home", "task:t", "tasks"] {
        let view = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::En, key, "")).unwrap();
        assert!(!view.contains("First weekly plan"), "{key} still shows the plan sent back: {view}");
    }
    let home = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::En, "home", "")).unwrap();
    assert!(home.contains("Second weekly plan"), "{home}");
}

#[test]
fn a_paused_task_says_since_when_why_and_when_it_comes_back_in_both_languages() {
    let mut state = fixture();
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation, task.agent_id) = (TaskState::Running, None, Some("agent-1".into()));
    let memory = LivePaused { task_id: "t-1".into(), session: Some("agent-1".into()), at: "14:05".into(), reason: LivePauseReason::Memory, fine_minutes: Some(2), next: false, waits_for: None, retry: None };
    let live = LiveSnapshot {
        pacing: Some(LivePacing { working: 5, limit: 6, fixed: None, auto: 6, most: 12, memory_free: Some(24), tight: true, queued: vec![], paused: vec![memory.clone()] }),
        ..LiveSnapshot::default()
    };
    let task = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::En, "task:t-1", "")).unwrap();
    assert!(task.contains("Paused · For memory since 14:05 · resumes by itself"), "{task}");
    let user = LivePaused { reason: LivePauseReason::User, at: "11:20".into(), ..memory.clone() };
    assert_eq!(common::paused_label(&user, Lang::En), "Paused · you paused it at 11:20");
    assert_eq!(common::paused_label(&user, Lang::Es), "Pausado · lo pausaste a las 11:20");
    let declared = LivePaused { reason: LivePauseReason::Machine, waits_for: Some("cola de compilación".into()), fine_minutes: Some(30), ..memory.clone() };
    assert_eq!(common::paused_label(&declared, Lang::Es), "Esperando: cola de compilación · desde las 14:05", "the node's reason, not the Mac's health");
    assert_eq!(common::paused_label(&LivePaused { next: true, ..declared }, Lang::Es), "Esperando: cola de compilación · desde las 14:05 · se reanuda el siguiente");
    let unexplained = LivePaused { reason: LivePauseReason::Machine, ..memory };
    assert_eq!(common::paused_label(&unexplained, Lang::En), "Waiting since 14:05");
}

#[test]
fn a_node_an_api_error_stopped_says_when_alondra_sends_it_back_and_an_outage_says_it_waits_for_the_service() {
    let mut state = fixture();
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation, task.agent_id) = (TaskState::Running, None, Some("agent-1".into()));
    let failed = LivePaused { task_id: "t-1".into(), session: Some("agent-1".into()), at: "23:26".into(), reason: LivePauseReason::ApiError,
        fine_minutes: None, next: false, waits_for: None, retry: Some(crate::views::LiveRetry { at: "23:28".into(), attempt: 2 }) };
    assert_eq!(common::paused_label(&failed, Lang::Es), "Error de la API de Claude a las 23:26 · reintenta a las 23:28 (intento 2 de 3)");
    assert_eq!(common::paused_label(&failed, Lang::En), "Claude API error at 23:26 · retries at 23:28 (attempt 2 of 3)");
    let outage = LivePaused { reason: LivePauseReason::ApiOutage, retry: Some(crate::views::LiveRetry { at: "23:40".into(), attempt: 3 }), ..failed.clone() };
    assert_eq!(common::paused_label(&outage, Lang::Es), "Esperando al servicio de Claude desde las 23:26 · vuelve a probar a las 23:40");
    let live = LiveSnapshot {
        pacing: Some(LivePacing { working: 1, limit: 4, fixed: None, auto: 4, most: 12, memory_free: Some(60), tight: false, queued: vec![], paused: vec![failed] }),
        ..LiveSnapshot::default()
    };
    let task = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::Es, "task:t-1", "")).unwrap();
    assert!(task.contains("Error de la API de Claude a las 23:26 · reintenta a las 23:28 (intento 2 de 3)"), "{task}");
}

fn action_ids(view: &View) -> Vec<String> {
    let json = serde_json::to_value(view).unwrap();
    let mut ids = vec![];
    fn walk(value: &serde_json::Value, ids: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let (Some(serde_json::Value::String(id)), Some(_)) = (map.get("id"), map.get("presentation")) {
                    ids.push(id.split('@').next().unwrap().to_owned());
                }
                map.values().for_each(|value| walk(value, ids));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|value| walk(value, ids)),
            _ => {}
        }
    }
    walk(&json, &mut ids);
    ids
}

fn find_action<'a>(view: &'a View, prefix: &str) -> Option<&'a super::Action> {
    fn in_rows<'a>(rows: &'a [super::Row], prefix: &str) -> Option<&'a super::Action> {
        rows.iter().find_map(|row| row.actions.iter().find(|action| action.id.starts_with(prefix)).or_else(|| in_rows(&row.children, prefix)))
    }
    view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
        Item::Row(row) => in_rows(std::slice::from_ref(row), prefix),
        Item::Actions(bar) => bar.actions.iter().find(|action| action.id.starts_with(prefix)),
        _ => None,
    })
}

fn pacing_live(queued: Vec<LiveQueued>, paused: Vec<LivePaused>, fixed: Option<u32>) -> LiveSnapshot {
    LiveSnapshot {
        pacing: Some(LivePacing { working: 6, limit: fixed.unwrap_or(6), fixed, auto: 6, most: 12, memory_free: Some(30), tight: false, queued, paused }),
        ..LiveSnapshot::default()
    }
}

#[test]
fn the_limit_shows_and_changes_in_this_macs_settings_never_in_the_tasks() {
    let state = fixture();
    let live = pacing_live(vec![], vec![], None);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let tasks = render(&state, &live, client, Lang::Es, "tasks", "");
        let text = serde_json::to_string(&tasks).unwrap();
        assert!(!text.contains("Límite"), "{client:?}: {text}");
        assert!(find_action(&tasks, "set_limit:").is_none(), "{client:?}");
    }

    let machine = render(&state, &live, Client::Ios, Lang::Es, "machine", "");
    let settings = machine.sections.iter().find(|section| section.id == "settings").expect("this Mac's settings");
    let row = settings.items.iter().find_map(|item| match item {
        Item::Row(row) if row.id == "node_limit" => Some(row),
        _ => None,
    }).unwrap_or_else(|| panic!("{settings:?}"));
    assert_eq!((row.title.as_str(), row.subtitle.as_deref()), ("Nodos a la vez", Some("Límite: automático (6)")));
    let limit = find_action(&machine, "set_limit:limit").expect("the limit is changed from this Mac's settings");
    let labels: Vec<&str> = limit.options.iter().map(|option| option.label.as_str()).collect();
    assert_eq!(labels.len(), 13, "automatic and 1 to 12: {labels:?}");
    assert_eq!(labels[0], "Automático (6) · ahora");
    assert_eq!(limit.options[12].id, "12");

    let live = pacing_live(vec![], vec![], Some(4));
    let machine = render(&state, &live, Client::Mac, Lang::En, "machine", "");
    assert!(serde_json::to_string(&machine).unwrap().contains("Limit: 4, set by you · automatic would be 6"));
    let limit = find_action(&machine, "set_limit:limit").unwrap();
    assert_eq!(limit.options.len(), 13);
    assert_eq!(limit.options[4].label, "4 · now");
}

#[test]
fn a_queued_task_offers_pause_and_a_paused_one_offers_resume_in_its_task() {
    let mut state = fixture();
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation) = (TaskState::NotStarted, None);
    let queued = LiveQueued { pausable: true, task_id: "t-1".into(), position: 1, of: 1, hold: LiveHold::Slots { working: 6, limit: 6 } };
    let live = pacing_live(vec![queued], vec![], None);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let detail = render(&state, &live, client, Lang::En, "task:t-1", "");
        assert!(action_ids(&detail).contains(&"pause:t-1".to_owned()), "{client:?}: {:?}", action_ids(&detail));
        assert!(!action_ids(&detail).iter().any(|id| id.starts_with("resume:")));
    }

    let paused = LivePaused { task_id: "t-1".into(), session: None, at: "11:20".into(), reason: LivePauseReason::User, fine_minutes: None, next: false, waits_for: None, retry: None };
    let live = pacing_live(vec![], vec![paused], None);
    let task = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    let resume = find_action(&task, "resume:t-1").expect("a task taken out of the queue resumes by its id");
    assert_eq!((resume.label.as_str(), resume.description.as_deref()), ("Reanudar", Some("Ahora, aunque se pase del límite")));
    let detail = render(&state, &live, Client::Mac, Lang::En, "task:t-1", "");
    assert!(action_ids(&detail).contains(&"resume:t-1".to_owned()), "{:?}", action_ids(&detail));
    assert!(!action_ids(&detail).iter().any(|id| id.starts_with("pause:")));
}

#[test]
fn a_queued_scheduled_run_or_returning_node_the_user_cannot_take_out_of_the_queue_offers_no_pause() {
    let mut state = fixture();
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation) = (TaskState::NotStarted, None);
    let queued = LiveQueued { pausable: false, task_id: "t-1".into(), position: 1, of: 1, hold: LiveHold::Slots { working: 6, limit: 6 } };
    let live = pacing_live(vec![queued], vec![], None);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let detail = render(&state, &live, client, Lang::En, "task:t-1", "");
        assert!(!action_ids(&detail).iter().any(|id| id.starts_with("pause:")), "{client:?}: {:?}", action_ids(&detail));
    }
}

#[test]
fn a_working_node_offers_pause_with_a_warning_and_a_paused_node_offers_resume_by_its_session() {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation, task.agent_id) = (TaskState::Running, None, Some("a-1".into()));
    let mut live = pacing_live(vec![], vec![], None);
    live.nodes.push(LiveNode { agent_id: "a-1".into(), name: "Node One".into(), working: true, ..LiveNode::default() });
    let detail = render(&state, &live, Client::Ios, Lang::Es, "task:t-1", "");
    let pause = find_action(&detail, "pause:a-1").expect("a working node can be paused from its task");
    assert_eq!(pause.confirm.as_ref().unwrap().title, "¿Pausar este nodo?");

    live.nodes[0].working = false;
    let paused = LivePaused { task_id: "t-1".into(), session: Some("a-1".into()), at: "14:05".into(), reason: LivePauseReason::Memory, fine_minutes: Some(2), next: false, waits_for: None, retry: None };
    live.pacing.as_mut().unwrap().paused.push(paused);
    let detail = render(&state, &live, Client::Tui, Lang::En, "task:t-1", "");
    assert!(action_ids(&detail).contains(&"resume:a-1".to_owned()), "{:?}", action_ids(&detail));
    assert!(!action_ids(&detail).iter().any(|id| id.starts_with("pause:")));
    let task = render(&state, &live, Client::Ios, Lang::En, "task:t-1", "");
    assert!(action_ids(&task).contains(&"resume:a-1".to_owned()), "{:?}", action_ids(&task));
}

#[test]
fn a_task_detail_shows_its_priority_and_offers_the_three_levels() {
    let mut state = fixture();
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation, task.priority) = (TaskState::NotStarted, None, crate::domain::TaskPriority::High);
    let detail = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "task:t-1", "");
    let text = serde_json::to_string(&detail).unwrap();
    assert!(text.contains("\"label\":\"Prioridad\",\"value\":\"Alta\""), "{text}");
    let priority = find_action(&detail, "set_priority:t-1").expect("priority is changed from the task's detail");
    let options: Vec<(&str, &str)> = priority.options.iter().map(|option| (option.id.as_str(), option.label.as_str())).collect();
    assert_eq!(options, [("high", "Alta · ahora"), ("normal", "Normal"), ("low", "Baja")]);
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    task.state = TaskState::Done;
    let done = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "task:t-1", "");
    assert!(find_action(&done, "set_priority:").is_none(), "a finished task has no priority to change");
}

const PACING_GOLDENS: [(&str, &str, Client, Lang); 5] = [
    ("task-pacing-ios.json", "task:t-queued-next", Client::Ios, Lang::En),
    ("task-pacing-ios-es.json", "task:t-queued-next", Client::Ios, Lang::Es),
    ("machine-settings-ios.json", "machine", Client::Ios, Lang::En),
    ("machine-settings-ios-es.json", "machine", Client::Ios, Lang::Es),
    ("machine-settings-mac.json", "machine", Client::Mac, Lang::En),
];

/// The nodes at their limit: 6 of 6 working with the Mac short of memory, two tasks queued, a node
/// paused for memory and a queued task the user paused.
fn pacing_fixture() -> (DomainState, LiveSnapshot) {
    let (mut state, mut live) = nodes_fixture();
    let template = state.tasks[&"t-zeta-worker".into()].clone();
    for (id, title, priority) in [
        ("t-queued-next", "Fix the crash when the schedule list is empty", crate::domain::TaskPriority::High),
        ("t-queued-later", "Translate the onboarding into Spanish", crate::domain::TaskPriority::Normal),
        ("t-held", "Refresh the App Store screenshots", crate::domain::TaskPriority::Low),
    ] {
        let mut task = template.clone();
        (task.id, task.title, task.state, task.agent_id, task.priority) = (id.into(), title.into(), TaskState::NotStarted, None, priority);
        state.tasks.insert(task.id.clone(), task);
    }
    live.pacing = Some(LivePacing {
        working: 6, limit: 6, fixed: None, auto: 6, most: 12, memory_free: Some(12), tight: true,
        queued: vec![
            LiveQueued { pausable: true, task_id: "t-queued-next".into(), position: 1, of: 2, hold: LiveHold::Memory { free: Some(12) } },
            LiveQueued { pausable: true, task_id: "t-queued-later".into(), position: 2, of: 2, hold: LiveHold::Memory { free: Some(12) } },
        ],
        paused: vec![
            LivePaused { task_id: "t-zeta-worker".into(), session: Some("zeta-worker".into()), at: "14:05".into(),
                reason: LivePauseReason::Memory, fine_minutes: Some(2), next: false, waits_for: None, retry: None },
            LivePaused { task_id: "t-held".into(), session: None, at: "11:20".into(),
                reason: LivePauseReason::User, fine_minutes: Some(2), next: false, waits_for: None, retry: None },
        ],
    });
    live.nodes.iter_mut().filter(|node| node.agent_id == "zeta-worker").for_each(|node| node.working = false);
    live.allowed_rules = vec![
        LiveAllowedRule { id: 3, repo: "alondra".into(), rule: "make build *".into(), added: "2026-09-29T09:12:00.000+02:00".into(),
            task_id: Some("t-zeta-worker".into()) },
        LiveAllowedRule { id: 4, repo: "alondra".into(), rule: "cargo test *".into(), added: "2026-09-30T08:40:00.000+02:00".into(),
            task_id: None },
    ];
    (state, live)
}

#[test]
fn pacing_views_match_goldens() {
    let (state, live) = pacing_fixture();
    for (filename, key, client, lang) in PACING_GOLDENS {
        let expected = match filename {
            "task-pacing-ios.json" => include_str!("../../../../fixtures/views/cards/task-pacing-ios.json"),
            "machine-settings-ios.json" => include_str!("../../../../fixtures/views/cards/machine-settings-ios.json"),
            "machine-settings-ios-es.json" => include_str!("../../../../fixtures/views/cards/machine-settings-ios-es.json"),
            "machine-settings-mac.json" => include_str!("../../../../fixtures/views/cards/machine-settings-mac.json"),
            _ => include_str!("../../../../fixtures/views/cards/task-pacing-ios-es.json"),
        };
        let actual = render(&state, &live, client, lang, key, "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

#[test]
#[ignore = "explicit fixture generation after Bazel authorization"]
fn generate_pacing_goldens() {
    let dir = std::env::var("ALONDRA_GOLDEN_OUT").expect("ALONDRA_GOLDEN_OUT required");
    let (state, live) = pacing_fixture();
    for (filename, key, client, lang) in PACING_GOLDENS {
        let view = render(&state, &live, client, lang, key, "2026-09-30T10:00:00Z");
        std::fs::write(format!("{dir}/{filename}"), format!("{}\n", serde_json::to_string_pretty(&view).unwrap())).unwrap();
    }
}

fn a_pr_card_that_cannot_merge_says_why() {
    let mut state = golden_fixture();
    let key = "card:hf-weekly-2026-09-30";
    let reason = |view: &View| card_of(view).context.iter().find(|line| line.icon == Icon::Warning).map(|line| line.text.clone());
    let offered = render(&state, &golden_live(), Client::Ios, Lang::Es, key, "");
    assert!(card_of(&offered).actions.iter().any(|action| action.id.starts_with("merge:")));
    assert_eq!(reason(&offered), None, "a card that merges needs no reason");

    state.pull_requests.get_mut(&"github.com/example/repo#12".into()).unwrap().head = "89abcdef0123456789abcdef0123456789abcdef".into();
    let moved = render(&state, &golden_live(), Client::Ios, Lang::Es, key, "");
    assert!(!card_of(&moved).actions.iter().any(|action| action.id.starts_with("merge:")));
    assert_eq!(reason(&moved).as_deref(), Some(
        "La PR #12 tiene commits que este resultado no incluye: no se puede hacer merge desde aquí hasta que el nodo la vuelva a presentar."));
}

#[test]
fn an_item_kind_from_a_newer_daemon_becomes_one_unknown_row_and_keeps_the_rest_of_the_view() {
    let mut json: serde_json::Value = serde_json::from_str(include_str!("../../../../fixtures/views/cards/home-tui.json")).unwrap();
    let section = json["sections"].as_array_mut().unwrap().iter_mut().find(|s| !s["items"].as_array().unwrap().is_empty()).unwrap();
    let known = section["items"].as_array().unwrap().len();
    section["items"].as_array_mut().unwrap().push(serde_json::json!({"type": "gauge_wall", "bars": [1, 2, 3]}));
    let id = section["id"].clone();

    let view: View = serde_json::from_value(json).expect("a newer item kind must not fail the whole view");

    let items = &view.sections.iter().find(|s| serde_json::json!(s.id) == id).unwrap().items;
    assert_eq!(items.len(), known + 1);
    assert_eq!(items.last(), Some(&Item::Unknown));
    assert!(items[..known].iter().all(|item| *item != Item::Unknown));
}

fn task_in(state: &mut DomainState, id: &str, request: &str, task_state: TaskState) {
    let mut task = fixture().tasks[&"t-1".into()].clone();
    (task.id, task.request_id, task.title, task.state, task.final_presentation) =
        (id.into(), request.into(), format!("Task {id}"), task_state, None);
    task.dependencies = Default::default();
    state.tasks.insert(task.id.clone(), task);
}

fn block_ids(state: &DomainState, live: &LiveSnapshot) -> Vec<(String, Vec<String>)> {
    active_blocks(state, live, Client::Ios, Lang::En).into_iter()
        .map(|block| (block.id, block.children.into_iter().map(|row| row.id).collect()))
        .collect()
}

#[test]
fn the_active_list_is_for_you_then_in_progress_then_waiting_each_in_its_own_order() {
    let mut state = fixture();
    let question = state.interactions[&"i-1".into()].clone();
    state.interactions.clear();
    state.tasks.clear();
    for (id, task_state) in [("t-old", TaskState::NeedsYou), ("t-new", TaskState::NeedsYou), ("t-busy", TaskState::Running),
        ("t-run2", TaskState::Running), ("t-q2", TaskState::NotStarted), ("t-q1", TaskState::NotStarted)] {
        task_in(&mut state, id, "r-1", task_state);
    }
    for task in ["t-old", "t-new"] {
        let mut asked = question.clone();
        asked.id = format!("q-{task}").into();
        asked.scope = Scope::Task { task_id: task.into() };
        state.interactions.insert(asked.id.clone(), asked);
    }
    for (id, agent) in [("t-busy", "a-busy"), ("t-run2", "a-other")] {
        state.tasks.get_mut(&id.into()).unwrap().agent_id = Some(agent.into());
    }
    let mut live = LiveSnapshot::default();
    for (id, at) in [("t-old", "2026-10-08T08:00:00+02:00"), ("t-new", "2026-10-08T09:00:00+02:00")] {
        live.task_since.insert(id.into(), at.into());
    }
    for (agent, since) in [("a-busy", "2026-10-08T10:05:00+02:00"), ("a-other", "2026-10-08T10:01:00+02:00")] {
        live.nodes.push(LiveNode { agent_id: agent.into(), name: agent.into(), working: true, tool: Some("Read: src/lib.rs".into()),
            state_since: Some(since.into()), ..LiveNode::default() });
    }
    live.pacing = Some(LivePacing { working: 2, limit: 2, fixed: None, auto: 2, most: 12, memory_free: Some(40), tight: false, paused: vec![],
        queued: ["t-q1", "t-q2"].iter().enumerate().map(|(index, id)| LiveQueued {
            pausable: true, task_id: (*id).into(), position: index + 1, of: 2, hold: LiveHold::Slots { working: 2, limit: 2 } }).collect() });
    assert_eq!(block_ids(&state, &live), [
        ("block:for-you".to_owned(), vec!["t-old".to_owned(), "t-new".to_owned()]),
        ("block:in-progress".to_owned(), vec!["t-busy".to_owned(), "t-run2".to_owned()]),
        ("block:waiting".to_owned(), vec!["t-q1".to_owned(), "t-q2".to_owned()]),
    ], "longest waiting first, most recently active first, the queue by its place");
    let blocks = active_blocks(&state, &live, Client::Ios, Lang::Es);
    assert_eq!(blocks.iter().map(|block| (block.title.as_str(), block.trailing.clone())).collect::<Vec<_>>(), [
        ("Para ti", Some(Trailing::Text { text: "2".into() })),
        ("En marcha", Some(Trailing::Text { text: "2".into() })),
        ("En espera", Some(Trailing::Text { text: "2".into() })),
    ]);
    let (_, queued) = active_row(&state, &live, Client::Mac, Lang::Es, "t-q2");
    assert_eq!((queued.badges[0].text.as_str(), queued.subtitle.as_deref()), ("En cola · 2.º de 2", Some("Hay 2 de 2 trabajando")));
    live.pacing.as_mut().unwrap().queued[0].hold = LiveHold::Next;
    assert_eq!(active_row(&state, &live, Client::Mac, Lang::Es, "t-q1").1.subtitle.as_deref(), Some("Arranca en menos de un minuto"));
    state.tasks.retain(|id, _| id.0.starts_with("t-q"));
    assert_eq!(block_ids(&state, &live).iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["block:waiting"], "an empty block is not shown");
}

#[test]
fn a_request_has_a_header_only_when_it_is_not_a_message_to_the_orchestrator() {
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.clear();
    for id in ["t-a", "t-b"] {
        task_in(&mut state, id, "r-1", TaskState::Running);
        let agent = crate::domain::AgentId(format!("a-{id}"));
        state.agents.insert(agent.clone(), Agent { id: agent.clone(), name: id.into(), phase: NodePhase::Working, since: 0 });
        state.tasks.get_mut(&id.into()).unwrap().agent_id = Some(agent);
    }
    state.requests.get_mut(&"r-1".into()).unwrap().title = Some("Resume the queue and two new bugs".into());
    state.requests.get_mut(&"r-1".into()).unwrap().message = true;
    let ids = |state: &DomainState| rows_of(state, Client::Ios, Lang::En).into_iter().map(|row| row.id)
        .filter(|id| id != "activity").collect::<Vec<_>>();
    assert_eq!(ids(&state), ["t-a", "t-b"], "a message to the orchestrator joins unrelated tasks: they stand loose");
    state.requests.get_mut(&"r-1".into()).unwrap().message = false;
    assert_eq!(ids(&state), ["group:r-1", "t-a", "t-b"], "a request with a real title and several tasks keeps its header");
    let (_, header) = active_row(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "group:r-1");
    assert_eq!(header.title, "Resume the queue and two new bugs");
    for id in ["t-a", "t-b"] {
        let task = state.tasks.get_mut(&id.into()).unwrap();
        task.state = TaskState::NotStarted;
        task.agent_id = None;
    }
    let live = LiveSnapshot { start_waits: [("t-a".into(), LiveStartWait::Room), ("t-b".into(), LiveStartWait::Room)].into(), ..LiveSnapshot::default() };
    let waiting = active_blocks(&state, &live, Client::Mac, Lang::En);
    assert_eq!(waiting[0].children.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), ["t-a", "t-b"],
        "a group that only waits has no header");
    assert_eq!(waiting[0].children[0].subtitle.as_deref(), Some("Starts when there is room · Resume the queue and two new bugs"),
        "its name goes with each line");
}

#[test]
fn a_node_that_stopped_says_what_it_said_and_only_in_prose() {
    let node = |message: &str| LiveNode {
        agent_id: "a-1".into(), name: "n".into(), working: false, last_message: Some(message.into()),
        last_message_at: Some("2026-10-09T06:40:00+02:00".into()), ..LiveNode::default()
    };
    let doing = |node: &LiveNode, lang| task_node::doing(node, false, lang).map(|(text, _)| text);
    assert_eq!(doing(&node("Done with the parser. Now the tests."), Lang::Es).as_deref(), Some("Dijo: «Done with the parser.»"));
    assert_eq!(task_node::doing(&node("Waiting for Bazel."), false, Lang::En).and_then(|(_, at)| at).as_deref(), Some("2026-10-09T06:40:00+02:00"));
    assert_eq!(doing(&node("`cargo test` passed"), Lang::En), None, "only prose is quoted");
    assert_eq!(doing(&node("Edit: src/main.rs"), Lang::En), None);
    let long = "word ".repeat(40);
    assert!(doing(&node(&long), Lang::En).unwrap().chars().count() <= 80 + "Said: “”".chars().count());
    assert_eq!(doing(&node("## Summary\n\n- Parser **done**. Tests next."), Lang::En).as_deref(), Some("Said: “Parser **done**.”"),
        "a heading is skipped and a list item loses its mark");
    assert_eq!(doing(&node("> 1. Shipped it. Then more."), Lang::En).as_deref(), Some("Said: “Shipped it.”"));
    assert_eq!(doing(&node("---\n2) Merged the fix."), Lang::En).as_deref(), Some("Said: “Merged the fix.”"));
    let working = LiveNode { tool: Some("Bash: cargo test".into()), ..node("Earlier") };
    assert_eq!(task_node::doing(&working, true, Lang::En), None, "at work, nothing inferred from its tool");
}
#[test]
fn a_task_that_needs_you_says_its_question_never_its_own_title() {
    let mut state = with_cards(&fixture());
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::NeedsYou;
    let pending = inbox_items(&state).iter().filter(|item| item.pending && item.task_id == Some("t-1".into())).count();
    assert!(pending > 0, "the fixture has cards waiting on its task");
    let live = LiveSnapshot::default();
    let (block, ios) = active_row(&state, &live, Client::Ios, Lang::Es, "t-1");
    let fact = ios.children.iter().find(|child| child.id == "activity").map(|line| line.title.clone()).unwrap();
    assert_eq!(block, "block:for-you");
    assert!(!fact.contains(&state.tasks[&"t-1".into()].title), "{fact}");
    if pending > 1 {
        assert_eq!(fact, format!("{pending} preguntas abiertas"));
    }
    let mac = active_row(&state, &live, Client::Mac, Lang::Es, "t-1").1;
    let questions = if pending == 1 { "1 pregunta".to_owned() } else { format!("{pending} preguntas") };
    assert_eq!(mac.trailing, Some(Trailing::Text { text: questions }), "the Mac says what the number counts");
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Running;
    assert_eq!(active_row(&state, &live, Client::Mac, Lang::Es, "t-1").1.trailing, None, "no number when nothing waits");
}


#[test]
fn a_working_task_between_turns_still_says_only_working() {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    (task.agent_id, task.state, task.final_presentation) = (Some("a-1".into()), TaskState::Running, None);
    let live = LiveSnapshot { nodes: vec![LiveNode {
        agent_id: "a-1".into(), name: "n".into(), working: false,
        last_message: Some("Folding works. Now the captures.".into()), last_message_at: Some("2026-10-09T06:40:00+02:00".into()),
        ..LiveNode::default()
    }], ..LiveSnapshot::default() };
    let (block, row) = active_row(&state, &live, Client::Ios, Lang::Es, "t-1");
    assert_eq!((block.as_str(), row.badges[0].text.as_str(), row.children.len()), ("block:in-progress", "Trabajando", 0),
        "what it said last is not what it does now");
}

/// A task Claude Code would not start in: the card is worded in the viewer's language, its button
/// says what it does, and it offers no reply because no node is behind it.
#[test]
fn an_untrusted_folder_card_speaks_the_users_language_and_offers_no_reply() {
    use crate::domain::{Actor, Command, CommandEnvelope, StartProblem};
    let mut state = failed_launch_fixture("Workspace not trusted.\n");
    let raise = |state: &mut DomainState, problem: StartProblem| {
        state.execute(&crate::domain::Actor::System, &crate::domain::CommandEnvelope {
            protocol_version: 2, data_epoch: state.data_epoch.clone(), command_id: format!("c-{}", state.revision).into(),
            expected_revision: state.revision, command: Command::RaiseStartProblem { task_id: "t-1".into(), problem },
        }).unwrap();
    };
    raise(&mut state, StartProblem::Untrusted { folder: "/Users/someone/Projects/indie-apps".into(), error: None });
    let id = state.interactions.keys().find(|id| id.0.starts_with(crate::domain::START_PROBLEM_PREFIX)).unwrap().0.clone();

    let spanish = decidable_card(&state, Lang::Es, &id);
    assert_eq!(spanish.need.as_deref(), Some("Confía en indie-apps para que «Build a feature» arranque"));
    assert_eq!(spanish.why.as_deref(), Some("Claude Code solo arranca sesiones en carpetas de confianza, y aún no confía en esta."));
    let labels: Vec<&str> = spanish.actions.iter().chain(&spanish.more).map(|action| action.label.as_str()).collect();
    assert!(labels.contains(&"Confiar y arrancarla") && labels.contains(&"Cancelar la tarea"), "{labels:?}");
    assert!(spanish.reply.is_none(), "no node is behind a task that did not start");
    let detail = format!("{:?}", render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, &format!("card:{id}"), ""));
    assert!(detail.contains("Carpeta: /Users/someone/Projects/indie-apps") && detail.contains("acepta el aviso"), "{detail}");

    let english = decidable_card(&state, Lang::En, &id);
    assert_eq!(english.need.as_deref(), Some("Trust indie-apps so “Build a feature” can start"));
    assert!(english.actions.iter().chain(&english.more).any(|action| action.label.contains("Trust it and start it")));
    assert!(english.reply.is_none());
}

#[test]
fn a_folder_alondra_could_not_trust_says_why_on_its_card() {
    use crate::domain::{Actor, Command, CommandEnvelope, StartProblem};
    let mut state = failed_launch_fixture("Workspace not trusted.\n");
    state.execute(&Actor::System, &CommandEnvelope {
        protocol_version: 2, data_epoch: state.data_epoch.clone(), command_id: "c-1".into(), expected_revision: state.revision,
        command: Command::RaiseStartProblem { task_id: "t-1".into(), problem: StartProblem::Untrusted {
            folder: "/Users/someone/Projects/indie-apps".into(), error: Some("another Claude Code session kept ~/.claude.json locked".into()),
        } },
    }).unwrap();
    let id = state.interactions.keys().find(|id| id.0.starts_with(crate::domain::START_PROBLEM_PREFIX)).unwrap().0.clone();

    let card = decidable_card(&state, Lang::Es, &id);
    assert_eq!(card.why.as_deref(), Some("Elegiste confiar en ella, pero Alondra no pudo guardarlo en los ajustes de Claude Code."));
    let detail = format!("{:?}", render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, &format!("card:{id}"), ""));
    assert!(detail.contains("Por qué falló: another Claude Code session kept ~/.claude.json locked"), "{detail}");
}

/// Every card action says, from the server, whether it takes its card out of the queue; the
/// clients hide the card on that alone. A reply answers an open question and hands a delivery back,
/// but is only a message on a notice or on a question the node is holding a session for.
#[test]
fn every_card_action_says_whether_it_takes_its_card_out() {
    fn resolving(state: &DomainState, key: &str) -> Vec<(String, bool)> {
        let view = render(state, &golden_live(), Client::Ios, Lang::En, key, "2026-09-30T10:00:00Z");
        let card = card_of(&view);
        card.actions.iter().chain(&card.more).chain(&card.reply)
            .map(|action| (action.id.split(['@', '~']).next().unwrap().to_owned(), action.resolves_card))
            .collect()
    }
    fn owned(pairs: &[(&str, bool)]) -> Vec<(String, bool)> {
        pairs.iter().map(|(id, resolves)| ((*id).to_owned(), *resolves)).collect()
    }
    let state = golden_fixture();
    let question = "a98d034b-249e-4487-a89b-57de38c26372";
    assert_eq!(resolving(&state, &format!("card:{question}")), owned(&[
        (&format!("ans:{question}"), true), (&format!("ans:{question}"), true),
        (&format!("clarify:{question}"), false), (&format!("reply:{question}"), true),
    ]), "an option and a written answer both answer the question; asking to clarify keeps it");
    let notice = "f12d034b-249e-4487-a89b-57de38c26372";
    assert_eq!(resolving(&state, &format!("card:{notice}")), owned(&[
        (&format!("read:{notice}"), true), (&format!("reply:{notice}"), false),
    ]), "a notice leaves when read; a reply to it is a message to its node");
    assert_eq!(resolving(&state, "card:hf-weekly-2026-09-30"), owned(&[
        ("merge:hf-weekly-2026-09-30", true), ("changes:github.com/example/repo#12", true),
        ("merge:hf-weekly-2026-09-30", true), ("close_unmerged:hf-weekly-2026-09-30", true), ("", false), ("reply:hf-weekly-2026-09-30", false),
    ]), "merging (either way), asking for changes and closing settle the delivery; the link opens GitHub; with no node to act on it, a reply leaves it in place");

    let mut group = group_state(&[0]);
    assert_eq!(resolving(&group, "card:g-q2"), owned(&[
        ("ans:g-q2", true), ("ans:g-q2", true),
        ("clarify:g-q2", false), ("retake:g-q2", false), ("reply:g-q2", true),
    ]), "a question of a group is answered alone; reopening another one keeps this one");
    group.holds.insert("g-q2".into(), crate::domain::Hold::Question);
    let held = resolving(&group, "card:g-q2");
    assert!(held.contains(&("reply:g-q2".to_owned(), true)), "words written on a held question answer it in its session: {held:?}");
}

const DOCUMENT_QUESTION_CARD: &str = "a98d034b-249e-4487-a89b-57de38c26372";

/// The question of the 9 October report: a node asks for the review of a document, so the terminal
/// shows its options, Request changes and Request clarification with its three reasons.
fn document_question_state() -> DomainState {
    let mut state = golden_fixture();
    state.documents.insert("design".into(), Document {
        id: "design".into(), title: "Migration design".into(), current_revision: "design-r1".into(),
    });
    state.document_revisions.insert("design-r1".into(), DocumentRevision {
        id: "design-r1".into(), document_id: "design".into(), number: 1,
        content_digest: "d".repeat(64), content_reference: "stored/design".into(),
    });
    state.interactions.get_mut(&DOCUMENT_QUESTION_CARD.into()).unwrap().subject =
        Some(InteractionSubject::DocumentReview { document_id: "design".into(), revision_id: "design-r1".into() });
    state
}

#[test]
fn document_question_card_for_the_terminal_matches_golden() {
    let view = render(&document_question_state(), &golden_live(), Client::Tui, Lang::Es,
        &format!("card:{DOCUMENT_QUESTION_CARD}"), "2026-09-30T10:00:00Z");
    let card = card_of(&view);
    assert!(card.actions.iter().any(|action| action.id.starts_with("changes:")), "{:?}", card.actions);
    assert!(card.more.iter().chain(&card.actions).any(|action| action.id.starts_with("clarify:")), "{card:?}");
    assert_golden("card-document-question-es-tui.json", &view,
        include_str!("../../../../fixtures/views/cards/card-document-question-es-tui.json"));
}

/// Every row of a view with the rows above it, outermost first.
fn rows_with_parents(view: &View) -> Vec<(Vec<String>, Row)> {
    fn walk(row: &Row, parents: &mut Vec<String>, out: &mut Vec<(Vec<String>, Row)>) {
        out.push((parents.clone(), row.clone()));
        parents.push(row.id.clone());
        for child in &row.children {
            walk(child, parents, out);
        }
        parents.pop();
    }
    let mut out = Vec::new();
    for item in view.sections.iter().flat_map(|section| &section.items) {
        if let Item::Row(row) = item {
            walk(row, &mut Vec::new(), &mut out);
        }
    }
    out
}

#[test]
fn the_golden_tasks_list_folds_the_chain_and_lists_its_tasks_only_under_it() {
    for (name, golden) in [
        ("tasks-tui.json", include_str!("../../../../fixtures/views/cards/tasks-tui.json")),
        ("tasks-ios.json", include_str!("../../../../fixtures/views/cards/tasks-ios.json")),
        ("tasks-mac.json", include_str!("../../../../fixtures/views/cards/tasks-mac.json")),
    ] {
        let view: View = serde_json::from_str(golden).unwrap();
        let rows = rows_with_parents(&view);
        let (_, chain) = rows.iter().find(|(_, row)| row.id.starts_with("chain:")).unwrap_or_else(|| panic!("{name}: no chain"));
        assert!(chain.folded, "{name}: the chain starts folded");
        assert!(!chain.children.is_empty(), "{name}");
        for member in &chain.children {
            let places: Vec<&Vec<String>> = rows.iter().filter(|(_, row)| row.id == member.id).map(|(parents, _)| parents).collect();
            assert_eq!(places.len(), 1, "{name}: {} is listed once", member.id);
            assert_eq!(places[0].last(), Some(&chain.id), "{name}: {} hangs only under the chain", member.id);
        }
    }
}

/// The orchestrator is Alondra's own: no screen lists it, counts it or opens it.
#[test]
fn no_screen_shows_the_orchestrator() {
    let mut state = golden_fixture();
    state.orchestrator_session = Some("6f1c2b9e-1111-4a2b-9c3d-0123456789ab".into());
    state.agents.insert("orchestrator".into(), Agent { id: "orchestrator".into(), name: "orchestrator".into(), phase: NodePhase::Working, since: 0 });
    let live = golden_live();
    for (client, lang) in [(Client::Ios, Lang::En), (Client::Ios, Lang::Es), (Client::Mac, Lang::Es), (Client::Tui, Lang::En)] {
        let screens: &[&str] = if client == Client::Mac { &["sidebar", "tasks"] } else { &["tasks", "home"] };
        for key in screens.iter().copied() {
            let view = render(&state, &live, client, lang, key, "");
            let text = serde_json::to_string(&(&view.title, &view.sections)).unwrap().to_lowercase();
            assert!(!text.contains("orchestrator") && !text.contains("orquestador"), "{client:?} {key}: {text}");
        }
    }
}

/// 13 October: the daemon release task delivered its version PR and then the published release;
/// both were accepted and its node had gone, yet an app said there was something to review with
/// no card in the inbox nor in the task. What says a task waits for the user, and every count of
/// it, comes from the cards the inbox shows.
fn release_delivered_twice(published: Option<crate::domain::Acceptance>) -> DomainState {
    let mut state = fixture();
    let delivery = state.interactions[&"i-3".into()].clone();
    state.interactions.clear();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    task.title = "Release the daemon with the memory pause fix".into();
    task.agent_id = Some("a-1".into());
    task.final_presentation = Some("published-1".into());
    let version_pr = Some(crate::domain::Acceptance { user_id: "phone".into(), at_revision: 2, valid: false });
    for (order, (id, accepted)) in [("pr-1", version_pr), ("published-1", published)].into_iter().enumerate() {
        let mut card = delivery.clone();
        card.id = id.into();
        card.order = order as u64 + 1;
        card.blocking = false;
        card.author = Author::Agent { agent_id: "a-1".into() };
        if let InteractionPayload::Delivery { acceptance, .. } = &mut card.payload {
            *acceptance = accepted;
        }
        state.interactions.insert(card.id.clone(), card);
    }
    state.agents.get_mut(&"a-1".into()).unwrap().phase = NodePhase::Delivered { in_turn: false };
    state.derive_states();
    with_cards(&state)
}

/// Everything a client says about the release task, and the counts beside it.
struct Said {
    inbox: u32,
    for_you: Vec<String>,
    for_you_count: Option<String>,
    row_state: String,
    detail_cards: usize,
    waiting_nodes: u32,
}

fn said(state: &DomainState, client: Client, lang: Lang) -> Said {
    let live = LiveSnapshot::default();
    let blocks = active_blocks(state, &live, client, lang);
    let for_you_block = blocks.iter().find(|block| block.id == "block:for-you");
    let for_you = for_you_block.map(|block| block.children.iter().map(|row| row.id.clone()).collect()).unwrap_or_default();
    let for_you_count = for_you_block.and_then(|block| match &block.trailing {
        Some(Trailing::Text { text }) => Some(text.clone()),
        _ => None,
    });
    let row_state = active_row(state, &live, client, lang, "t-1").1.badges[0].text.clone();
    let detail = render(state, &live, client, lang, "task:t-1", "");
    let detail_cards = detail.sections.iter().filter(|section| section.id == "waiting").map(|section| section.items.len()).sum();
    let waiting_nodes = detail.status.counts.iter().find(|count| count.id == "waiting").map_or(0, |count| count.value);
    Said { inbox: inbox_badge(state), for_you, for_you_count, row_state, detail_cards, waiting_nodes }
}

const CLAIMS: [&str; 6] = ["Ready for review", "Para revisar", "Needs you", "Te necesita", "Waiting for your review", "Espera tu revisión"];

fn says_nothing_waits(state: &DomainState) {
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for lang in [Lang::En, Lang::Es] {
            let said = said(state, client, lang);
            assert_eq!((said.inbox, said.detail_cards), (0, 0), "{client:?}: no card waits");
            assert!(said.for_you.is_empty() && said.for_you_count.is_none(), "{client:?}: nothing is «For you»: {:?}", said.for_you);
            assert!(!CLAIMS.contains(&said.row_state.as_str()), "{client:?}: the task says {}", said.row_state);
            assert_eq!(said.waiting_nodes, 0, "{client:?}: no node counted as waiting");
        }
    }
}

#[test]
fn a_release_whose_two_deliveries_were_accepted_says_nothing_waits_for_you_on_any_client() {
    says_nothing_waits(&release_delivered_twice(Some(crate::domain::Acceptance { user_id: "phone".into(), at_revision: 4, valid: true })));
}

#[test]
fn a_release_whose_published_delivery_was_withdrawn_says_nothing_waits_for_you_on_any_client() {
    let mut state = release_delivered_twice(None);
    state.interactions.get_mut(&"published-1".into()).unwrap().withdrawn = true;
    state.derive_states();
    assert_eq!(state.tasks[&"t-1".into()].state, TaskState::ForYourReview, "the task still reads as in review");
    says_nothing_waits(&state);
}

#[test]
fn a_release_whose_node_asks_the_orchestrator_says_nothing_waits_for_you_on_any_client() {
    let mut state = release_delivered_twice(None);
    let mut asked = fixture().interactions[&"i-1".into()].clone();
    asked.id = "ask-orchestrator".into();
    asked.recipient = Recipient::Orchestrator;
    state.interactions.insert(asked.id.clone(), asked);
    state.interactions.get_mut(&"published-1".into()).unwrap().withdrawn = true;
    state.derive_states();
    assert_eq!(state.tasks[&"t-1".into()].state, TaskState::ForYourReview, "a question to the orchestrator holds the task on the orchestrator, never on the user");
    says_nothing_waits(&state);
}

#[test]
fn a_release_whose_published_delivery_is_open_shows_its_card_wherever_it_says_it_waits() {
    let state = release_delivered_twice(None);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let said = said(&state, client, Lang::Es);
        assert_eq!((said.inbox, said.detail_cards), (1, 1), "{client:?}: the inbox and the task show its one card");
        assert_eq!((said.for_you, said.for_you_count.as_deref()), (vec!["t-1".to_owned()], Some("1")), "{client:?}");
        assert_eq!(said.row_state, "Para revisar", "{client:?}");
        assert_eq!(said.waiting_nodes, 1, "{client:?}");
    }
}

#[test]
fn every_card_button_leaves_the_server_ready_to_paint() {
    use crate::allow_always::AllowAlways;
    use crate::domain::{CardKind, Hold};
    let mut permission = held_fixture(Hold::Permission, spec(CardKind::Authorize, "Allow building", ""));
    let interaction = permission.interactions.get_mut(&"i-1".into()).unwrap();
    interaction.payload = InteractionPayload::Question { options: Hold::Permission.options_offering(true), answer: None };
    interaction.card.allow_always = Some(AllowAlways::Offered { repo: "alondra".into(), rules: vec!["make build".into()], quiet: vec![] });
    let question = described_question_fixture();
    let states = [(golden_fixture(), golden_live()), (pr_fixture(), LiveSnapshot::default()), (permission, LiveSnapshot::default()),
        (question, LiveSnapshot::default())];
    let mut checked = 0;
    let mut captions = 0;
    for (state, live) in &states {
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            for lang in [Lang::En, Lang::Es] {
                let home = render(state, live, Client::Ios, lang, "home", "2026-09-30T10:00:00Z");
                for id in home.sections.iter().flat_map(|section| &section.card_ids) {
                    let view = render(state, live, client, lang, &format!("card:{id}"), "2026-09-30T10:00:00Z");
                    let card = card_of(&view);
                    assert!(card.actions.len() <= 3, "{id}: {} buttons", card.actions.len());
                    for action in card.actions.iter().chain(&card.more).chain(&card.reply) {
                        assert!(action.enabled, "{id}: {} is disabled", action.label);
                        let role = action.role.unwrap_or_else(|| panic!("{id}: {} has no role", action.label));
                        assert_eq!(role == Role::Risk, action.style == Style::Destructive, "{id}: {} is {role:?} as {:?}", action.label, action.style);
                        if action.description.is_some() {
                            let claudes = state.holds.get(&id.as_str().into()) == Some(&Hold::Question);
                            assert!(claudes || action.option_id.as_deref() == Some(Hold::ALWAYS), "{id}: words under {}", action.label);
                            captions += 1;
                        }
                    }
                    if let Some(reply) = &card.reply {
                        assert!(matches!(&reply.input, Input::Text { placeholder, .. } if !placeholder.is_empty()), "{id}: reply without placeholder");
                    }
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 30, "only {checked} cards checked");
    assert!(captions > 0, "«Allow always» lost its words");
}

fn proposed_plan_fixture() -> DomainState {
    use crate::domain::{Actor, CardKind, CardSpec, Command, CommandEnvelope, ContentKind as Content, DeliveryTarget, LaunchPlan};
    let mut state = DomainState::new("epoch");
    let mut run = |state: &mut DomainState, actor: Actor, command: Command| {
        let envelope = CommandEnvelope { protocol_version: 2, data_epoch: state.data_epoch.clone(),
            command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command };
        state.execute(&actor, &envelope).unwrap();
    };
    run(&mut state, Actor::System, Command::RegisterAgent { agent_id: "a-1".into(), name: "redraft/library-index".into(), at: 0 });
    run(&mut state, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r-1".into(),
        body: "How do other apps analyse the whole library at once?".into(), auto_merge: false, maintenance: false, message: false });
    run(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t-1".into(), request_id: "r-1".into(),
        title: "Redraft: analyse the whole library at once".into(), agent_id: Some("a-1".into()), review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("redraft/library-index", "/tmp/redraft")) });
    let targets = vec![DeliveryTarget::Text { text: "Report".into() }];
    let mut card = CardSpec::new(CardKind::Review, "Read the report on indexing Redraft's whole library")
        .why("It ends in a plan: approve it to start building it, or only archive the report.")
        .content(Content::Report, "## Summary\n\n- **Viable.** Every photo of an optimised library keeps a 360 px copy on the iPhone.\n- Other apps index once and then follow the library's changes.")
        .plan("# Index the whole library\n\n- [ ] Store each photo's feature print in Core Data, next to what users have today\n- [ ] Index changes in the background with the library's change history\n- [ ] Show indexing progress on the home screen");
    card.kept_at = Some("/Users/me/.alondra/files/results/t-1/result-1.md".into());
    run(&mut state, Actor::Agent { agent_id: "a-1".into() }, Command::PresentResult { checklist: vec![], attachments: vec![],
        task_id: "t-1".into(), interaction_id: "result-1".into(), body: "Read the report".into(), targets, is_final: true,
        nothing_to_review: false, card });
    state
}

#[test]
fn a_result_that_proposes_a_plan_shows_it_and_offers_to_build_it_or_only_archive_it() {
    let state = proposed_plan_fixture();
    let live = LiveSnapshot::default();
    for (client, lang, filename, expected) in [
        (Client::Ios, Lang::En, "card-delivery-plan.json", include_str!("../../../../fixtures/views/cards/card-delivery-plan.json")),
        (Client::Ios, Lang::Es, "card-delivery-plan-es.json", include_str!("../../../../fixtures/views/cards/card-delivery-plan-es.json")),
        (Client::Mac, Lang::Es, "card-delivery-plan-es-mac.json", include_str!("../../../../fixtures/views/cards/card-delivery-plan-es-mac.json")),
    ] {
        let actual = render(&state, &live, client, lang, "card:result-1", "2026-10-15T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
    let view = render(&state, &live, Client::Tui, Lang::Es, "card:result-1", "");
    let card = card_of(&view);
    let labels: Vec<&str> = card.actions.iter().map(|action| action.label.as_str()).collect();
    assert_eq!(labels, ["Aprobar y empezar a implementar", "Solo archivar", "Pedir cambios"]);
    assert!(card.actions[0].id.ends_with("~implement") || card.actions[0].id.contains("implement"), "{}", card.actions[0].id);
    let text = serde_json::to_string(&view).unwrap();
    assert!(!text.contains("Show indexing progress on the home screen"), "{text}");
    assert_eq!(document_links(&view), ["document:result-1.plan"]);
    let plan = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::Es, "document:result-1.plan", "")).unwrap();
    assert!(plan.contains("Show indexing progress on the home screen"), "{plan}");
    assert!(text.contains("Guardado en `/Users/me/.alondra/files/results/t-1/result-1.md`"), "{text}");
}

/// A result with a report, a design document of the task, a plan it proposes and two Markdown
/// files its words cite: four documents around a short report.
fn documents_fixture() -> DomainState {
    use crate::domain::{Actor, CardKind, CardSpec, Command, CommandEnvelope, ContentKind as Content, DeliveryTarget, LaunchPlan};
    let mut state = DomainState::new("epoch");
    let mut run = |state: &mut DomainState, actor: Actor, command: Command| {
        let envelope = CommandEnvelope { protocol_version: 2, data_epoch: state.data_epoch.clone(),
            command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command };
        state.execute(&actor, &envelope).unwrap();
    };
    run(&mut state, Actor::System, Command::RegisterAgent { agent_id: "a-1".into(), name: "redraft/library-index".into(), at: 0 });
    run(&mut state, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r-1".into(),
        body: "How do other apps analyse the whole library at once?".into(), auto_merge: false, maintenance: false, message: false });
    run(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t-1".into(), request_id: "r-1".into(),
        title: "Redraft: analyse the whole library at once".into(), agent_id: Some("a-1".into()), review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("redraft/library-index", "/tmp/redraft")) });
    run(&mut state, Actor::Agent { agent_id: "a-1".into() }, Command::CreateDocument { task_id: "t-1".into(), document_id: "design".into(),
        title: "Library index design".into(), revision_id: "design-r1".into(), content_digest: "d".repeat(64),
        content_reference: "files/design.md".into(), role: "result".into() });
    let report = crate::domain::include_cited_files(
        "## Summary\n\n- **Viable.** Every photo keeps a 360 px copy on the iPhone.\n- How other apps do it: docs/research/competitors.md",
        |path| (path == "docs/research/competitors.md").then(|| "# How other apps index a library\n\n## Photos\n\nIndexes overnight while charging.\n\n## Lightroom\n\nIndexes on import.".to_owned()),
    ).unwrap();
    let plan = crate::domain::include_cited_files(
        "# Index the whole library\n\nMeasurements in docs/measurements.md\n\n- [ ] Store each photo's feature print in Core Data\n- [ ] Index changes in the background\n- [ ] Show indexing progress on the home screen",
        |path| (path == "docs/measurements.md").then(|| "# Measurements\n\n| Library | Time |\n|---|---|\n| 10 000 photos | 4 min |".to_owned()),
    ).unwrap();
    let targets = vec![DeliveryTarget::DocumentRevision { document_id: "design".into(), revision_id: "design-r1".into() }];
    let card = CardSpec::new(CardKind::Review, "Read the report on indexing Redraft's whole library")
        .why("It ends in a plan: approve it to start building it, or only archive the report.")
        .content(Content::Report, &report)
        .plan(&plan);
    run(&mut state, Actor::Agent { agent_id: "a-1".into() }, Command::PresentResult { checklist: vec![], attachments: vec![],
        task_id: "t-1".into(), interaction_id: "result-1".into(), body: "Read the report".into(), targets, is_final: true,
        nothing_to_review: false, card });
    state
}

#[test]
fn a_cards_documents_are_links_in_their_own_section_and_never_part_of_its_text() {
    let state = documents_fixture();
    let live = LiveSnapshot::default();
    for (client, lang, filename, expected) in [
        (Client::Ios, Lang::En, "card-documents.json", include_str!("../../../../fixtures/views/cards/card-documents.json")),
        (Client::Ios, Lang::Es, "card-documents-es.json", include_str!("../../../../fixtures/views/cards/card-documents-es.json")),
        (Client::Mac, Lang::En, "card-documents-mac.json", include_str!("../../../../fixtures/views/cards/card-documents-mac.json")),
        (Client::Mac, Lang::Es, "card-documents-es-mac.json", include_str!("../../../../fixtures/views/cards/card-documents-es-mac.json")),
        (Client::Tui, Lang::Es, "card-documents-es-tui.json", include_str!("../../../../fixtures/views/cards/card-documents-es-tui.json")),
    ] {
        let actual = render(&state, &live, client, lang, "card:result-1", "2026-10-15T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
    let view = render(&state, &live, Client::Ios, Lang::Es, "card:result-1", "");
    let text: String = view.sections.iter().filter(|section| section.id != "documents").map(|section| serde_json::to_string(section).unwrap()).collect();
    for inside in ["Indexes overnight", "10 000 photos", "Show indexing progress", "/document-revisions/"] {
        assert!(!text.contains(inside), "the card holds «{inside}» in its text: {text}");
    }
    assert!(text.contains("Every photo keeps a 360 px copy"), "the report stays: {text}");
    let documents = view.sections.iter().find(|section| section.id == "documents").expect("a section of documents");
    assert_eq!(documents.title.as_deref(), Some("Documentos"));
    let [Item::Facts(facts)] = documents.items.as_slice() else { panic!("{:?}", documents.items) };
    let links: Vec<(&str, &str, Option<&Link>)> = facts.items.iter().map(|fact| (fact.label.as_str(), fact.value.as_str(), fact.link.as_ref())).collect();
    let view_link = |key: &str| Link::View { key: key.into() };
    assert_eq!(links, [
        ("Documento", "Library index design", Some(&view_link("document:design-r1"))),
        ("Plan propuesto", "Index the whole library", Some(&view_link("document:result-1.plan"))),
        ("docs/research/competitors.md", "How other apps index a library", Some(&view_link("document:result-1.file-1"))),
        ("docs/measurements.md", "Measurements", Some(&view_link("document:result-1.file-2"))),
    ]);
    let markdown_of = |key: &str| {
        let view = render(&state, &live, Client::Ios, Lang::Es, key, "");
        view.sections.iter().flat_map(|section| &section.items).find_map(|item|
            if let Item::Markdown(markdown) = item { Some(markdown.text.clone()) } else { None }).unwrap_or_else(|| panic!("{key} shows its text"))
    };
    assert_eq!(markdown_of("document:result-1.file-1"),
        "# How other apps index a library\n\n## Photos\n\nIndexes overnight while charging.\n\n## Lightroom\n\nIndexes on import.");
    assert!(markdown_of("document:result-1.plan").starts_with("# Index the whole library"));
    assert!(!markdown_of("document:result-1.plan").contains("10 000 photos"), "the plan's own file is a document of its own");
    assert!(markdown_of("document:result-1.file-2").contains("10 000 photos"));
    let missing = build("document:result-1.file-3", RenderContext {
        domain: &state, index: &crate::domain::ReadIndex::build(&state), live: &live, client: Client::Ios, lang: Lang::Es,
        transport: Transport::Loopback, cursor: "test", rendered_at: "",
    });
    assert_eq!(missing.err(), Some(BuildError::NotFound));
}

fn first_card_keys(view: &View) -> Vec<&str> {
    view.first_cards.iter().map(|card| card.key.as_str()).collect()
}

#[test]
fn home_and_sidebar_bring_the_first_three_cards_whole() {
    let state = golden_fixture();
    let live = golden_live();
    let at = "2026-09-30T10:00:00Z";
    for (root, client) in [("home", Client::Ios), ("sidebar", Client::Mac)] {
        for lang in [Lang::En, Lang::Es] {
            let view = render(&state, &live, client, lang, root, at);
            assert_eq!(first_card_keys(&view), [
                "card:a98d034b-249e-4487-a89b-57de38c26372",
                "card:8fa4034b-249e-4487-a89b-57de38c26372",
                "card:hf-weekly-2026-09-30",
            ], "{root} lists them in the queue's order");
            for card in &view.first_cards {
                assert_eq!(card, &render(&state, &live, client, lang, &card.key, at),
                    "{root}: {} is the view its own key answers", card.key);
            }
        }
    }
    let tui = render(&state, &live, Client::Tui, Lang::Es, "home", at);
    assert!(tui.first_cards.is_empty(), "the terminal's home already carries its detail");
}

#[test]
fn a_focused_home_brings_the_focused_card_first() {
    let state = golden_fixture();
    let view = render(&state, &golden_live(), Client::Ios, Lang::Es, "home?focus=f12d034b-249e-4487-a89b-57de38c26372", "");
    assert_eq!(first_card_keys(&view), [
        "card:f12d034b-249e-4487-a89b-57de38c26372",
        "card:cc2d034b-249e-4487-a89b-57de38c26372",
        "card:a98d034b-249e-4487-a89b-57de38c26372",
    ]);
}

#[test]
fn a_short_queue_brings_the_cards_it_has() {
    let mut state = golden_fixture();
    let live = golden_live();
    for id in ["8fa4034b-249e-4487-a89b-57de38c26372", "hf-weekly-2026-09-30", "f12d034b-249e-4487-a89b-57de38c26372",
        "cc2d034b-249e-4487-a89b-57de38c26372"] {
        state.interactions.remove(&id.into());
    }
    let one = render(&state, &live, Client::Ios, Lang::Es, "home", "");
    assert_eq!(first_card_keys(&one), ["card:a98d034b-249e-4487-a89b-57de38c26372"]);
    state.interactions.remove(&"a98d034b-249e-4487-a89b-57de38c26372".into());
    for (root, client) in [("home", Client::Ios), ("sidebar", Client::Mac)] {
        let none = render(&state, &live, client, Lang::Es, root, "");
        assert!(none.first_cards.is_empty(), "{root}");
        assert!(!serde_json::to_string(&none).unwrap().contains("first_cards"), "{root} leaves the field out");
    }
}

#[test]
fn home_changes_version_with_its_first_cards_only() {
    let state = golden_fixture();
    let live = golden_live();
    let home = |state: &DomainState| render(state, &live, Client::Ios, Lang::Es, "home", "").version;
    let before = home(&state);
    let mut third = state.clone();
    third.interactions.get_mut(&"hf-weekly-2026-09-30".into()).unwrap().card.need = "Review another candidate".into();
    assert_ne!(home(&third), before, "the third card changed, so the home that carries it did");
    let mut fourth = state.clone();
    fourth.interactions.get_mut(&"f12d034b-249e-4487-a89b-57de38c26372".into()).unwrap().card.need = "Read another notice".into();
    assert_ne!(render(&fourth, &live, Client::Ios, Lang::Es, "card:f12d034b-249e-4487-a89b-57de38c26372", "").version,
        render(&state, &live, Client::Ios, Lang::Es, "card:f12d034b-249e-4487-a89b-57de38c26372", "").version);
    assert_eq!(home(&fourth), before, "a card past the first three leaves home as it was");
}

#[test]
fn a_new_cursor_alone_leaves_home_unchanged() {
    let state = golden_fixture();
    let live = golden_live();
    let at = |cursor: &str| build("home", RenderContext {
        domain: &state, index: &crate::domain::ReadIndex::build(&state), live: &live, client: Client::Ios,
        lang: Lang::Es, transport: Transport::Loopback, cursor, rendered_at: "",
    }).unwrap();
    let (first, second) = (at("cursor-1"), at("cursor-2"));
    assert_eq!(first.first_cards[0].cursor, "cursor-1");
    assert_eq!(first.version, second.version, "the cursors inside the cards are not part of the version either");
}

/// 16 October: a node asked the orchestrator to create one task per phase of a migration; the
/// orchestrator created them and never answered, and the node stood still. A node that waits for
/// the orchestrator says so, and never puts its last words in a card for the user.
#[test]
fn a_task_whose_node_waits_for_the_orchestrator_says_so_and_puts_no_card_in_front_of_the_user() {
    use crate::domain::{Actor, CardKind, CardSpec, Command, LaunchPlan, NodeEvent, UNHEARD_PREFIX};
    let node = || Actor::Agent { agent_id: "a-1".into() };
    let mut state = DomainState::new("epoch");
    execute(&mut state, Actor::System, Command::RegisterAgent { agent_id: "a-1".into(), name: "Node One".into(), at: 0 });
    execute(&mut state, Actor::User { user_id: "u".into() }, Command::CreateRequest {
        request_id: "r-1".into(), body: "Resume the migration".into(), auto_merge: false, maintenance: false, message: false });
    execute(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None,
        task_id: "t-1".into(), request_id: "r-1".into(), title: "Resume the migration".into(), agent_id: Some("a-1".into()),
        review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")) });
    execute(&mut state, node(), Command::StartTask { task_id: "t-1".into() });
    execute(&mut state, Actor::System, Command::MoveNode { agent_id: "a-1".into(), event: NodeEvent::TurnStarted, at: 1 });
    execute(&mut state, node(), Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t-1".into() }, recipient: Recipient::Orchestrator,
        body: "Create one task per phase of the migration".into(), blocking: true,
        options: vec![QuestionOption { id: "done".into(), label: "Done".into(), description: None },
            QuestionOption { id: "no".into(), label: "No".into(), description: None }],
        card: CardSpec::new(CardKind::Decide, "Create the phase tasks").why("The plan needs them"),
    });
    execute(&mut state, Actor::System, Command::MoveNode { agent_id: "a-1".into(), event: NodeEvent::TurnEnded, at: 2 });
    assert_eq!(state.node_phase(&"a-1".into()), Some(&NodePhase::WaitingUser { in_turn: false }));
    assert!(!state.interactions.keys().any(|id| id.0.starts_with(UNHEARD_PREFIX)), "no card with the node's last words");
    assert_eq!(crate::views::inbox_badge(&crate::domain::ReadIndex::build(&state)), 0);
    let live = LiveSnapshot::default();
    assert_eq!(task_state_text(&state, &live, Lang::Es), "Esperando al orquestador");
    let detail = serde_json::to_string(&render(&state, &live, Client::Tui, Lang::Es, "task:t-1", "")).unwrap();
    assert!(detail.contains("Esperando al orquestador"), "{detail}");

    execute(&mut state, Actor::Orchestrator { generation: 0 }, Command::AnswerQuestion {
        attachments: vec![], interaction_id: "q".into(), option_id: "done".into(), inputs: BTreeMap::new() });
    assert!(!matches!(state.node_phase(&"a-1".into()), Some(NodePhase::WaitingUser { .. })), "the answer sends the node back to work");
    assert_ne!(task_state_text(&state, &live, Lang::Es), "Esperando al orquestador");
}

/// 15 October: the user chose 512 px prints on Redraft's phase B while phases C, D and E waited with
/// plans written for the 360 px copy. A dependent held for the orchestrator's review says so.
#[test]
fn a_dependent_held_by_the_users_decision_reads_plan_review_pending_until_it_is_settled() {
    use crate::domain::{Actor, CardKind, CardSpec, Command, LaunchPlan, PlanReviewOutcome};
    let mut state = DomainState::new("epoch");
    execute(&mut state, Actor::System, Command::RegisterAgent { agent_id: "a-b".into(), name: "Node B".into(), at: 0 });
    execute(&mut state, Actor::User { user_id: "u".into() }, Command::CreateRequest {
        request_id: "r-1".into(), body: "Redraft index".into(), auto_merge: false, maintenance: false, message: false });
    execute(&mut state, Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r-1".into(), title: "Redraft index".into() });
    for (task, title, agent, dependencies) in [("t-b", "Phase B", Some("a-b"), vec![]), ("t-1", "Phase C", None, vec!["t-b"])] {
        execute(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask {
            task_id: task.into(), request_id: "r-1".into(), title: title.into(), agent_id: agent.map(Into::into), review_prs: false,
            dependencies: dependencies.into_iter().map(Into::into).collect(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
            plan_items: None });
    }
    execute(&mut state, Actor::Agent { agent_id: "a-b".into() }, Command::StartTask { task_id: "t-b".into() });
    execute(&mut state, Actor::Agent { agent_id: "a-b".into() }, Command::AskQuestion {
        attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t-b".into() }, recipient: Recipient::User,
        body: "11 of 21 groups match".into(), blocking: true,
        options: vec![QuestionOption { id: "threshold".into(), label: "Adjust the threshold".into(), description: None },
            QuestionOption { id: "plan-512".into(), label: "512 px prints".into(), description: None }],
        card: CardSpec::new(CardKind::Decide, "Decide the index's next step").why("Phase C starts after it"),
    });
    let live = LiveSnapshot::default();
    assert_ne!(task_state_text(&state, &live, Lang::Es), "Revisión de plan pendiente");
    execute(&mut state, Actor::User { user_id: "u".into() }, Command::AnswerQuestion {
        attachments: vec![], interaction_id: "q".into(), option_id: "plan-512".into(), inputs: BTreeMap::new() });
    assert!(task_line(&state, &live, Lang::Es).starts_with("Revisión de plan pendiente · Decidiste en «Phase B»; el orquestador revisa su plan"));
    assert!(task_line(&state, &live, Lang::En).starts_with("Plan review pending · You decided on “Phase B”; the orchestrator reviews its plan"));
    execute(&mut state, Actor::Orchestrator { generation: 0 }, Command::SettlePlanReview {
        review_id: "q".into(), task_id: "t-1".into(), outcome: PlanReviewOutcome::Unchanged { reason: "Phase C does not use the prints".into() } });
    assert_ne!(task_state_text(&state, &live, Lang::Es), "Revisión de plan pendiente");
}

#[test]
fn a_node_that_runs_through_the_sdk_offers_pause_while_its_execution_works() {
    use crate::domain::{ExecutionIdentity, LifecycleRecord};
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation, task.agent_id) = (TaskState::Running, None, Some("a-1".into()));
    let mut live = pacing_live(vec![], vec![], None);
    live.nodes.push(LiveNode { agent_id: "a-1".into(), name: "Node One".into(), working: true, ..LiveNode::default() });
    assert!(find_action(&render(&state, &live, Client::Tui, Lang::En, "task:t-1", ""), "pause:a-1").is_some(), "the same node through claude --bg offers pause");
    state.lifecycle_journal.insert("t-1".into(), vec![
        LifecycleRecord::Planned { input: "input:t-1:1".into() },
        LifecycleRecord::Admitted {
            node: "a-1".into(),
            execution: Some(ExecutionIdentity { conversation: "conversation".into(), instance: "instance".into(), host: "alondra-sdk".into() }),
        },
    ]);
    for client in [Client::Tui, Client::Ios, Client::Mac] {
        let view = render(&state, &live, client, Lang::En, "task:t-1", "");
        assert!(!action_ids(&view).iter().any(|id| id.starts_with("pause:")), "{client:?} offers pause to an SDK node that runs nothing: {:?}", action_ids(&view));
    }
    state.lifecycle_journal.get_mut(&crate::domain::TaskId::from("t-1")).unwrap().extend([
        LifecycleRecord::ResourceAvailable,
        LifecycleRecord::TurnStarted { turn: "turn".into(), input: "input:t-1:1".into() },
    ]);
    for client in [Client::Tui, Client::Ios, Client::Mac] {
        let view = render(&state, &live, client, Lang::En, "task:t-1", "");
        assert!(find_action(&view, "pause:a-1").is_some(), "{client:?}: a working SDK node offers pause: {:?}", action_ids(&view));
    }
}

/// 20 October: the running task «t-alondrad-flaky-usage-sdk-owner-tests» offered only message,
/// priority and cancel, because its node ran through the SDK. Its own screen offers Pause while
/// its execution works and Resume once the user paused it, on every client.
#[test]
fn the_screen_of_a_task_whose_node_runs_through_the_sdk_offers_pause_or_resume_by_its_state() {
    use crate::domain::{ExecutionIdentity, LifecycleRecord};
    let admitted = vec![
        LifecycleRecord::Planned { input: "input:t-1:1".into() },
        LifecycleRecord::Admitted {
            node: "a-1".into(),
            execution: Some(ExecutionIdentity { conversation: "conversation".into(), instance: "instance".into(), host: "alondra-sdk".into() }),
        },
    ];
    let working = [admitted.clone(), vec![
        LifecycleRecord::ResourceAvailable,
        LifecycleRecord::TurnStarted { turn: "turn".into(), input: "input:t-1:1".into() },
    ]].concat();
    let by_user = Pause { reason: PauseReason::ByUser, since: 0, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
    let paused = LivePaused { task_id: "t-1".into(), session: Some("a-1".into()), at: "16:05".into(), reason: LivePauseReason::User,
        fine_minutes: None, next: false, waits_for: None, retry: None };
    let cases = [
        ("working", NodePhase::Working, TaskState::Running, working.clone(), vec![], Some("pause:a-1")),
        ("running nothing", NodePhase::Working, TaskState::Running, admitted, vec![], None),
        ("paused by the user", NodePhase::Paused(by_user), TaskState::Running, working.clone(), vec![paused], Some("resume:a-1")),
        ("done", NodePhase::Working, TaskState::Done, working, vec![], None),
    ];
    for (name, phase, task_state, journal, gate, offered) in cases {
        let mut state = fixture();
        node_in(&mut state, phase);
        state.tasks.get_mut(&"t-1".into()).unwrap().state = task_state;
        state.lifecycle_journal.insert("t-1".into(), journal);
        let live = pacing_live(vec![], gate, None);
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            let ids = action_ids(&render(&state, &live, client, Lang::Es, "task:t-1", ""));
            let shown: Vec<&String> = ids.iter().filter(|id| id.starts_with("pause:") || id.starts_with("resume:")).collect();
            match offered {
                Some(prefix) => assert!(shown.iter().all(|id| id.starts_with(prefix)) && !shown.is_empty(),
                    "{client:?}: a task {name} offers only {prefix}: {ids:?}"),
                None => assert!(shown.is_empty(), "{client:?}: a task {name} offers neither pause nor resume: {ids:?}"),
            }
        }
    }
}

/// t-parcel-open-issues-widget, 23 October: its node's phase still said working from its start
/// while its session had long rested on its delivery, so the task read «Answering you» for 12
/// hours. A task whose node runs through the SDK reads what its lifecycle does: working until it
/// presents its delivery, then delivered, whether its last turn still runs or has ended.
#[test]
fn the_task_of_a_node_of_the_sdk_reads_working_only_while_its_lifecycle_runs_something() {
    use crate::domain::{ExecutionIdentity, LifecycleRecord};
    let mut state = fixture();
    node_in(&mut state, NodePhase::Working);
    let answering = vec![
        LifecycleRecord::Planned { input: "input:t-1:1".into() },
        LifecycleRecord::Admitted {
            node: "a-1".into(),
            execution: Some(ExecutionIdentity { conversation: "conversation".into(), instance: "instance".into(), host: "alondra-sdk".into() }),
        },
        LifecycleRecord::ResourceAvailable,
        LifecycleRecord::TurnStarted { turn: "turn".into(), input: "input:t-1:1".into() },
    ];
    let live = LiveSnapshot::default();
    state.tasks.get_mut(&"t-1".into()).unwrap().final_presentation = Some("d".into());
    state.lifecycle_journal.insert("t-1".into(), answering.clone());
    assert_eq!(task_state_text(&state, &live, Lang::En), "Working", "its turn runs");
    let answering = [answering, vec![LifecycleRecord::Presented { delivery: "d".into() }]].concat();
    state.lifecycle_journal.insert("t-1".into(), answering.clone());
    assert_eq!(task_state_text(&state, &live, Lang::En), "Delivered", "its turn still runs, on its delivery");
    state.lifecycle_journal.insert("t-1".into(), [answering, vec![LifecycleRecord::TurnEnded { turn: "turn".into(), unheard_card: "unheard".into(), wake: None, text: None, awaits: None, stalled: false }]].concat());
    assert_eq!(task_state_text(&state, &live, Lang::En), "Delivered", "its session rests on the delivery");
    assert_eq!(task_state_text(&state, &live, Lang::Es), "Entregada");
}

#[test]
fn the_task_of_a_node_that_runs_through_the_sdk_offers_interrupt_but_no_close() {
    use crate::domain::{ExecutionIdentity, LifecycleRecord};
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&crate::domain::TaskId::from("t-1")).unwrap();
    (task.state, task.final_presentation, task.agent_id) = (TaskState::Running, None, Some("a-1".into()));
    let live = pacing_live(vec![], vec![], None);
    for client in [Client::Tui, Client::Ios] {
        assert!(!action_ids(&render(&state, &live, client, Lang::En, "task:t-1", "")).iter().any(|id| id.starts_with("close:")),
            "{client:?}: no node is closed by hand");
    }
    state.lifecycle_journal.insert("t-1".into(), vec![
        LifecycleRecord::Planned { input: "input:t-1:1".into() },
        LifecycleRecord::Admitted {
            node: "a-1".into(),
            execution: Some(ExecutionIdentity { conversation: "conversation".into(), instance: "instance".into(), host: "alondra-sdk".into() }),
        },
        LifecycleRecord::ResourceAvailable,
        LifecycleRecord::TurnStarted { turn: "turn".into(), input: "input:t-1:1".into() },
    ]);
    for client in [Client::Tui, Client::Ios, Client::Mac] {
        let ids = action_ids(&render(&state, &live, client, Lang::En, "task:t-1", ""));
        assert!(!ids.iter().any(|id| id.starts_with("close:")), "{client:?} offers to close an SDK node: {ids:?}");
        assert!(ids.iter().any(|id| id.starts_with("interrupt:a-1")), "{client:?}: a working SDK node offers Interrupt: {ids:?}");
    }
}

/// 22 October: «Parcel: widget de issues abiertos» ended its turn with a background command
/// running; once the command ended, the task waited for Claude to wake up with its notification,
/// which never came. Its screen offered only message, priority and cancel, and what it was told
/// stayed queued. Waiting for that wake-up, or for Claude to take what it was handed, offers
/// Interrupt and Pause on every client, as a turn does; a resting node offers neither.
#[test]
fn the_task_of_an_sdk_node_that_waits_for_its_background_work_or_its_input_offers_interrupt_and_pause() {
    use crate::domain::{ExecutionIdentity, LifecycleRecord};
    let handed = vec![
        LifecycleRecord::Planned { input: "input:t-1:1".into() },
        LifecycleRecord::Admitted {
            node: "a-1".into(),
            execution: Some(ExecutionIdentity { conversation: "conversation".into(), instance: "instance".into(), host: "alondra-sdk".into() }),
        },
        LifecycleRecord::ResourceAvailable,
    ];
    let background = [handed.clone(), vec![
        LifecycleRecord::TurnStarted { turn: "turn".into(), input: "input:t-1:1".into() },
        LifecycleRecord::WorkStarted { turn: "turn".into(), work: "claude-task:snapshots".into() },
        LifecycleRecord::TurnEnded { turn: "turn".into(), unheard_card: "turn-ended-t-1".into(), wake: None, text: None, awaits: None, stalled: false },
    ]].concat();
    let waking = [background.clone(), vec![
        LifecycleRecord::WorkEnded { work: "claude-task:snapshots".into(), continuation: "claude-notification:snapshots".into() },
    ]].concat();
    let resting = [waking.clone(), vec![
        LifecycleRecord::TurnResumed { turn: "woken".into(), continuation: "claude-notification:snapshots".into() },
        LifecycleRecord::TurnEnded { turn: "woken".into(), unheard_card: "turn-ended-t-1-woken".into(), wake: None, text: None, awaits: None, stalled: false },
    ]].concat();
    let live = pacing_live(vec![], vec![], None);
    for (name, journal, offered) in [
        ("handed what it was told", handed, true),
        ("running background work", background, true),
        ("waiting for its background work to wake it", waking, true),
        ("resting", resting, false),
    ] {
        let mut state = fixture();
        node_in(&mut state, NodePhase::Working);
        state.lifecycle_journal.insert("t-1".into(), journal);
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            let ids = action_ids(&render(&state, &live, client, Lang::Es, "task:t-1", ""));
            for prefix in ["interrupt:a-1", "pause:a-1"] {
                assert_eq!(ids.iter().any(|id| id.starts_with(prefix)), offered, "{client:?}: a node {name} offers {prefix} only while something runs: {ids:?}");
            }
        }
    }
}

// The phase `claude --bg` reports never moves for a node that runs through the SDK: the task reads its
// state from its task and execution, never «Starting» while its turn runs.
fn mcp_card(client: Client, lang: Lang) -> (DomainState, Card) {
    let state = crate::domain::mcp_wait_tests::waiting_for_linear();
    let card = state.mcp_waiting()[0].card.clone();
    let view = render(&state, &LiveSnapshot::default(), client, lang, &format!("card:{card}"), "");
    let Item::Card(card) = view.sections[0].items[0].clone() else { panic!("the MCP wait's card") };
    (state, card)
}

#[test]
fn the_mcp_sign_in_card_matches_goldens() {
    for (filename, client, lang, expected) in [
        ("card-mcp-sign-in.json", Client::Ios, Lang::En, include_str!("../../../../fixtures/views/cards/card-mcp-sign-in.json")),
        ("card-mcp-sign-in-es.json", Client::Ios, Lang::Es, include_str!("../../../../fixtures/views/cards/card-mcp-sign-in-es.json")),
        ("card-mcp-sign-in-mac.json", Client::Mac, Lang::En, include_str!("../../../../fixtures/views/cards/card-mcp-sign-in-mac.json")),
    ] {
        let state = crate::domain::mcp_wait_tests::waiting_for_linear();
        let card = state.mcp_waiting()[0].card.clone();
        let actual = render(&state, &LiveSnapshot::default(), client, lang, &format!("card:{card}"), "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

#[test]
fn one_card_signs_in_to_a_server_however_many_tasks_wait_for_it() {
    let (state, card) = mcp_card(Client::Tui, Lang::Es);
    assert_eq!(card.need.as_deref(), Some("Inicia sesión en linear: esperan 2 tareas"));
    assert!(card.why.as_deref().is_some_and(|why| why.contains("Conectar abre el inicio de sesión")
        && why.ends_with("Esperan: «Task t» y «Task u».")), "every waiting task is named: {:?}", card.why);
    let labels: Vec<&str> = card.actions.iter().map(|action| action.label.as_str()).collect();
    assert_eq!(labels, ["Conectar", "Seguir sin linear", "Reconectar"]);
    let wait = state.mcp_waiting()[0].card.0.clone();
    assert!(matches!(&card.actions[0].link, Some(Link::Local { local: LocalLink::McpSignIn { card, server } }) if *card == wait && server == "linear"));
    assert!(matches!(&card.actions[2].link, Some(Link::Local { local: LocalLink::McpReconnect { card } }) if *card == wait));
    assert_eq!(card.actions[1].option_id.as_deref(), Some(crate::domain::MCP_GO_ON_WITHOUT));
    assert_eq!(card.actions[1].stamp.as_deref(), Some("Las tareas siguen sin linear"));
    assert!(card.reply.is_none(), "nothing written on the card would send the tasks on");
    for client in [Client::Ios, Client::Mac] {
        let (_, app) = mcp_card(client, Lang::En);
        let labels: Vec<&str> = app.actions.iter().map(|action| action.label.as_str()).collect();
        assert_eq!(labels, ["Connect", "Go on without linear", "Reconnect"], "{client:?} signs in from the card");
        assert!(matches!(&app.actions[0].link, Some(Link::Local { local: LocalLink::McpSignIn { card, server } }) if *card == wait && server == "linear"));
    }
    let waiting: Vec<String> = inbox_items(&state).into_iter().filter(|item| item.pending).map(|item| item.id).collect();
    assert_eq!(waiting.iter().filter(|id| id.contains(crate::domain::MCP_WAIT_PREFIX)).count(), 1, "{waiting:?}");
    let task = render(&state, &LiveSnapshot::default(), Client::Tui, Lang::Es, "task:u", "");
    assert!(serde_json::to_string(&task).unwrap().contains("Espera a que inicies sesión en linear"));
}

const MCP_SERVERS_GOLDENS: [(&str, Client, Lang); 3] = [
    ("mcp-servers-ios.json", Client::Ios, Lang::En),
    ("mcp-servers-ios-es.json", Client::Ios, Lang::Es),
    ("mcp-servers-mac.json", Client::Mac, Lang::En),
];

/// Your servers: Linear needs a sign-in, Notion's sign-in is open, GitHub and a connector work.
/// Two projects with tasks: alondra's Sentry failed its last sign-in though one is still open and
/// its local files server works; quill's own database works. Lumo was checked without a task and
/// Blurcamera never was. Lumo was
/// checked without a node and Blurcamera never was.
fn mcp_servers_fixture() -> (DomainState, LiveSnapshot) {
    use crate::domain::{McpServerState, McpStatus, NodeMcp, OpenSignIn};
    let (mut state, mut live) = nodes_fixture();
    use alondra_foundation::{McpConnect, McpOrigin, McpScope, McpTraits};
    let server = |name: &str, status: McpStatus, transport: &str, source: &str, url: Option<&str>, error: Option<&str>| {
        let (scope, origin, connect) = match source {
            "claudeai" => (McpScope::User, McpOrigin::Connector, McpConnect::Connector),
            "user" => (McpScope::User, McpOrigin::UserSettings, McpConnect::None),
            "local" => (McpScope::Project, McpOrigin::LocalSettings, McpConnect::None),
            "plugin" => (McpScope::User, McpOrigin::Plugin, McpConnect::None),
            _ => (McpScope::Project, McpOrigin::ProjectSettings, McpConnect::None),
        };
        let remote = matches!(transport, "http" | "sse");
        let connect = if remote { McpConnect::SignIn } else { connect };
        McpServerState {
            name: name.into(), status, error: error.map(Into::into), source: Some(source.into()), transport: Some(transport.into()),
            url: url.map(Into::into), traits: McpTraits { scope, origin, connect, reconnect: true, sign_out: remote && source != "claudeai", remove: matches!(source, "user" | "local") },
        }
    };
    let linear = |status| server("linear", status, "http", "user", Some("https://mcp.linear.app/mcp"), None);
    state.mcp.insert("zeta-worker".into(), NodeMcp {
        generation: 1,
        servers: vec![
            linear(McpStatus::NeedsAuth),
            server("sentry", McpStatus::Failed, "http", "project", Some("https://mcp.sentry.dev/mcp"), Some("401 Unauthorized")),
            server("notion", McpStatus::NeedsAuth, "http", "user", Some("https://mcp.notion.com/mcp"), None),
            server("files", McpStatus::Connected, "stdio", "project", None, None),
            server("claude.ai Gmail", McpStatus::Connected, "claudeai-proxy", "claudeai", None, None),
            server(crate::domain::ALONDRA_MCP_SERVER, McpStatus::Connected, "stdio", "plugin", None, None),
        ],
        sign_ins: [
            ("sentry".to_owned(), OpenSignIn { url: "https://sentry.io/oauth/authorize".into(), callback_port: Some(49200), state: Some("s".into()) }),
            ("notion".to_owned(), OpenSignIn { url: "https://notion.so/oauth/authorize".into(), callback_port: Some(49201), state: Some("n".into()) }),
        ].into(),
        refused: BTreeMap::new(),
    });
    state.mcp.insert("queued".into(), NodeMcp { generation: 1, servers: vec![
        server("github", McpStatus::Connected, "http", "user", Some("https://api.githubcopilot.com/mcp"), None),
        server("db", McpStatus::Connected, "stdio", "local", None, None),
    ], ..NodeMcp::default() });
    for (task, project) in [("t-zeta-worker", "alondra"), ("t-queued", "quill")] {
        state.tasks.get_mut(&crate::domain::TaskId::from(task)).unwrap().project = Some(project.into());
    }
    live.projects = ["alondra", "quill", "lumo", "blurcamera"].into_iter().map(|name| crate::options::ProjectEntry {
        name: name.into(), path: format!("/Users/someone/Projects/{name}").into(), last_used: None, nodes: 0,
    }).collect();
    live.mcp_probes.insert("lumo".into(), LiveMcpProbe::Checked {
        at: "2026-09-30T09:40:00".into(),
        servers: vec![
            server("revenuecat", McpStatus::NeedsAuth, "http", "user", Some("https://mcp.revenuecat.ai/mcp"), None),
            server("supabase", McpStatus::NeedsAuth, "http", "project", Some("https://mcp.supabase.com/mcp"), None),
        ],
    });
    (state, live)
}

#[test]
fn mcp_servers_views_match_goldens() {
    let (state, live) = mcp_servers_fixture();
    let mut rendered = vec![];
    for (filename, client, lang) in MCP_SERVERS_GOLDENS {
        let expected = match filename {
            "mcp-servers-ios.json" => include_str!("../../../../fixtures/views/cards/mcp-servers-ios.json"),
            "mcp-servers-ios-es.json" => include_str!("../../../../fixtures/views/cards/mcp-servers-ios-es.json"),
            _ => include_str!("../../../../fixtures/views/cards/mcp-servers-mac.json"),
        };
        rendered.push((filename, render(&state, &live, client, lang, "mcp_servers", "2026-09-30T10:00:00Z"), expected));
    }
    for (filename, actual, expected) in rendered {
        assert_golden(filename, &actual, expected);
    }
}

#[test]
fn the_mcp_servers_screen_offers_what_each_server_needs_per_project() {
    use crate::domain::McpServerKey;
    let (state, live) = mcp_servers_fixture();
    let view = render(&state, &live, Client::Ios, Lang::Es, "mcp_servers", "");
    assert_eq!(view.title.as_deref(), Some("Servidores MCP"));
    assert_eq!(view.sections.iter().map(|section| (section.id.as_str(), section.title.as_deref().unwrap_or(""))).collect::<Vec<_>>(),
        [("yours", "Tus servidores"), ("project:alondra", "alondra"), ("project:quill", "quill"), ("idle_projects", "Otros proyectos")]);
    let rows = |section: &str| -> Vec<Row> {
        view.sections.iter().find(|s| s.id == section).unwrap().items.iter().filter_map(|item| match item { Item::Row(row) => Some(row.clone()), _ => None }).collect()
    };
    let titles = |section: &str| rows(section).into_iter().map(|row| row.title).collect::<Vec<_>>();
    let row = |section: &str, title: &str| rows(section).into_iter().find(|row| row.title == title).expect(title);
    let labels = |row: &Row| row.actions.iter().map(|a| a.label.clone()).collect::<Vec<_>>();
    let target = McpServerKey { name: "linear".into(), url: Some("https://mcp.linear.app/mcp".into()) }.target();

    assert_eq!(titles("yours"), ["claude.ai Gmail", "github", "linear", "notion", "revenuecat"],
        "the user's own servers once, whichever task or check reported them");
    assert_eq!(titles("project:alondra"), ["files", "sentry"], "a project lists only its own servers, never Alondra's");
    assert_eq!(titles("project:quill"), ["db"]);

    let linear = row("yours", "linear");
    assert_eq!(linear.badges[0].text, "Necesita iniciar sesión");
    assert_eq!(linear.subtitle.as_deref(), Some("Tu configuración\nLo usa «Build the importer»"), "where it comes from, then who uses it");
    assert_eq!(format!("{:?}", linear.actions[0].link),
        format!("Some(Local {{ local: McpSignIn {{ card: \"{target}\", server: \"linear\" }} }})"),
        "a server that needs a sign-in has one thing to do first: connect it");
    assert_eq!(labels(&linear), ["Conectar", "Borrar"], "one of the user's own settings can also be deleted");
    assert_eq!((row("yours", "notion").badges[0].text.clone(), labels(&row("yours", "notion"))), ("Iniciando sesión".to_owned(), vec!["Borrar".to_owned()]),
        "a sign-in in progress offers nothing else to do but deleting the server");
    let sentry = row("project:alondra", "sentry");
    assert_eq!(sentry.badges[0].text, "No se pudo iniciar sesión", "a failed attempt reads as failed, even with a sign-in open");
    assert_eq!(sentry.subtitle.as_deref(), Some("Configuración del proyecto · 401 Unauthorized\nLo usa «Build the importer»"));
    assert_eq!(labels(&sentry), ["Conectar"], "Connect tries again");
    assert_eq!(labels(&row("project:alondra", "files")), ["Reconectar"], "a server on this machine keeps no sign-in to forget");
    assert_eq!(labels(&row("yours", "claude.ai Gmail")), ["Reconectar"], "a connector is signed out on the provider's website");
    assert_eq!(row("yours", "claude.ai Gmail").subtitle.as_deref(), Some("Conector de tu cuenta\nLo usa «Build the importer»"));

    let github = row("yours", "github");
    assert_eq!((github.badges[0].text.as_str(), labels(&github)), ("Conectado", vec!["Reconectar".to_owned(), "Cerrar sesión".to_owned(), "Borrar".to_owned()]));
    let sign_out = github.actions.iter().find(|a| a.label == "Cerrar sesión").unwrap();
    let github_target = McpServerKey { name: "github".into(), url: Some("https://api.githubcopilot.com/mcp".into()) }.target();
    assert_eq!(sign_out.id, format!("mcp_sign_out:{github_target}@{}", state.revision));
    assert_eq!(sign_out.confirm.as_ref().map(|c| c.title.as_str()), Some("¿Cerrar sesión en github?"));
    let revenuecat = row("yours", "revenuecat");
    assert_eq!(labels(&revenuecat), ["Borrar"], "found by a check, with no task to act through: only deleting it from the user's settings");
    assert_eq!(revenuecat.subtitle.as_deref(), Some("Tu configuración\nAhora no lo usa ninguna tarea: podrás conectarlo cuando alguna lo use"),
        "a server that cannot be connected from here says why and what to do");
    assert!(view.sections.iter().all(|section| section.style == Some(SectionStyle::List)), "every section is a settings list");

    let others = view.sections.iter().find(|s| s.id == "idle_projects").unwrap();
    assert_eq!(others.title.as_deref(), Some("Otros proyectos"));
    assert!(matches!(&others.items[0], Item::Notice(notice) if notice.text == "Comprobar lee el estado de sus servidores sin empezar ninguna tarea."));
    let lumo = row("idle_projects", "lumo");
    assert_eq!(lumo.children.iter().map(|child| (child.title.as_str(), child.badges[0].text.as_str())).collect::<Vec<_>>(), [("supabase", "Necesita iniciar sesión")],
        "a checked project lists only its own servers");
    assert_eq!(lumo.actions.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(), ["Comprobar otra vez"]);
    let blurcamera = row("idle_projects", "blurcamera");
    assert_eq!(blurcamera.actions[0].id, format!("mcp_check:{}@{}", mcp_servers::project_target("blurcamera"), state.revision));
    assert!(rows("idle_projects").iter().all(|row| row.title != "alondra" && row.title != "quill"), "a project a task runs in is not checked");

    let mut checking = live.clone();
    checking.mcp_probes.insert("blurcamera".into(), LiveMcpProbe::Checking);
    let view = render(&state, &checking, Client::Ios, Lang::En, "mcp_servers", "");
    let Item::Row(blurcamera) = view.sections.last().unwrap().items.iter().find(|item| matches!(item, Item::Row(row) if row.title == "blurcamera")).unwrap() else { unreachable!() };
    assert_eq!((blurcamera.subtitle.as_deref(), blurcamera.actions.len()), (Some("Checking…"), 0), "no second check while one runs");

    let empty = render(&DomainState::new("test-epoch"), &LiveSnapshot::default(), Client::Tui, Lang::En, "mcp_servers", "");
    assert_eq!(empty.summary.as_deref(), Some("None in use"));
    assert_eq!(empty.empty.map(|notice| notice.text), Some("No task uses an MCP server now".into()));
}

#[test]
fn only_a_server_in_the_user_s_own_settings_offers_delete_and_says_what_it_deletes() {
    use crate::domain::McpServerKey;
    let (state, live) = mcp_servers_fixture();
    let view = render(&state, &live, Client::Ios, Lang::Es, "mcp_servers", "");
    let rows: Vec<&Row> = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item { Item::Row(row) => Some(row), _ => None }).collect();
    let deletes = |row: &&Row| row.actions.iter().find(|action| action.label == "Borrar").cloned();
    let offered: Vec<&str> = rows.iter().filter(|row| deletes(row).is_some()).map(|row| row.title.as_str()).collect();
    assert_eq!(offered, ["github", "linear", "notion", "revenuecat", "db"],
        "the user's and their project's own: never a repo's .mcp.json (files, sentry), a connector (Gmail) or a plugin");

    let github = deletes(&rows.iter().find(|row| row.title == "github").unwrap()).unwrap();
    let github_target = McpServerKey { name: "github".into(), url: Some("https://api.githubcopilot.com/mcp".into()) }.target();
    assert_eq!(github.id, format!("mcp_remove:{github_target}@{}", state.revision));
    assert_eq!((github.style, github.resolves_card, github.stamp.as_deref()), (Style::Destructive, true, Some("Borrado")),
        "red, and the row goes the moment it is confirmed");
    let confirm = github.confirm.unwrap();
    assert_eq!(confirm.title, "¿Borrar github?");
    assert_eq!(confirm.message, "Alondra lo borra de tu configuración de Claude Code, en todos tus proyectos. Las tareas que ya lo usan lo mantienen hasta que vuelvan a arrancar.");

    let db = deletes(&rows.iter().find(|row| row.title == "db").unwrap()).unwrap();
    let db_target = McpServerKey { name: "db".into(), url: None }.target();
    assert_eq!(db.id, format!("mcp_remove:{db_target}.{}@{}", mcp_servers::project_target("quill"), state.revision),
        "a project's own server names its project");
    assert_eq!(db.confirm.unwrap().message, "Alondra lo borra de tu configuración de Claude Code para quill. Las tareas que ya lo usan lo mantienen hasta que vuelvan a arrancar.");
    assert_eq!(view.summary.as_deref(), Some("9 servidores · 3 piden iniciar sesión"),
        "every server listed, a checked project's included, and those waiting for a sign-in");
    let english = render(&state, &live, Client::Mac, Lang::En, "mcp_servers", "");
    assert_eq!(english.summary.as_deref(), Some("9 servers · 3 need sign-in"));
}

#[test]
fn a_deleted_server_leaves_the_screen_until_a_node_that_starts_again_reports_it() {
    use crate::domain::McpServerKey;
    use crate::views::LiveMcpRemoval;
    let (mut state, mut live) = mcp_servers_fixture();
    let github = McpServerKey { name: "github".into(), url: Some("https://api.githubcopilot.com/mcp".into()) };
    live.mcp_removed.push(LiveMcpRemoval { key: github, loaded: [("queued".to_owned(), 1)].into() });
    let titles = |state: &DomainState, live: &LiveSnapshot| -> Vec<String> {
        let view = render(state, live, Client::Ios, Lang::En, "mcp_servers", "");
        view.sections.iter().find(|section| section.id == "yours").unwrap().items.iter()
            .filter_map(|item| match item { Item::Row(row) => Some(row.title.clone()), _ => None }).collect()
    };
    assert!(!titles(&state, &live).contains(&"github".to_owned()), "the node that still has it loaded no longer lists it");
    state.mcp.get_mut(&crate::domain::AgentId::from("queued")).unwrap().generation = 2;
    assert!(titles(&state, &live).contains(&"github".to_owned()), "its agent started again and still has it: it was added back");
}

const ACCOUNTS_GOLDENS: [(&str, Client, Lang); 4] = [
    ("accounts-ios.json", Client::Ios, Lang::En),
    ("accounts-ios-es.json", Client::Ios, Lang::Es),
    ("accounts-mac.json", Client::Mac, Lang::En),
    ("accounts-mac-es.json", Client::Mac, Lang::Es),
];

/// Four Claude accounts: the default one, signed in on Max and measured; Work, signed in on Max
/// 20x with a weight the user set, measured and running a node; Personal, signed out; and Old,
/// turned off. New work goes to the one with the most room.
fn accounts_fixture() -> (DomainState, LiveSnapshot) {
    use crate::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, Provider};
    use crate::usage::{AccountUsage, Share, UsageWindow, WindowKind, WindowReading};
    let (mut state, live) = (golden_fixture(), golden_live());
    let account = |id: &str, name: &str, state, email: Option<&str>, plan: Option<&str>, weight| AccountStatus {
        provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: name.into(), state, native_identity: None, email: email.map(Into::into),
        plan: plan.map(Into::into), billing: AccountBilling::Subscription { plan: None, weight }, sign_ins: 0,
    };
    state.accounts = vec![
        account("default", "Default", AccountState::SignedIn, Some("someone@example.org"), Some("max"), None),
        account("work", "Work", AccountState::SignedIn, Some("someone@work.example"), Some("max_20x"), Some(8)),
        account("personal", "Personal", AccountState::SignedOut, None, None, None),
        account("old", "Old", AccountState::Disabled, None, None, None),
    ];
    let window = |kind, used: f64| UsageWindow {
        reading: WindowReading { kind, used: Share::of(used), resets_at: None, exhausted: false }, measured_at: 1_759_225_000_000,
    };
    let usage = |id: &str, session: f64, week: f64, plan: &str| AccountUsage {
        provider: Provider::Claude, account: AccountId::parse(id).unwrap(),
        windows: vec![window(WindowKind::Session, session), window(WindowKind::Weekly, week)], plan: Some(plan.into()), day_start: None,
    };
    state.account_usage = vec![usage("default", 0.12, 0.34, "max"), usage("work", 0.61, 0.48, "max_20x")];
    let running = state.tasks.get_mut(&"t-651c38484cd2f34e74d5107ba05f7f15".into()).unwrap();
    running.agent_id = Some("d887a34b-249e-4487-a89b-57de38c26372".into());
    running.account = Some(AccountId::parse("work").unwrap());
    (state, live)
}

#[test]
fn accounts_views_match_goldens() {
    let (state, live) = accounts_fixture();
    if let Ok(dir) = std::env::var("ALONDRA_GOLDEN_OUT") {
        for (filename, client, lang) in ACCOUNTS_GOLDENS {
            let view = render(&state, &live, client, lang, "accounts", "2026-09-30T10:00:00Z");
            std::fs::write(format!("{dir}/{filename}"), format!("{}\n", serde_json::to_string_pretty(&view).unwrap())).unwrap();
        }
    }
    for (filename, client, lang) in ACCOUNTS_GOLDENS {
        let expected = match filename {
            "accounts-ios.json" => include_str!("../../../../fixtures/views/cards/accounts-ios.json"),
            "accounts-ios-es.json" => include_str!("../../../../fixtures/views/cards/accounts-ios-es.json"),
            "accounts-mac.json" => include_str!("../../../../fixtures/views/cards/accounts-mac.json"),
            _ => include_str!("../../../../fixtures/views/cards/accounts-mac-es.json"),
        };
        let actual = render(&state, &live, client, lang, "accounts", "2026-09-30T10:00:00Z");
        assert_golden(filename, &actual, expected);
    }
}

#[test]
fn the_accounts_screen_shows_each_account_and_offers_only_what_its_rules_allow() {
    let (state, live) = accounts_fixture();
    let view = render(&state, &live, Client::Tui, Lang::En, "accounts", "");
    assert_eq!(view.title.as_deref(), Some("Accounts"));
    assert!(!serde_json::to_string(&view).unwrap().contains("/accounts/"), "a screen never shows a folder");
    let rows: Vec<Row> = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        Item::Row(row) => Some(row.clone()),
        _ => None,
    }).collect();
    let row = |title: &str| rows.iter().find(|row| row.title == title).unwrap_or_else(|| panic!("{title}")).clone();
    let labels = |row: &Row| row.actions.iter().map(|action| action.label.clone()).collect::<Vec<_>>();

    let default = row("Default");
    assert_eq!(default.subtitle.as_deref(), Some("Claude · someone@example.org · max · weight 5, by plan\n5 h 12 % · week 34 %"));
    assert_eq!(default.badges[0].text, "Signed in");
    assert_eq!(labels(&default), ["Weight", "Move down", "Turn off", "Rename", "Refresh models"], "the Mac's own sign-in is never removed, and it is first");

    let work = row("Work");
    assert_eq!(work.subtitle.as_deref(), Some("Claude · someone@work.example · max_20x · weight 8, set by you\n5 h 61 % · week 48 % · 1 node"));
    assert_eq!(labels(&work), ["Weight", "Move up", "Move down", "Turn off", "Rename", "Refresh models", "Remove"]);
    let remove = work.actions.iter().find(|action| action.label == "Remove").unwrap();
    assert!(!remove.enabled, "a node runs on it");
    assert_eq!(remove.disabled_reason.as_deref(), Some("Open tasks still run on Work: remove it once they are done."));
    let weights: Vec<&str> = work.actions[0].options.iter().map(|option| option.id.as_str()).collect();
    assert_eq!(weights, ["auto", "1", "2", "3", "5", "10", "15", "20"], "its own weight is not offered again");

    let personal = row("Personal");
    assert_eq!(personal.badges[0].text, "Signed out");
    assert_eq!(labels(&personal)[0], "Sign in");
    assert!(matches!(&personal.actions[0].link, Some(Link::Local { local: LocalLink::AccountSignIn { account, command, .. } })
        if account == "Personal" && command == "alondra accounts login 'personal'"));
    assert!(labels(&personal).contains(&"Remove".to_owned()));

    let old = row("Old");
    assert_eq!(old.badges[0].text, "Off");
    assert_eq!(labels(&old), ["Weight", "Move up", "Turn on", "Rename", "Refresh models", "Remove"], "the last can't move down");

    let add = row("Add a Claude account");
    assert!(matches!(&add.actions[0].link, Some(Link::Local { local: LocalLink::AccountAdd { command, .. } }) if command == "alondra accounts add"));
    let policy = row("Most room first");
    assert_eq!(labels(&policy), ["Change"]);
    assert_eq!(policy.actions[0].options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>(), ["chain"], "only the other way is offered");
    let moves = row("Move a node when its account runs out");
    assert_eq!(moves.badges[0].text, "Off", "off unless the user turns it on");
    assert_eq!(moves.subtitle.as_deref(), Some("Continue with the next eligible account in your order when quota runs out"));
    assert_eq!((labels(&moves), moves.actions[0].option_id.as_deref()), (vec!["Turn on".to_owned()], Some("on")));
    assert_eq!(crate::views::accounts::shell_word("Bob's"), r"'Bob'\''s'");
}

#[test]
fn the_merge_button_says_the_method_set_in_settings_and_merges_with_one_press() {
    use crate::domain::{Actor, Command, MergeMethod};
    let live = LiveSnapshot::default();
    let mut state = presented_pr();
    state.pull_requests.get_mut(&"pr".into()).unwrap().allowed_methods = vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase];
    let merge_of = |state: &DomainState, client, lang| {
        let view = render(state, &live, client, lang, "card:d", "");
        let card = card_of(&view).clone();
        let merge = card.actions.iter().find(|action| action.id.starts_with("merge:")).cloned().expect("the card offers the merge");
        let others: Vec<(String, Option<String>)> = card.more.iter().filter(|action| action.id.starts_with("merge:"))
            .map(|action| (action.label.clone(), action.option_id.clone())).collect();
        (merge, others)
    };
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let (merge, others) = merge_of(&state, client, Lang::En);
        assert_eq!((merge.label.as_str(), merge.option_id.as_deref()), ("Squash and merge", Some("squash")), "{client:?}");
        assert_eq!(merge.presentation, Presentation::Button, "{client:?}: no menu");
        assert!(merge.options.is_empty() && merge.confirm.is_none() && merge.description.is_none(), "{client:?}: {merge:?}");
        assert_eq!(others, [("Merge with a merge commit".to_owned(), Some("merge".to_owned())), ("Merge with rebase".to_owned(), Some("rebase".to_owned()))],
            "{client:?}: the other ways are under More");
    }
    for (method, english, spanish) in [
        (MergeMethod::Merge, "Merge", "Hacer merge"),
        (MergeMethod::Rebase, "Rebase and merge", "Rebase and merge"),
        (MergeMethod::Squash, "Squash and merge", "Squash and merge"),
    ] {
        execute(&mut state, Actor::User { user_id: "u".into() }, Command::SetMergeMethod { method });
        let (merge, _) = merge_of(&state, Client::Tui, Lang::En);
        assert_eq!((merge.label.as_str(), merge.option_id.as_deref()), (english, Some(method.id())), "the open card follows the setting");
        assert_eq!(merge_of(&state, Client::Ios, Lang::Es).0.label, spanish);
    }
    state.pull_requests.get_mut(&"pr".into()).unwrap().allowed_methods = vec![MergeMethod::Merge, MergeMethod::Rebase];
    let (merge, others) = merge_of(&state, Client::Ios, Lang::Es);
    assert_eq!((merge.label.as_str(), merge.option_id.as_deref()), ("Hacer merge", Some("merge")));
    assert_eq!(merge.description.as_deref(), Some("Este repositorio no permite squash: se hará con merge commit"));
    assert_eq!(others, [("Hacer merge con rebase".to_owned(), Some("rebase".to_owned()))]);
    let english = merge_of(&state, Client::Mac, Lang::En).0;
    assert_eq!(english.description.as_deref(), Some("This repository does not allow squash: it merges with a merge commit"));
}

#[test]
fn the_merge_method_is_picked_in_this_macs_settings_above_the_node_limit() {
    use crate::domain::MergeMethod;
    let mut state = fixture();
    let machine = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, "machine", "");
    let settings = machine.sections.iter().find(|section| section.id == "settings").expect("this Mac's settings");
    let Item::Row(row) = &settings.items[0] else { panic!("{settings:?}") };
    assert_eq!((row.id.as_str(), row.title.as_str(), row.subtitle.as_deref()),
        ("merge_method", "Merge de las PR", Some("Squash and merge · si el repositorio lo permite")));
    let pick = find_action(&machine, "set_merge_method:").expect("the merge method is changed from this Mac's settings");
    assert_eq!(pick.presentation, Presentation::Menu);
    let options: Vec<(&str, &str, Option<&str>)> = pick.options.iter()
        .map(|option| (option.id.as_str(), option.label.as_str(), option.description.as_deref())).collect();
    assert_eq!(options, [
        ("squash", "Squash and merge · ahora", Some("Un solo commit en la rama base")),
        ("merge", "Merge", Some("Conserva todos los commits")),
        ("rebase", "Rebase and merge", Some("Los commits se reaplican sobre la rama base")),
    ]);
    state.merge_method = MergeMethod::Rebase;
    let live = pacing_live(vec![], vec![], None);
    let machine = render(&state, &live, Client::Tui, Lang::En, "machine", "");
    let settings = machine.sections.iter().find(|section| section.id == "settings").unwrap();
    let rows: Vec<(&str, Option<&str>)> = settings.items.iter().filter_map(|item| match item {
        Item::Row(row) => Some((row.id.as_str(), row.subtitle.as_deref())),
        _ => None,
    }).collect();
    assert_eq!(rows, [("merge_method", Some("Rebase and merge · if the repository allows it")), ("node_limit", Some("Limit: automatic (6)"))]);
    let pick = find_action(&machine, "set_merge_method:").unwrap();
    assert_eq!(pick.options[2].label, "Rebase and merge · now");
}

/// Every state something a node runs in the background can be in: running, or ended with one of
/// the outcomes Claude reports. There is no other.
fn run_states() -> Vec<RunState> {
    std::iter::once(RunState::Running).chain(RunOutcome::ALL.map(|outcome| RunState::Ended { outcome, ran_secs: 90 })).collect()
}

/// Every phase of the node machine, each once; the match fails to compile when a phase is added.
fn node_phases() -> Vec<NodePhase> {
    let work = Background { work: crate::model::BackgroundKind::Shell, label: "npm run dev".into(), since: 0, others: 0, gone: None };
    let pause = Pause { reason: PauseReason::Memory, since: 0, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false };
    let phases = vec![
        NodePhase::Starting { reviving: false, idle_since: None }, NodePhase::Lost, NodePhase::Working, NodePhase::WaitingBackground(work),
        NodePhase::WaitingUser { in_turn: false }, NodePhase::Paused(pause), NodePhase::Delivered { in_turn: false },
        NodePhase::Resuming { idle_since: None, after_wait: false }, NodePhase::WaitingDependency, NodePhase::NoTask, NodePhase::Closing, NodePhase::Closed,
        NodePhase::ThroughSdk,
    ];
    for phase in &phases {
        match phase {
            NodePhase::Starting { .. } | NodePhase::Lost | NodePhase::Working | NodePhase::WaitingBackground(_) | NodePhase::WaitingUser { .. }
            | NodePhase::Paused(_) | NodePhase::Delivered { .. } | NodePhase::Resuming { .. } | NodePhase::WaitingDependency | NodePhase::NoTask
            | NodePhase::Closing | NodePhase::Closed | NodePhase::ThroughSdk => {}
        }
    }
    phases
}

/// Every task state, each once; the match fails to compile when one is added.
fn task_states() -> Vec<TaskState> {
    let states = vec![TaskState::NotStarted, TaskState::Running, TaskState::NeedsYou, TaskState::AwaitingBlocker, TaskState::ForYourReview,
        TaskState::WaitingDependency, TaskState::Closing, TaskState::Done, TaskState::Cancelled];
    for state in &states {
        match state {
            TaskState::NotStarted | TaskState::Running | TaskState::NeedsYou | TaskState::AwaitingBlocker | TaskState::ForYourReview
            | TaskState::WaitingDependency | TaskState::Closing | TaskState::Done | TaskState::Cancelled => {}
        }
    }
    states
}

fn node_running_in(state: RunState) -> LiveNode {
    let mut node = node_on_its_task();
    node.steps = working_node().steps;
    node.subagents = working_node().subagents.into_iter().map(|run| LiveRun { state, ..run }).collect();
    node.workflows = working_node().workflows.into_iter().map(|run| LiveRun { state, ..run }).collect();
    node.background = node.background.into_iter().map(|item| LiveBackground { state, ..item }).collect();
    node
}

/// #1166 marked work running for 30 min with nothing else shown «¿colgado?». What a node runs in
/// the background now shows only its state, on every screen and client, in both languages:
/// running, or how it ended. A state has one badge, and no state has a warning.
#[test]
fn background_work_shows_only_that_it_runs_or_how_it_ended() {
    let mut state = fixture();
    node_in(&mut state, NodePhase::Working);
    let mut shown: Vec<(RunState, Lang, String)> = vec![];
    for run in run_states() {
        let live = LiveSnapshot { nodes: vec![node_running_in(run)], ..Default::default() };
        for lang in [Lang::En, Lang::Es] {
            let mut texts = vec![];
            for client in [Client::Ios, Client::Mac, Client::Tui] {
                for key in ["task:t-1"] {
                    let view = render(&state, &live, client, lang, key, "");
                    let rows: Vec<&Row> = ["subagents", "workflows", "background"].iter().flat_map(|id| node_work_rows(&view, id)).collect();
                    assert!(!rows.is_empty(), "{client:?} {key} shows the background work");
                    for row in rows {
                        let [badge] = row.badges.as_slice() else { panic!("{client:?} {key} {run:?}: one state, one badge: {:?}", row.badges) };
                        assert_ne!(badge.tone, Tone::Warning, "{client:?} {key} {run:?}: no state of background work is a warning");
                        texts.push(badge.text.clone());
                    }
                }
            }
            texts.dedup();
            let [text] = texts.as_slice() else { panic!("{run:?} {lang:?}: every row and screen in the same state reads the same: {texts:?}") };
            shown.push((run, lang, text.clone()));
        }
    }
    let read = |lang| shown.iter().filter(|(_, at, _)| *at == lang).map(|(run, _, text)| (*run, text.as_str())).collect::<Vec<_>>();
    let [running, finished, failed, stopped]: [RunState; 4] = run_states().try_into().unwrap();
    assert_eq!(read(Lang::En), [(running, "Running"), (finished, "Finished"), (failed, "Failed"), (stopped, "Stopped")]);
    assert_eq!(read(Lang::Es), [(running, "En marcha"), (finished, "Terminado"), (failed, "Falló"), (stopped, "Detenido")]);
}

/// A screen changes only when a fact does: the same domain and the same live readings render the
/// same on every screen two days later, for every phase of the node, every task state, every
/// state of its background work and every state of a task of the Agent SDK.
#[test]
fn no_screen_changes_with_the_clock_alone() {
    let screens = |state: &DomainState, live: &LiveSnapshot, at: &str, in_state: Option<&alondra_domain::LiveState>| -> Vec<serde_json::Value> {
        let mut out = vec![];
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            for lang in [Lang::En, Lang::Es] {
                for key in ["home", "tasks", "sidebar", "task:t-1", "request:r-1"] {
                    if (key == "sidebar" && client != Client::Mac) || (key == "home" && client == Client::Mac) {
                        continue;
                    }
                    let mut view = serde_json::to_value(render_in(state, live, client, lang, key, at, in_state)).unwrap();
                    view.as_object_mut().unwrap().remove("rendered_at");
                    out.push(view);
                }
            }
        }
        out
    };
    for (index, phase) in node_phases().into_iter().enumerate() {
        for task in task_states() {
            let mut state = fixture();
            node_in(&mut state, phase.clone());
            state.tasks.get_mut(&"t-1".into()).unwrap().state = task;
            let runs = run_states();
            let live = LiveSnapshot { nodes: vec![node_running_in(runs[index % runs.len()])], ..Default::default() };
            assert_eq!(screens(&state, &live, "2026-09-30T10:00:00Z", None), screens(&state, &live, "2026-10-02T10:00:00Z", None),
                "{phase:?} with its task {task:?} reads the same two days later");
        }
    }
    let state = sdk_task();
    let live = LiveSnapshot { nodes: vec![node_running_in(RunState::Running)], ..Default::default() };
    for in_state in live_states() {
        assert_eq!(screens(&state, &live, "2026-09-30T10:00:00Z", Some(&in_state)), screens(&state, &live, "2026-10-02T10:00:00Z", Some(&in_state)),
            "a task of the Agent SDK in {in_state:?} reads the same two days later");
    }
}

/// Every state a live task can be in, each once; the match fails to compile when one is added.
fn live_states() -> Vec<alondra_domain::LiveState> {
    use alondra_domain::{Capacity, LiveState, Several, Step, Worker};
    let states = vec![
        LiveState::Working(Worker::Turn), LiveState::Working(Worker::OwnWork), LiveState::Working(Worker::AfterMerge),
        LiveState::Working(Worker::Alondra(Step::Starting)), LiveState::Working(Worker::Alondra(Step::Delivering)),
        LiveState::Working(Worker::Alondra(Step::Waking)), LiveState::Working(Worker::Alondra(Step::Answering)),
        LiveState::Working(Worker::Alondra(Step::Merging)), LiveState::Working(Worker::Alondra(Step::Finishing)),
        LiveState::WaitingUser(Several::new([crate::domain::InteractionId::from("delivery-t-1")]).unwrap()),
        LiveState::WaitingUser(Several::new([crate::domain::InteractionId::from("claude-signed-out-t-1")]).unwrap()),
        LiveState::WaitingUser(Several::new([crate::domain::InteractionId::from(format!("{}user-0-t-1", crate::domain::NODE_PAUSE_PREFIX))]).unwrap()),
        LiveState::WaitingCapacity(Capacity::Gate), LiveState::WaitingCapacity(Capacity::Machine), LiveState::WaitingCapacity(Capacity::Disk),
        LiveState::WaitingCapacity(Capacity::Usage), LiveState::WaitingCapacity(Capacity::ClaudeApi),
        LiveState::WaitingTask(Several::new([crate::domain::TaskId::from("t-2")]).unwrap()),
    ];
    for state in &states {
        match state {
            LiveState::Working(Worker::Turn | Worker::OwnWork | Worker::AfterMerge
                | Worker::Alondra(Step::Starting | Step::Delivering | Step::Waking | Step::Answering | Step::Merging | Step::Finishing))
            | LiveState::WaitingUser(_) | LiveState::WaitingCapacity(Capacity::Gate | Capacity::Machine | Capacity::Disk | Capacity::Usage | Capacity::ClaudeApi)
            | LiveState::WaitingTask(_) => {}
        }
    }
    states
}

/// Task t-1 whose node runs a turn through the Agent SDK, with `delivery-t-1` as its delivery.
fn sdk_task() -> DomainState {
    use crate::domain::{ExecutionIdentity, LifecycleRecord};
    let mut state = fixture();
    node_in(&mut state, NodePhase::Starting { reviving: false, idle_since: None });
    state.tasks.get_mut(&"t-1".into()).unwrap().final_presentation = Some("delivery-t-1".into());
    state.lifecycle_journal.insert("t-1".into(), vec![
        LifecycleRecord::Planned { input: "input:t-1:1".into() },
        LifecycleRecord::Admitted {
            node: "a-1".into(),
            execution: Some(ExecutionIdentity { conversation: "conversation".into(), instance: "instance".into(), host: "alondra-sdk".into() }),
        },
        LifecycleRecord::ResourceAvailable,
        LifecycleRecord::TurnStarted { turn: "turn".into(), input: "input:t-1:1".into() },
    ]);
    state
}

/// The phase `claude --bg` reports never moves for a node of the Agent SDK, so its task reads only
/// its lifecycle's state: each state reads the same on the phone, the Mac and the terminal, on
/// the list and on the task's screen, in each language, and none says its node closed, that it
/// comes back or that it is still starting.
#[test]
fn each_live_state_reads_one_way_on_every_client() {
    let state = sdk_task();
    let live = LiveSnapshot::default();
    let mut shown = vec![];
    for in_state in live_states() {
        for lang in [Lang::En, Lang::Es] {
            let mut texts = vec![];
            for client in [Client::Ios, Client::Mac, Client::Tui] {
                let list = render_in(&state, &live, client, lang, "tasks", "", Some(&in_state));
                let row = rows_with_parents(&list).into_iter().map(|(_, row)| row).find(|row| row.id == "t-1").unwrap_or_else(|| panic!("{client:?} lists t-1"));
                let [badge] = row.badges.as_slice() else { panic!("{client:?} {in_state:?}: one state, one badge: {:?}", row.badges) };
                let screen = serde_json::to_string(&render_in(&state, &live, client, lang, "task:t-1", "", Some(&in_state))).unwrap();
                assert!(screen.contains(&badge.text), "{client:?} {in_state:?}: the task's screen says {:?}: {screen}", badge.text);
                texts.push(badge.text.clone());
            }
            texts.dedup();
            let [text] = texts.as_slice() else { panic!("{in_state:?} {lang:?}: every client reads the same: {texts:?}") };
            for gone in ["Stopped", "Detenida", "Starting", "Arrancando", "Reconnecting", "Reconectando", "Waiting for the orchestrator", "Esperando al orquestador"] {
                assert_ne!(text, gone, "{in_state:?} {lang:?}");
            }
            shown.push((lang, text.clone()));
        }
    }
    let read = |lang| shown.iter().filter(|(at, _)| *at == lang).map(|(_, text)| text.as_str()).collect::<Vec<_>>();
    assert_eq!(read(Lang::En), ["Working", "In the background", "Finishing after the merge", "Working", "Working", "Working", "Working", "Closing", "Closing",
        "Delivered", "Waiting for an answer", "Paused", "Waiting for the machine", "Waiting for the machine", "Waiting for capacity", "Waiting for Claude's usage limit",
        "Waiting for the Claude API", "Waiting"]);
    assert_eq!(read(Lang::Es), ["Trabajando", "En segundo plano", "Terminando tras el merge", "Trabajando", "Trabajando", "Trabajando", "Trabajando", "Cerrando", "Cerrando",
        "Entregada", "Esperando respuesta", "Pausada", "Esperando máquina", "Esperando máquina", "Esperando capacidad", "Esperando al límite de uso de Claude",
        "Esperando a la API de Claude", "Esperando"]);
}

/// On 23 October a node ended its turn behind `sleep 5900`, then behind a workflow, and its task
/// read «Working» for 26 minutes with nothing to explain it. A task whose node's turn ended behind
/// work of its own says it waits for that work and names what still runs (a command, a Monitor, a
/// workflow, a subagent), on every client and in both languages, through the Agent SDK and through
/// `claude --bg`; what already ended is not named.
#[test]
fn a_task_whose_turn_ended_behind_its_own_work_says_so_and_names_what_runs() {
    use alondra_domain::{LiveState, Worker};
    let ended = RunState::Ended { outcome: RunOutcome::Finished, ran_secs: 3 };
    let item = |kind: &str, label: &str, since: &str, state| LiveBackground { kind: kind.into(), label: label.into(), since: since.into(), state };
    let run = |label: &str, since: &str, state| LiveRun { label: label.into(), since: since.into(), state, ..Default::default() };
    let node = |background, workflows, subagents| LiveNode { background, workflows, subagents, steps: vec![], background_badge: None, ..node_on_its_task() };
    let sleep = || item("shell", "sleep 5900", "2026-10-23T08:27:00Z", RunState::Running);
    let workflow = |state| run("Mide con git y PRs la velocidad", "2026-10-23T08:54:00Z", state);
    let subagent = |state| run("Explore the parser", "2026-10-23T08:55:00Z", state);
    let cases: Vec<(LiveNode, &str, &str)> = vec![
        (node(vec![item("shell", "npm test", "2026-10-23T08:00:00Z", ended), sleep()], vec![], vec![]), "sleep 5900", "sleep 5900"),
        (node(vec![item("other", "Monitor: tail -f build.log", "2026-10-23T08:30:00Z", RunState::Running)], vec![], vec![]),
            "Monitor: tail -f build.log", "Monitor: tail -f build.log"),
        (node(vec![], vec![workflow(RunState::Running)], vec![subagent(ended)]), "Mide con git y PRs la velocidad", "Mide con git y PRs la velocidad"),
        (node(vec![], vec![], vec![subagent(RunState::Running)]), "Explore the parser", "Explore the parser"),
        (node(vec![sleep()], vec![workflow(RunState::Running)], vec![subagent(RunState::Running)]), "sleep 5900 and 2 more", "sleep 5900 y 2 más"),
    ];
    let stored = Background { work: crate::model::BackgroundKind::Shell, label: "sleep 5900".into(), since: 0, others: 1, gone: None };
    let mut by_bg = fixture();
    node_in(&mut by_bg, NodePhase::WaitingBackground(stored));
    let sdk = sdk_task();
    let own_work = LiveState::Working(Worker::OwnWork);
    let mut runs: Vec<(&str, &DomainState, Option<&LiveState>, LiveSnapshot, Option<(&str, &str)>)> = vec![];
    for (node, en, es) in &cases {
        let live = LiveSnapshot { nodes: vec![node.clone()], ..Default::default() };
        runs.push(("claude --bg", &by_bg, None, live.clone(), Some((*en, *es))));
        runs.push(("the Agent SDK", &sdk, Some(&own_work), live, Some((*en, *es))));
    }
    runs.push(("claude --bg with no readings", &by_bg, None, LiveSnapshot::default(), Some(("sleep 5900 and 1 more", "sleep 5900 y 1 más"))));
    runs.push(("the Agent SDK with no readings", &sdk, Some(&own_work), LiveSnapshot::default(), None));
    for (through, state, in_state, live, named) in &runs {
        for (lang, waits) in [(Lang::En, "In the background"), (Lang::Es, "En segundo plano")] {
            let named = named.map(|(en, es)| if lang == Lang::En { en } else { es });
            for client in [Client::Ios, Client::Mac, Client::Tui] {
                let list = render_in(state, live, client, lang, "tasks", "", *in_state);
                let row = rows_with_parents(&list).into_iter().map(|(_, row)| row).find(|row| row.id == "t-1").unwrap_or_else(|| panic!("{client:?} lists t-1"));
                let badges: Vec<&str> = row.badges.iter().map(|badge| badge.text.as_str()).collect();
                assert_eq!(badges, [waits], "{through} {client:?} {lang:?}");
                let shown = |view: &View| serde_json::to_string(view).unwrap();
                let (list, screen) = (shown(&list), shown(&render_in(state, live, client, lang, "task:t-1", "", *in_state)));
                assert!(screen.contains(waits), "{through} {client:?} {lang:?}: the task's screen says {waits}: {screen}");
                match named {
                    Some(named) => {
                        assert!(list.contains(&format!("\"{named}\"")), "{through} {client:?} {lang:?}: the list names {named}: {list}");
                        assert!(screen.contains(&format!("\"{waits} · {named}\"")), "{through} {client:?} {lang:?}: the screen names {named}: {screen}");
                    }
                    None => assert!(!list.contains("sleep 5900"), "{through} {client:?} {lang:?}: nothing is named without readings: {list}"),
                }
                for ended in ["npm test", "Explore the parser and", "Explore the parser y"] {
                    assert!(!list.contains(ended), "{through} {client:?} {lang:?}: {ended} ended and is not named: {list}");
                }
            }
        }
    }
}

/// Whatever phase `claude --bg` last left on a node of the Agent SDK (starting, lost, closed…), its
/// task whose turn runs reads «Working» on every client: never «Stopped», coming back or starting.
#[test]
fn a_task_of_the_sdk_whose_turn_runs_reads_working_whatever_its_nodes_old_phase_says() {
    let live = LiveSnapshot::default();
    for phase in node_phases() {
        let mut state = sdk_task();
        state.agents.get_mut(&"a-1".into()).unwrap().phase = phase.clone();
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            for (lang, working) in [(Lang::En, "Working"), (Lang::Es, "Trabajando")] {
                let list = render(&state, &live, client, lang, "tasks", "");
                let row = rows_with_parents(&list).into_iter().map(|(_, row)| row).find(|row| row.id == "t-1")
                    .unwrap_or_else(|| panic!("{client:?} {phase:?}: t-1 is listed"));
                let texts: Vec<&str> = row.badges.iter().map(|badge| badge.text.as_str()).collect();
                assert_eq!(texts, [working], "{client:?} {lang:?}: its node's phase {phase:?}");
            }
        }
    }
}

#[test]
fn values_from_a_newer_daemon_decode_as_unsupported_or_their_default() {
    assert_eq!(serde_json::from_value::<Link>(serde_json::json!({"kind": "deep_link", "target": "x"})).unwrap(), Link::Unsupported);
    assert_eq!(
        serde_json::from_value::<Link>(serde_json::json!({"kind": "local", "local": {"kind": "passkey_sign_in", "card": "c"}})).unwrap(),
        Link::Local { local: LocalLink::Unsupported }
    );
    assert_eq!(serde_json::from_value::<Trailing>(serde_json::json!({"kind": "meter", "value": 3})).unwrap(), Trailing::Unsupported);
    assert_eq!(serde_json::from_value::<Input>(serde_json::json!({"kind": "voice"})).unwrap(), Input::Unsupported);
    assert_eq!(serde_json::from_value::<FieldKind>(serde_json::json!({"kind": "time_range"})).unwrap(), FieldKind::Unsupported);
    assert_eq!(serde_json::from_value::<Presentation>(serde_json::json!("slider")).unwrap(), Presentation::Unsupported);
    assert_eq!(serde_json::from_value::<CommentKind>(serde_json::json!("pixels")).unwrap(), CommentKind::Unsupported);
    assert_eq!(serde_json::from_value::<Tone>(serde_json::json!("aurora")).unwrap(), Tone::Neutral);
    assert_eq!(serde_json::from_value::<Level>(serde_json::json!("scorching")).unwrap(), Level::Calm);
    assert_eq!(serde_json::from_value::<Icon>(serde_json::json!("calendar")).unwrap(), Icon::Notice);
    assert_eq!(serde_json::from_value::<Style>(serde_json::json!("glow")).unwrap(), Style::Secondary);
    assert_eq!(serde_json::from_value::<Detail>(serde_json::json!("streamed")).unwrap(), Detail::Reference);
    assert_eq!(serde_json::from_value::<TimeFormat>(serde_json::json!("countdown")).unwrap(), TimeFormat::Relative);
    assert_eq!(serde_json::from_value::<ContentKind>(serde_json::json!("video")).unwrap(), ContentKind::File);
    assert_eq!(serde_json::from_value::<AttachmentKind>(serde_json::json!("audio")).unwrap(), AttachmentKind::Binary);
    assert_eq!(serde_json::from_value::<CheckState>(serde_json::json!("skipped")).unwrap(), CheckState::Pending);
    assert_eq!(serde_json::from_value::<OutcomeKind>(serde_json::json!("queued")).unwrap(), OutcomeKind::Changed);
    let section: Section =
        serde_json::from_value(serde_json::json!({"id": "s", "collapsed": false, "items": [], "style": "grid"})).unwrap();
    assert_eq!(section.style, None);
    let row: Row = serde_json::from_value(serde_json::json!({
        "id": "r", "icon": "task", "title": "T", "depth": 0, "struck": false, "kind": "sign_in"
    }))
    .unwrap();
    assert_eq!(row.kind, None);
}

#[test]
fn a_known_kind_with_broken_data_from_the_daemon_still_fails() {
    assert!(serde_json::from_value::<Link>(serde_json::json!({"kind": "view"})).is_err());
    assert!(serde_json::from_value::<LocalLink>(serde_json::json!({"kind": "attach"})).is_err());
    assert!(serde_json::from_value::<FieldKind>(serde_json::json!({"kind": "stepper", "min": "a"})).is_err());
    assert!(serde_json::from_value::<Trailing>(serde_json::json!({"kind": "count"})).is_err());
    assert!(serde_json::from_value::<Input>(serde_json::json!({"kind": "text"})).is_err());
}

#[test]
fn the_daemon_cannot_send_an_unsupported_value() {
    assert!(serde_json::to_value(Link::Unsupported).is_err());
    assert!(serde_json::to_value(Link::Local { local: LocalLink::Unsupported }).is_err());
    assert!(serde_json::to_value(Trailing::Unsupported).is_err());
    assert!(serde_json::to_value(Input::Unsupported).is_err());
    assert!(serde_json::to_value(FieldKind::Unsupported).is_err());
    assert!(serde_json::to_value(Presentation::Unsupported).is_err());
    assert!(serde_json::to_value(CommentKind::Unsupported).is_err());
}

fn all_rows(rows: &[Row]) -> Vec<Row> {
    rows.iter().flat_map(|row| std::iter::once(row.clone()).chain(all_rows(&row.children))).collect()
}

fn elapsed_row(view: &View) -> Option<&Row> {
    view.sections.iter().find(|section| section.id == "elapsed").and_then(|section| match section.items.first() {
        Some(Item::Row(row)) => Some(row),
        _ => None,
    })
}

#[test]
fn a_task_counts_its_time_from_its_first_start_in_its_history_not_from_its_node() {
    let mut state = fixture();
    state.interactions.clear();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    (task.agent_id, task.state, task.final_presentation) = (Some("a-1".into()), TaskState::NeedsYou, None);
    let mut live = LiveSnapshot::default();
    live.task_since.insert("t-1".into(), "2026-09-30T09:40:00.000+02:00".into());
    live.nodes.push(LiveNode {
        agent_id: "a-1".into(), name: "Node One".into(), waiting: true,
        state_since: Some("2026-09-30T09:40:00.000+02:00".into()),
        ..LiveNode::default()
    });
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let (_, row) = active_row(&state, &live, client, Lang::En, "t-1");
        assert!(row.badges.iter().all(|badge| badge.elapsed.is_none()), "{client:?}: a task without a start in its history counts nothing");
    }
    live.task_started.insert("t-1".into(), LiveTaskStart { since: "2026-09-30T08:05:00.000+02:00".into(), until: None, ran_secs: None });
    let started = Some(Elapsed { since: "2026-09-30T08:05:00.000+02:00".into(), until: None });
    for (client, lang, text) in [(Client::Ios, Lang::Es, "en marcha desde las 08:05"), (Client::Mac, Lang::En, "running since 08:05"),
        (Client::Tui, Lang::Es, "en marcha desde las 08:05")] {
        let (_, row) = active_row(&state, &live, client, lang, "t-1");
        let mark = row.badges.last().unwrap();
        assert_eq!((mark.text.as_str(), &mark.elapsed), (text, &started), "{client:?}: counts from the history while it waits for the user");
        let view = render(&state, &live, client, lang, "task:t-1", "");
        let detail = elapsed_row(&view).expect("the task's screen says how long it runs");
        let (title, since) = match lang { Lang::En => ("Running for", "since 08:05"), Lang::Es => ("En marcha", "desde las 08:05") };
        assert_eq!((detail.title.as_str(), detail.badges[0].text.as_str(), &detail.badges[0].elapsed), (title, since, &started), "{client:?}");
    }
}

#[test]
fn a_finished_task_shows_how_long_it_took_frozen() {
    let mut state = fixture();
    state.interactions.clear();
    state.tasks.get_mut(&"t-1".into()).unwrap().state = TaskState::Done;
    let mut live = LiveSnapshot::default();
    live.task_started.insert("t-1".into(), LiveTaskStart {
        since: "2026-09-30T08:00:00.000+02:00".into(), until: Some("2026-09-30T09:05:00.000+02:00".into()), ran_secs: Some(3_900),
    });
    let took = Some(Elapsed { since: "2026-09-30T08:00:00.000+02:00".into(), until: Some("2026-09-30T09:05:00.000+02:00".into()) });
    for (client, lang, listed, detail_title) in [(Client::Mac, Lang::En, "took 1 h 05 min", "Took"), (Client::Ios, Lang::Es, "duró 1 h 05 min", "Duró")] {
        let rows: Vec<Row> = render(&state, &live, client, lang, "tasks?section=finished&after=0", "").sections.into_iter()
            .flat_map(|section| section.items)
            .filter_map(|item| match item { Item::Row(row) => Some(row), _ => None }).collect();
        let row = all_rows(&rows).into_iter().find(|row| ["t-1", "r-1"].contains(&row.id.as_str()) && row.badges.iter().any(|badge| badge.elapsed.is_some()))
            .unwrap_or_else(|| panic!("{client:?}: the finished task says how long it took: {rows:?}"));
        let mark = row.badges.last().unwrap();
        assert_eq!((mark.text.as_str(), &mark.elapsed), (listed, &took), "{client:?}");
        let view = render(&state, &live, Client::Ios, lang, "task:t-1", "");
        let detail = elapsed_row(&view).unwrap();
        assert_eq!((detail.title.as_str(), detail.badges[0].text.as_str(), &detail.badges[0].elapsed), (detail_title, "1 h 05 min", &took));
    }
}

/// 24 October, in the sandbox: David wrote to a task whose PR waited for his review, the node of
/// the Agent SDK took the message up in a turn, and the task still read «Ready for review» with
/// nothing that said it worked. While its turn runs it reads «Working» on every client, the list
/// and the task's screen, and once the turn ends it is back to «Ready for review».
#[test]
fn a_task_in_review_reads_working_while_its_node_takes_up_a_message_and_in_review_once_it_ends() {
    use crate::domain::{ExecutionIdentity, LifecycleRecord};
    let mut state = release_delivered_twice(None);
    let delivered = vec![
        LifecycleRecord::Planned { input: "input:t-1:1".into() },
        LifecycleRecord::Admitted {
            node: "a-1".into(),
            execution: Some(ExecutionIdentity { conversation: "conversation".into(), instance: "instance".into(), host: "alondra-sdk".into() }),
        },
        LifecycleRecord::ResourceAvailable,
        LifecycleRecord::TurnStarted { turn: "turn-1".into(), input: "input:t-1:1".into() },
        LifecycleRecord::Presented { delivery: "published-1".into() },
        LifecycleRecord::TurnEnded { turn: "turn-1".into(), unheard_card: "unheard-1".into(), wake: None, text: None, awaits: None, stalled: false },
    ];
    let told = [delivered.clone(), vec![
        LifecycleRecord::Told { input: "input:t-1:2".into(), text: Some("Also bump the changelog".into()) },
        LifecycleRecord::TurnStarted { turn: "turn-2".into(), input: "input:t-1:2".into() },
    ]].concat();
    let ended = [told.clone(), vec![LifecycleRecord::TurnEnded { turn: "turn-2".into(), unheard_card: "unheard-2".into(), wake: None, text: None, awaits: None, stalled: false }]].concat();
    let live = LiveSnapshot::default();
    let shown = |state: &DomainState, client, lang| {
        let row = active_row(state, &live, client, lang, "t-1").1.badges[0].text.clone();
        let screen = serde_json::to_string(&render(state, &live, client, lang, "task:t-1", "")).unwrap();
        assert!(screen.contains(&serde_json::to_string(&row).unwrap()), "{client:?}: the task's screen says {row}: {screen}");
        row
    };
    for (journal, en, es) in [(delivered, "Ready for review", "Para revisar"), (told, "Working", "Trabajando"), (ended, "Ready for review", "Para revisar")] {
        state.lifecycle_journal.insert("t-1".into(), journal);
        state.derive_states();
        assert_eq!(state.tasks[&"t-1".into()].state, TaskState::ForYourReview, "its delivery still waits for the review");
        for client in [Client::Ios, Client::Mac, Client::Tui] {
            assert_eq!(shown(&state, client, Lang::En), en, "{client:?}");
            assert_eq!(shown(&state, client, Lang::Es), es, "{client:?}");
        }
    }
}


#[test]
fn provider_accounts_share_one_order_without_aliasing_same_slugs_or_native_windows() {
    use crate::accounts::{AccountKey, Provider};
    let (mut state, mut live) = accounts_fixture();
    let work = state.accounts.iter().find(|account| account.id.as_str() == "work").unwrap().clone();
    let mut codex = work.clone();
    codex.provider = Provider::Codex;
    state.accounts.push(codex);
    let key = |provider| AccountKey { provider, account: work.id.clone() };
    state.account_order = vec![key(Provider::Codex), key(Provider::Claude)];
    state.account_usage.push(crate::usage::AccountUsage {
        provider: Provider::Codex, account: work.id.clone(), plan: None, day_start: None,
        windows: vec![crate::usage::UsageWindow { reading: crate::usage::WindowReading {
            kind: crate::usage::WindowKind::named("codex:account:primary:300"), used: crate::usage::Share::of(0.42), resets_at: Some(1_000), exhausted: false,
        }, measured_at: 10 }],
    });
    live.observed_at = Some(2_000);
    let view = render(&state, &live, Client::Ios, Lang::En, "accounts", "");
    let rows: Vec<_> = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item { Item::Row(row) if row.account.is_some() => Some(row), _ => None }).collect();
    let codex = rows.iter().find(|row| row.account.as_ref().unwrap().key == key(Provider::Codex)).unwrap();
    let claude = rows.iter().find(|row| row.account.as_ref().unwrap().key == key(Provider::Claude)).unwrap();
    assert_ne!(codex.id, claude.id);
    assert_eq!(codex.account.as_ref().unwrap().position, 1);
    assert_eq!(claude.account.as_ref().unwrap().position, 2);
    assert!(codex.subtitle.as_ref().unwrap().starts_with("Codex"));
    assert!(!codex.subtitle.as_ref().unwrap().contains("weight"));
    assert!(!codex.actions.iter().any(|action| action.id.starts_with("account_weight:")));
    let policy = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Row(row) if row.id == "claude-policy" => Some(row), _ => None }).unwrap();
    assert_eq!(policy.title, "In list order"); assert!(policy.actions.is_empty());
    let window = &codex.account.as_ref().unwrap().windows[0];
    assert_eq!((window.used_percent, window.measured_at, window.resets_at, window.stale), (42, 10, Some(1_000), true));
    assert!(window.kind.starts_with("codex:account:"));
    assert_eq!(window.label, "Account · Primary · 300 min");
    let spanish = render(&state, &live, Client::Ios, Lang::Es, "accounts", "");
    let metadata = spanish.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Row(row) if row.account.as_ref().is_some_and(|metadata| metadata.key == key(Provider::Codex)) => row.account.as_ref(), _ => None }).unwrap();
    assert_eq!(metadata.windows[0].label, "Cuenta · Principal · 300 min");
    assert!(!codex.actions.iter().any(|action| action.id.starts_with("account_") && action.id.split('~').next_back() == Some("claude")));
    assert!(codex.actions.iter().filter(|action| !action.id.is_empty()).all(|action| action.id.ends_with("~codex")));
}

#[test]
fn committed_pending_models_never_replace_effective_models_or_positional_task_header_facts() {
    use crate::accounts::{AccountKey, Provider};
    use crate::model_selection::{ModelKey, ModelProfile, ModelSelection, PendingTaskSelection, TaskSelection};
    let mut state = fixture();
    let id = state.tasks.keys().next().unwrap().clone();
    let old = ModelSelection { model: ModelKey { provider: Provider::Claude, model: "actual-model".into() }, reasoning_effort: None, catalog_revision: "old-catalog".into() };
    let next = ModelSelection { model: ModelKey { provider: Provider::Codex, model: "next-model".into() }, reasoning_effort: Some("high".into()), catalog_revision: "next-catalog".into() };
    let task = state.tasks.get_mut(&id).unwrap();
    let mut selection = TaskSelection::automatic(ModelProfile::Implementation);
    selection.effective = Some(old);
    task.selection = Some(selection);
    task.pending_selection = Some(PendingTaskSelection { account: AccountKey::native(Provider::Codex), model: next, reason: "The user chose the next model".into() });
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, &format!("task:{id}"), "");
    let facts = |section: &str| view.sections.iter().filter(|part| part.id == section).flat_map(|part| &part.items).flat_map(|item| match item { Item::Facts(facts) => facts.items.clone(), _ => vec![] }).collect::<Vec<_>>();
    let header = facts("header");
    assert_eq!(header[0].label, "State");
    assert_eq!(header[1].label, "Request", "native readers rely on this established header shape");
    assert!(!header.iter().any(|fact| matches!(fact.label.as_str(), "Requested model" | "Effective model" | "Pending model")));
    let readings = facts("readings");
    assert_eq!(readings.iter().find(|fact| fact.label == "Requested model").unwrap().value, "Automatic");
    assert_eq!(readings.iter().find(|fact| fact.label == "Effective model").unwrap().value, "Claude · actual-model");
    assert!(readings.iter().find(|fact| fact.label == "Pending model").unwrap().value.contains("Codex · next-model · high"));
}

fn advertised_model_controls_fixture() -> (DomainState, LiveSnapshot) {
    use crate::model_selection::{AccountModelCatalog, CatalogModel, ModelCandidate, ModelProfile, RoleModelPolicy};
    let (mut state, live) = accounts_fixture();
    state.orchestrator_session = None;
    let catalog = AccountModelCatalog { account: crate::accounts::AccountKey::native(crate::accounts::Provider::Claude), revision: "catalog-one".into(), measured_at: 1000,
        native_default: None, models: ["native-large", "native-fast"].into_iter().map(|name| CatalogModel { model: name.into(), display_name: name.into(), reasoning_efforts: vec!["low".into(), "high".into()], default_reasoning_effort: Some("high".into()), context_window: Some(128000), input_modalities: vec!["text".into()], is_default: false, hidden: false }).collect() };
    state.model_catalogs = vec![catalog];
    state.model_profiles.entry(crate::accounts::Provider::Claude).or_default().insert(ModelProfile::Orchestrator, RoleModelPolicy::OrderedCandidates { candidates: vec![
        ModelCandidate { model: "native-large".into(), reasoning_effort: Some("high".into()) }, ModelCandidate { model: "native-fast".into(), reasoning_effort: Some("low".into()) },
    ] });
    state.model_profiles_version = crate::model_selection::profiles_version(&state.model_profiles);
    assert!(state.journal_every_task());
    (state, live)
}

#[test]
fn provider_model_bootstrap_is_available_without_an_orchestrator_and_never_invents_a_native_default() {
    let (state, live) = advertised_model_controls_fixture();
    let accounts = render(&state, &live, Client::Ios, Lang::En, "accounts", "");
    let role = accounts.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Row(row) if row.title == "Claude · Orchestrator" => Some(row), _ => None }).unwrap();
    let chooser = role.actions.iter().find(|action| action.label == "Choose model").unwrap();
    assert!(chooser.enabled); assert!(chooser.options.iter().any(|option| option.id == "model:claude/native-large"));
    assert!(!chooser.options.iter().any(|option| option.id == "auto"), "catalog order does not prove a native default");
    assert!(chooser.id.contains(&state.model_profiles_version));
    assert!(role.actions.iter().any(|action| action.label == "Reasoning effort"));
    let view = render(&state, &live, Client::Tui, Lang::En, "model_profile:claude-orchestrator", "");
    let form = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Form(form) => Some(form), _ => None }).unwrap();
    assert!(form.submit.enabled); assert!(form.submit.id.contains(&state.model_profiles_version));
    for (id, value) in [("model", "claude/native-large"), ("reasoning_effort", "high"), ("candidate_2", "claude/native-fast"), ("reasoning_effort_2", "low"), ("candidate_3", "")] {
        assert_eq!(form.fields.iter().find(|field| field.id == id).unwrap().value, serde_json::json!(value));
    }
    let FieldKind::Choice { items } = &form.fields.iter().find(|field| field.id == "model").unwrap().kind else { panic!("model choice") };
    assert!(items.iter().find(|item| item.id == "auto").unwrap().disabled_reason.is_some());
}

#[test]
fn active_model_editor_keeps_account_read_only_and_uses_selection_stamp_instead_of_progress_revision() {
    let (mut state, live) = advertised_model_controls_fixture();
    let task = state.tasks.values().find(|task| !task.state.terminal()).unwrap().id.clone();
    state.tasks.get_mut(&task).unwrap().selection = Some(crate::model_selection::TaskSelection::automatic(crate::model_selection::ModelProfile::Implementation));
    let key = format!("task_selection:{task}");
    let view = render(&state, &live, Client::Tui, Lang::En, &key, "");
    let form = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Form(form) => Some(form), _ => None }).unwrap();
    assert!(form.fields.iter().all(|field| !matches!(field.id.as_str(), "account" | "pin_account" | "reasoning_effort")), "existing-node account and effort selection is read-only");
    let model = form.fields.iter().find(|field| field.id == "model").unwrap();
    assert!(model.help.as_ref().unwrap().contains("Create another node"));
    let FieldKind::Choice { items } = &model.kind else { panic!("model choices") };
    assert!(items.iter().all(|item| item.id == "auto" || item.id.starts_with("claude/")));
    assert_eq!(form.substitution_was_edited, Some(false));
    assert!(form.submit.id.contains("@0~"));
    assert!(form.submit.id.ends_with(&state.task_selection_stamp(&task).unwrap()));
    let first = form.submit.id.clone(); state.revision += 100;
    let view = render(&state, &live, Client::Tui, Lang::En, &key, "");
    let form = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Form(form) => Some(form), _ => None }).unwrap();
    assert_eq!(form.submit.id, first, "unrelated progress must not make this input stale");
}

#[test]
fn native_limit_errors_show_no_fabricated_usage_percentage() {
    let (mut state, live) = accounts_fixture();
    let mut account = state.accounts[0].clone(); account.provider = crate::accounts::Provider::Codex;
    state.accounts.push(account.clone());
    state.account_usage.push(crate::usage::AccountUsage { provider: account.provider, account: account.id.clone(), plan: None, day_start: None,
        windows: vec![crate::usage::UsageWindow { measured_at: 10, reading: crate::usage::WindowReading { kind: crate::usage::WindowKind::Other("codex:account:limit".into()), used: crate::usage::Share::NONE, resets_at: None, exhausted: true } }] });
    let view = render(&state, &live, Client::Ios, Lang::En, "accounts", "");
    let row = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Row(row) if row.account.as_ref().is_some_and(|metadata| metadata.key.provider == crate::accounts::Provider::Codex && metadata.key.account == account.id) => Some(row), _ => None }).unwrap();
    let metadata = row.account.as_ref().unwrap();
    assert!(metadata.windows.is_empty());
    assert!(metadata.reason.as_ref().unwrap().contains("no usage percentage was reported"));
    assert!(row.subtitle.as_ref().unwrap().contains("Provider reported a usage limit"));
    assert!(!row.subtitle.as_ref().unwrap().contains("0 %"));
}

#[test]
fn profile_edit_and_active_model_navigation_use_existing_view_links() {
    let (state, live) = advertised_model_controls_fixture();
    let accounts = render(&state, &live, Client::Tui, Lang::En, "accounts", "");
    let edit = accounts.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Row(row) if row.title == "Claude · Orchestrator" => row.actions.iter().find(|action| action.label == "Edit ordered candidates"), _ => None }).unwrap();
    assert!(edit.id.is_empty(), "navigation must not post an empty profile mutation");
    assert_eq!(edit.link, Some(Link::View { key: "model_profile:claude-orchestrator".into() }));
    let task = state.tasks.values().find(|task| !task.state.terminal()).unwrap().id.clone();
    let detail = render(&state, &live, Client::Tui, Lang::En, &format!("task:{task}"), "");
    let navigate = detail.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item { Item::Actions(bar) => Some(&bar.actions), _ => None }).flatten().find(|action| action.label == "Change model").unwrap();
    assert!(navigate.id.is_empty()); assert_eq!(navigate.link, Some(Link::View { key: format!("task_selection:{task}") }));
}

#[test]
fn codex_hold_cards_use_qualified_account_names_and_native_sign_in_guidance() {
    use crate::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, Provider};
    let mut state = fixture();
    let task = state.tasks.get_mut(&"t-1".into()).unwrap();
    task.provider = Provider::Codex;
    task.account = Some(AccountId::parse("work").unwrap());
    state.accounts = [
        (Provider::Claude, "Claude work"), (Provider::Codex, "Codex work"),
    ].map(|(provider, name)| AccountStatus { provider, id: AccountId::parse("work").unwrap(), name: name.into(), state: AccountState::SignedOut,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0 }).to_vec();
    let mut interaction = state.interactions[&"i-1".into()].clone();
    state.interactions.clear();
    interaction.id = "claude-signed-out-t-1".into();
    interaction.author = Author::System;
    state.interactions.insert(interaction.id.clone(), interaction);
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        for lang in [Lang::En, Lang::Es] {
            let view = render(&state, &LiveSnapshot::default(), client, lang, "card:claude-signed-out-t-1", "");
            let card = card_of(&view);
            assert!(card.need.as_ref().unwrap().contains("Codex work"), "{card:?}");
            assert!(!card.need.as_ref().unwrap().contains("Claude work"), "{card:?}");
            let why = card.why.as_ref().unwrap();
            assert!(why.contains("alondra accounts login --provider codex work"), "{why}");
            assert!(!why.contains("/login"), "{why}");
        }
    }
}

#[test]
fn a_committed_pending_model_change_localizes_its_app_owned_reason() {
    use crate::model_selection::{ModelKey, ModelProfile, ModelSelection, PendingTaskSelection, TaskSelection};
    let (mut state, live) = advertised_model_controls_fixture();
    let task = state.tasks.values_mut().find(|task| !task.state.terminal()).unwrap();
    let id = task.id.clone();
    let model = ModelSelection { model: ModelKey { provider: task.provider, model: "native-large".into() }, reasoning_effort: Some("high".into()), catalog_revision: "catalog-one".into() };
    let mut selection = TaskSelection::automatic(ModelProfile::Implementation); selection.effective = Some(model.clone());
    task.selection = Some(selection);
    task.pending_selection = Some(PendingTaskSelection { account: task.account_key(), model, reason: "User changed model selection".into() });
    let view = render(&state, &live, Client::Ios, Lang::Es, &format!("task:{id}"), "");
    let shown = serde_json::to_string(&view).unwrap();
    assert!(shown.contains("Cambio de modelo solicitado"), "{shown}");
    assert!(!shown.contains("User changed model selection"), "{shown}");
}

#[test]
fn a_task_account_fact_qualifies_identical_default_slugs_by_provider() {
    use crate::accounts::{AccountId, Provider};
    let (mut state, live) = advertised_model_controls_fixture();
    let account = state.accounts.iter_mut().find(|account| account.provider == Provider::Claude && account.id.is_default()).unwrap();
    account.name = "Claude owner".into();
    let mut codex = account.clone(); codex.provider = Provider::Codex; codex.name = "Codex owner".into();
    state.accounts.push(codex);
    let task = state.tasks.values_mut().find(|task| !task.state.terminal()).unwrap();
    task.provider = Provider::Codex; task.account = Some(AccountId::default_account()); let id = task.id.clone();
    for client in [Client::Ios, Client::Mac, Client::Tui] {
        let view = render(&state, &live, client, Lang::En, &format!("task:{id}"), "");
        let facts = view.sections.iter().find(|section| section.id == "options").unwrap().items.iter().find_map(|item| match item { Item::Facts(facts) => Some(&facts.items), _ => None }).unwrap();
        let account = facts.iter().find(|fact| fact.label == "Codex account").unwrap();
        assert_eq!(account.value, "Codex owner", "{client:?}");
    }
}

const OUTCOME_ANSWER_CARD: &str = "r-encrypt-answer";
const OUTCOME_DONE_CARD: &str = "request-done-r-accounts";
const OUTCOME_CASES: [(&str, &str, Client, Lang); 12] = [
    ("card-outcome-answer.json", OUTCOME_ANSWER_CARD, Client::Ios, Lang::En),
    ("card-outcome-answer-es.json", OUTCOME_ANSWER_CARD, Client::Ios, Lang::Es),
    ("card-outcome-answer-mac.json", OUTCOME_ANSWER_CARD, Client::Mac, Lang::En),
    ("card-outcome-answer-es-mac.json", OUTCOME_ANSWER_CARD, Client::Mac, Lang::Es),
    ("card-outcome-answer-tui.json", OUTCOME_ANSWER_CARD, Client::Tui, Lang::En),
    ("card-outcome-answer-es-tui.json", OUTCOME_ANSWER_CARD, Client::Tui, Lang::Es),
    ("card-outcome-done.json", OUTCOME_DONE_CARD, Client::Ios, Lang::En),
    ("card-outcome-done-es.json", OUTCOME_DONE_CARD, Client::Ios, Lang::Es),
    ("card-outcome-done-mac.json", OUTCOME_DONE_CARD, Client::Mac, Lang::En),
    ("card-outcome-done-es-mac.json", OUTCOME_DONE_CARD, Client::Mac, Lang::Es),
    ("card-outcome-done-tui.json", OUTCOME_DONE_CARD, Client::Tui, Lang::En),
    ("card-outcome-done-es-tui.json", OUTCOME_DONE_CARD, Client::Tui, Lang::Es),
];

/// Two requests of the user as they ended: one answered without tasks, one whose three tasks are
/// over, two of them done with their PRs and one cancelled.
fn outcome_state() -> DomainState {
    use crate::domain::{Actor, CardSpec, Command, CommandEnvelope, DeliveryTarget, LaunchPlan, MergeMethod, NodeEnd, PROTOCOL_VERSION};
    fn run(state: &mut DomainState, actor: Actor, command: Command) {
        state.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("c-{}", state.revision).into(),
            expected_revision: state.revision,
            command,
        }).unwrap();
    }
    let user = Actor::User { user_id: "u".into() };
    let orchestrator = Actor::Orchestrator { generation: 0 };
    let mut state = DomainState::new("epoch-outcome");
    run(&mut state, user.clone(), Command::CreateRequest {
        request_id: "r-encrypt".into(), body: "¿Podemos cifrar las imágenes antes de subirlas a R2?".into(), auto_merge: false, maintenance: false, message: false,
    });
    run(&mut state, orchestrator.clone(), Command::CloseRequest {
        request_id: "r-encrypt".into(),
        answer: Some("Para el cifrado una imagen son bytes como cualquier otro: el iPhone la cifra con la clave del usuario antes de subirla, y R2 solo guarda bytes cifrados.\n\nYa se hace así desde la versión 0.3.0, así que no hace falta ninguna tarea.".into()),
        withdrawn: false,
        title: Some("Cifrar imágenes en R2".into()),
        headline: Some("Sí: el iPhone ya cifra las imágenes antes de subirlas".into()),
    });
    run(&mut state, user.clone(), Command::CreateRequest {
        request_id: "r-accounts".into(), body: "Quiero usar dos cuentas de Claude y cambiar entre ellas".into(), auto_merge: false, maintenance: false, message: false,
    });
    run(&mut state, orchestrator.clone(), Command::TitleRequest { request_id: "r-accounts".into(), title: "Dos cuentas de Claude".into() });
    for (task, title) in [("t-store", "Guardar varias cuentas de Claude"), ("t-switch", "Cambiar de cuenta en Ajustes"), ("t-docs", "Documentar las cuentas")] {
        run(&mut state, orchestrator.clone(), Command::CreateTask {
            plan_items: None, task_id: task.into(), request_id: "r-accounts".into(), title: title.into(), agent_id: None,
            review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp/project")),
        });
    }
    let text = vec![DeliveryTarget::Text { text: "Hecho".into() }];
    for (task, number) in [("t-store", 1290), ("t-switch", 1291)] {
        let node = Actor::Agent { agent_id: format!("node-{task}").into() };
        run(&mut state, Actor::System, Command::RegisterAgent { agent_id: format!("node-{task}").into(), name: "Node".into(), at: 0 });
        run(&mut state, orchestrator.clone(), Command::AssignTask { task_id: task.into(), agent_id: format!("node-{task}").into(), model: None });
        let result = format!("final-{task}");
        run(&mut state, node, Command::PresentResult {
            checklist: vec![], attachments: vec![], task_id: task.into(), interaction_id: result.as_str().into(), body: "Hecho".into(),
            targets: text.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Revisa el resultado", &text, true),
        });
        run(&mut state, user.clone(), Command::AcceptDelivery { presentation_id: result.as_str().into(), targets: text.clone() });
        run(&mut state, Actor::System, Command::RecordNodeEnd { task_id: task.into(), end: NodeEnd::Closed, agent_id: None });
        let id = crate::domain::PrId::from(format!("pr-{number}"));
        state.pull_requests.insert(id.clone(), PullRequest {
            id: id.clone(), task_id: task.into(), host: "github.com".into(), repository: "owner/alondra".into(), number,
            head: "head1".into(), base: "main".into(), state: PrState::Merged, merged_head: Some("head1".into()),
            allowed_methods: vec![MergeMethod::Squash], review: None,
        });
        state.tasks.get_mut(&task.into()).unwrap().pr_id = Some(id);
    }
    run(&mut state, user, Command::CancelTask { task_id: "t-docs".into() });
    run(&mut state, orchestrator, Command::CloseRequest {
        request_id: "r-accounts".into(),
        answer: Some("Ya puedes guardar dos cuentas de Claude y cambiar entre ellas en Ajustes. La documentación se dejó para más adelante.".into()),
        withdrawn: false, title: None, headline: None,
    });
    state
}

#[test]
fn outcome_cards_match_goldens() {
    let state = outcome_state();
    let differ: Vec<&str> = OUTCOME_CASES.into_iter().zip([
        include_str!("../../../../fixtures/views/cards/card-outcome-answer.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-answer-es.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-answer-mac.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-answer-es-mac.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-answer-tui.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-answer-es-tui.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-done.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-done-es.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-done-mac.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-done-es-mac.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-done-tui.json"),
        include_str!("../../../../fixtures/views/cards/card-outcome-done-es-tui.json"),
    ]).filter_map(|((filename, card, client, lang), expected)| {
        let actual = render(&state, &LiveSnapshot::default(), client, lang, &format!("card:{card}"), "2026-10-26T10:00:00Z");
        std::panic::catch_unwind(|| assert_golden(filename, &actual, expected)).is_err().then_some(filename)
    }).collect();
    assert!(differ.is_empty(), "{differ:?} differ from their goldens");
}

#[test]
fn an_outcome_card_names_its_request_above_and_not_in_its_headline() {
    let state = outcome_state();
    for (card, lang, title, meta) in [
        (OUTCOME_DONE_CARD, Lang::Es, "Hecho en parte: se canceló 1 de 3 tareas", "Dos cuentas de Claude"),
        (OUTCOME_DONE_CARD, Lang::En, "Done in part: 1 of 3 tasks was cancelled", "Dos cuentas de Claude"),
        (OUTCOME_ANSWER_CARD, Lang::Es, "Sí: el iPhone ya cifra las imágenes antes de subirlas", "Cifrar imágenes en R2"),
    ] {
        let view = render(&state, &LiveSnapshot::default(), Client::Ios, lang, &format!("card:{card}"), "2026-10-26T10:00:00Z");
        let Some(Item::Card(shown)) = view.sections.iter().flat_map(|section| &section.items).next() else { panic!("{card} renders as a card") };
        assert_eq!((shown.title.as_str(), shown.meta.as_deref()), (title, Some(meta)));
        assert!(shown.actions.iter().any(|action| action.id.starts_with("read:")), "{card} is marked seen");
    }
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::Es, &format!("card:{OUTCOME_DONE_CARD}"), "2026-10-26T10:00:00Z");
    let facts: Vec<(String, Option<Link>)> = view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        Item::Facts(facts) => Some(facts.items.iter().map(|fact| (format!("{}: {}", fact.label, fact.value), fact.link.clone())).collect::<Vec<_>>()),
        _ => None,
    }).flatten().collect();
    assert_eq!(facts, [
        ("Hecha: Guardar varias cuentas de Claude · PR #1290".to_owned(), Some(Link::View { key: "task:t-store".into() })),
        ("Hecha: Cambiar de cuenta en Ajustes · PR #1291".to_owned(), Some(Link::View { key: "task:t-switch".into() })),
        ("Cancelada: Documentar las cuentas".to_owned(), Some(Link::View { key: "task:t-docs".into() })),
    ]);
}

fn with_brought_images(live: &mut LiveSnapshot, body: &str) {
    use crate::views::pr_images::{ImageProblem, PrImage, Shown};
    let images = crate::markdown::images::images(body)
        .into_iter()
        .map(|image| PrImage {
            shown: if image.url.contains("secret") {
                Shown::Unavailable { link: image.url.clone(), problem: ImageProblem::NoAccess { repository: "secret/repo".into() } }
            } else {
                Shown::Stored { route: format!("/files?path=files/images/{}.png", "a".repeat(64)) }
            },
            span: image.span,
            dest: image.dest,
            url: image.url,
            alt: image.alt,
            html: image.html,
        })
        .collect();
    live.pull_requests[0].body = Some(body.into());
    live.pull_requests[0].images = images;
}

const IMAGES_BODY: &str = "Storage areas:\n\n![storage](https://github.com/example/repo/blob/8d0e5fb7edcc190633b9fc0b868f7051e48bc0f9/docs/a.jpg?raw=true)\n\n![](https://github.com/secret/repo/blob/main/b.png)\n";

#[test]
fn a_pull_request_card_and_its_detail_show_the_images_the_daemon_brought_and_say_why_for_the_rest() {
    let state = golden_fixture();
    let mut live = golden_live();
    with_brought_images(&mut live, IMAGES_BODY);
    let stored = format!("![storage](/files?path=files/images/{}.png)", "a".repeat(64));
    for path in ["card:github.com/example/repo#12", "pull_request:github.com/example/repo#12"] {
        let view = render(&state, &live, Client::Ios, Lang::Es, path, "");
        let description = section_of(&view, "description").unwrap();
        let [Item::Markdown(body)] = &description.items[..] else { panic!("one text: {:?}", description.items) };
        assert!(body.text.contains(&stored), "{path}: {}", body.text);
        assert!(
            body.text.contains("![b.png — GitHub no da acceso a secret/repo](https://github.com/secret/repo/blob/main/b.png)"),
            "{path}: {}",
            body.text
        );
    }
}

#[test]
fn a_report_that_repeats_the_description_shows_its_images_too() {
    let mut state = golden_fixture();
    for interaction in state.interactions.values_mut() {
        if interaction.card.need == "Review the beta candidate" {
            interaction.card.content = Some(crate::domain::CardContent {
                kind: crate::domain::ContentKind::Report,
                summary: None,
                body: format!("What changed.\n\n{IMAGES_BODY}"),
            });
        }
    }
    let mut live = golden_live();
    with_brought_images(&mut live, IMAGES_BODY);
    let view = render(&state, &live, Client::Ios, Lang::En, "card:github.com/example/repo#12", "");
    let description = section_of(&view, "description").unwrap();
    let [Item::Markdown(body)] = &description.items[..] else { panic!("one text: {:?}", description.items) };
    assert_eq!(body.text.matches("Storage areas").count(), 1, "{}", body.text);
    assert!(body.text.contains(&format!("![storage](/files?path=files/images/{}.png)", "a".repeat(64))), "{}", body.text);
    assert!(body.text.contains("![b.png — GitHub gives no access to secret/repo]("), "{}", body.text);
}

#[test]
fn an_attachment_whose_file_is_an_image_reaches_the_apps_as_an_image() {
    let mut state = fixture();
    let attachment = |id: &str, name: &str, reference: &str| crate::domain::AttachmentRef {
        id: id.into(), name: name.into(), content_digest: "d".repeat(64), content_reference: reference.into(), bytes: 10,
    };
    state.interactions.get_mut(&"i-1".into()).unwrap().attachments = vec![
        attachment("shot", "iPhone, claro", "files/t/ios-light.JPG"),
        attachment("named", "dark.webp", "files/t/stored"),
        attachment("log", "build.log", "files/t/build.log"),
    ];
    let view = render(&state, &LiveSnapshot::default(), Client::Ios, Lang::En, "card:i-1", "");
    let kinds: Vec<_> = card_of(&view).attachments.iter().map(|a| (a.id.as_str(), a.kind)).collect();
    assert_eq!(kinds, [("shot", AttachmentKind::Image), ("named", AttachmentKind::Image), ("log", AttachmentKind::Binary)]);
}
