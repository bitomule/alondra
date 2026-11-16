use super::*;
use alondra_domain_rules::{CardKind, CardSpec, CardType, InboxCard, NodePauseKind, Reader, SystemCard, SystemCardKind};

/// Data the system actually keeps in these cards; prose belongs to the final view.
fn system_fixture(kind: SystemCardKind) -> DomainState {
    use SystemCardKind as K;
    let mut state = fixture();
    state.tasks.get_mut(&"t-1".into()).unwrap().title = "Publicar la versión".into();
    let mut interaction = state.interactions[&"i-1".into()].clone();
    state.interactions.clear();
    let (system, body) = match kind {
        K::NodePause => (SystemCard::NodePause { pause: NodePauseKind::User, since: 0 }, String::new()),
        K::OverPace => (SystemCard::OverPace { day: "2026-11-07".into() }, alondra_core::usage::PaceNumbers { today_spent: 20, today_share: 10, week_percent: 50 }.to_body()),
        K::PaceResume => (SystemCard::PaceResume { day: "2026-11-07".into() }, String::new()),
        K::Machine => (SystemCard::Machine, alondra_core::machine_incident::MachineCard {
            cause: alondra_core::machine_incident::MachineCause::Disk, disk_free: 1_000_000_000, nodes: vec![], reason: None,
            resource: None, minutes: None, disk: None, still: false, purgeable: 0,
        }.to_body()),
        K::LocalNetwork => (SystemCard::LocalNetwork, alondra_domain_rules::local_network_card::LocalNetworkCard { launcher: true, still: false }.to_body()),
        K::McpWait => (SystemCard::McpWait, serde_json::to_string(&alondra_domain_rules::McpServerKey { name: "example".into(), url: None }).unwrap()),
        K::ClaudeSignedOut => (SystemCard::ClaudeSignedOut, String::new()),
        K::ClaudeOutage => (SystemCard::ClaudeOutage, String::new()),
        K::ClaudeUsage => (SystemCard::ClaudeUsage { resets_at: None }, String::new()),
        K::HostLost => (SystemCard::HostLost, String::new()),
        K::TurnEnded => (SystemCard::TurnEnded, String::new()),
        K::SessionEnded => (SystemCard::SessionEnded, String::new()),
        K::HandoffDoubt => (SystemCard::HandoffDoubt, String::new()),
        K::DecisionCancelled => (SystemCard::DecisionCancelled, String::new()),
        K::Dropped => (SystemCard::Dropped, String::new()),
        K::Unheard => (SystemCard::Unheard, String::new()),
        K::EndedUnanswered => {
            let mut question = interaction.clone();
            question.id = "earlier".into();
            question.card = CardSpec::new(CardKind::Decide, "Publicar la versión");
            state.interactions.insert(question.id.clone(), question);
            (SystemCard::EndedUnanswered { question: "earlier".into() }, String::new())
        }
        K::StartProblem => (SystemCard::StartProblem, serde_json::to_string(&alondra_domain_rules::StartProblem::NodeClosed).unwrap()),
        K::PostMergeProblem => (SystemCard::PostMergeProblem, "Sigue publicando".into()),
        K::CloseProblem => (SystemCard::CloseProblem, alondra_domain_rules::close_problem::CloseProblem {
            title: "Publicar la versión".into(), cancelled: false, stop: alondra_domain_rules::close_problem::CloseStop::Working, detail: String::new(),
        }.to_body()),
        K::DiscardWorktrees => (SystemCard::DiscardWorktrees, alondra_domain_rules::discard_worktrees::DiscardWorktrees { tasks: vec![
            alondra_domain_rules::discard_worktrees::CancelledWorktree { task_id: "t-1".into(), title: "Publicar la versión".into(), files: 1, paths: vec!["src/main.rs".into()] },
        ] }.to_body()),
        K::OpenPrDelivery => (SystemCard::OpenPrDelivery, alondra_domain_rules::open_pr_delivery::OpenPrDelivery {
            title: "Publicar la versión".into(), repository: "example/repo".into(), number: 12,
            readiness: alondra_domain_rules::open_pr_delivery::PrReadiness::Ready, result: None,
        }.to_body()),
        K::LaunchProblem => (SystemCard::LaunchProblem { run: "run".into() }, "No se pudo abrir la carpeta de trabajo".into()),
        K::BatchProblem => (SystemCard::BatchProblem { run: "run".into() }, alondra_domain_rules::orchestrator_batch::BatchProblem {
            stop: alondra_domain_rules::orchestrator_batch::BatchStop::Interrupted, calls: vec![], subjects: vec![], error: String::new(),
        }.to_body()),
        K::StuckProblem => (SystemCard::StuckProblem, alondra_domain_rules::orchestrator_cards::StuckProblem { failures: 3, error: String::new() }.to_body()),
        K::RequestDone => (SystemCard::RequestDone, alondra_domain_rules::request_outcome::DoneCard {
            request_id: "r-1".into(), summary: Some("Publicada la versión 1.2, build 34, en TestFlight".into()), tasks: vec![],
        }.to_body()),
        K::RequestTasks => (SystemCard::RequestTasks, alondra_domain_rules::request_outcome::TasksCard {
            request_id: "r-1".into(), more: false, tasks: vec![alondra_domain_rules::request_outcome::AnnouncedTask {
                id: "t-1".into(), title: "Publicar la versión".into(), priority: Default::default(), after: vec![],
            }],
        }.to_body()),
        K::Inbox => (SystemCard::Inbox { about: InboxCard::UnsplitProblem, subject: "r-1".into(), number: 1 },
            alondra_domain_rules::orchestrator_cards::UnsplitProblem { request_id: "r-1".into(), title: "Publicar la versión".into() }.to_body()),
        K::Stalled | K::RefusedCards | K::Unknown => panic!("{kind:?} is not a client card"),
    };
    interaction.author = Author::System;
    interaction.system_card = Some(system);
    interaction.body = body;
    interaction.card = CardSpec::new(CardType::System(kind).contract().kinds[0], stored_title(kind));
    interaction.payload = if CardType::System(kind).contract().response == alondra_domain_rules::Response::Reads {
        InteractionPayload::Information { read_by: BTreeSet::new() }
    } else {
        InteractionPayload::Question { options: fixture_options(kind, &interaction.body), answer: None }
    };
    interaction.blocking = !matches!(&interaction.payload, InteractionPayload::Information { .. });
    if kind == K::HostLost {
        interaction.payload = InteractionPayload::Question { options: vec![QuestionOption {
            id: alondra_domain_rules::RETAKE.into(), label: alondra_domain_rules::RETAKE_LABEL.into(), description: None,
        }], answer: None };
    }
    if kind == K::Unheard { interaction.body = interaction.card.need.clone(); }
    if kind == K::PostMergeProblem {
        interaction.card = CardSpec::new(CardKind::Problem, "Decide whether to close the session")
            .why("Its node says it still works after the merge and does not answer.")
            .content(alondra_domain_rules::ContentKind::Diagnosis, &interaction.body);
    }
    if kind == K::LaunchProblem {
        interaction.card = CardSpec::new(CardKind::Problem, "Scheduled task \"Publicar la versión\" could not start")
            .why("No se pudo abrir la carpeta de trabajo")
            .content(alondra_domain_rules::ContentKind::Diagnosis, &interaction.body);
    }
    if kind == K::OpenPrDelivery {
        interaction.payload = InteractionPayload::Delivery {
            targets: vec![], is_final: true, acceptance: None, feedback: vec![],
        };
        state.tasks.get_mut(&"t-1".into()).unwrap().final_presentation = Some(interaction.id.clone());
    }
    if kind == K::NodePause {
        interaction.card = CardSpec::new(CardKind::Decide, "Resume “Publicar la versión”");
    }
    state.interactions.insert(interaction.id.clone(), interaction);
    state
}

fn fixture_options(kind: SystemCardKind, body: &str) -> Vec<QuestionOption> {
    use SystemCardKind::*;
    let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
    match kind {
        NodePause => vec![option(alondra_domain_rules::NODE_PAUSE_RESUME, "Resume")],
        OverPace => vec![option("pause_until_tomorrow", "Pause until tomorrow"), option("keep_going", "Keep going")],
        PaceResume => vec![option("resume_now", "Resume now"), option("stay_paused", "Stay paused")],
        Machine => vec![option(alondra_core::machine_incident::MACHINE_FIXED_OPTION, alondra_core::machine_incident::MACHINE_FIXED_LABEL)],
        LocalNetwork => vec![option(alondra_domain_rules::local_network_card::LOCAL_NETWORK_ALLOWED_OPTION, alondra_domain_rules::local_network_card::LOCAL_NETWORK_ALLOWED_LABEL)],
        McpWait => vec![option(alondra_domain_rules::MCP_GO_ON_WITHOUT, "Go on without example")],
        ClaudeSignedOut => vec![option(alondra_domain_rules::SIGNED_IN, alondra_domain_rules::SIGNED_IN_LABEL)],
        ClaudeOutage | ClaudeUsage => vec![option(alondra_domain_rules::TRY_NOW, alondra_domain_rules::TRY_NOW_LABEL)],
        PostMergeProblem => vec![option(alondra_domain_rules::POST_MERGE_CLOSE, "Close the node"), option(alondra_domain_rules::POST_MERGE_LEAVE, "Leave it")],
        LaunchProblem => vec![option(alondra_domain_rules::LAUNCH_RETRY_OPTION, "Try again"), option("seen", "Skip it")],
        StartProblem => alondra_domain_rules::start_problem::options(&alondra_domain_rules::start_problem::from_body(body).unwrap(), Lang::En),
        CloseProblem => alondra_domain_rules::close_problem::CloseProblem::from_body(body).unwrap().options().0,
        DiscardWorktrees => alondra_domain_rules::discard_worktrees::DiscardWorktrees::from_body(body).unwrap().options().0,
        BatchProblem => vec![option(alondra_domain_rules::orchestrator_batch::BATCH_CONTINUE, alondra_domain_rules::orchestrator_batch::BATCH_CONTINUE_LABEL), option(alondra_domain_rules::orchestrator_batch::BATCH_RETRY, alondra_domain_rules::orchestrator_batch::BATCH_RETRY_LABEL)],
        StuckProblem => vec![option(alondra_domain_rules::orchestrator_cards::STUCK_RETRY, alondra_domain_rules::orchestrator_cards::STUCK_RETRY_LABEL)],
        Inbox => vec![option(alondra_domain_rules::orchestrator_cards::UNSPLIT_RETRY, alondra_domain_rules::orchestrator_cards::UNSPLIT_RETRY_LABEL)],
        _ => vec![option(alondra_domain_rules::CARRY_ON, alondra_domain_rules::CARRY_ON_LABEL)],
    }
}

fn stored_title(kind: SystemCardKind) -> &'static str {
    use SystemCardKind::*;
    match kind {
        NodePause => "Resume “Publicar la versión”",
        OverPace => "Today is over its share of the weekly Claude usage",
        PaceResume => "Resume Claude work now?",
        Machine => "The disk is nearly full",
        LocalNetwork => "Allow Alondra to use this Mac's local network",
        McpWait => "Sign in to example: a task waits for it",
        ClaudeSignedOut => "Sign in to Claude outside Alondra",
        ClaudeOutage => "Claude's API is not answering: a task waits",
        ClaudeUsage => "Claude's usage limit is reached: a task waits",
        HostLost => "“Publicar la versión” stopped: Alondra could not resume its session",
        TurnEnded => "“Publicar la versión” stopped without delivering or asking: tell it how to go on",
        SessionEnded => "“Publicar la versión” was interrupted when its session closed: tell it how to go on",
        HandoffDoubt => "“Publicar la versión” closed before confirming it read your message: send it again?",
        DecisionCancelled => "“Publicar la versión” stopped waiting for your decision: tell it how to go on",
        Dropped => "Claude didn't get to read your message",
        Unheard => "“Publicar la versión” stopped without a word",
        EndedUnanswered => "No longer waits for your answer: Publicar la versión",
        StartProblem => "Relaunch “Publicar la versión” or cancel it",
        PostMergeProblem => "Decide whether to close the session",
        CloseProblem => "Alondra can't close the session of “Publicar la versión”",
        DiscardWorktrees => "Delete the worktree of “Publicar la versión”?",
        OpenPrDelivery => "Review PR #12 of “Publicar la versión”",
        LaunchProblem => "Scheduled task \"Publicar la versión\" could not start",
        BatchProblem => "Alondra stopped halfway through handling your requests",
        StuckProblem => "Alondra cannot finish processing your requests",
        RequestDone => "Done",
        RequestTasks => "I created the task “Publicar la versión”",
        Inbox => "“Publicar la versión” was not turned into tasks",
        Stalled | RefusedCards | Unknown => unreachable!("internal cards have no client fixture"),
    }
}

#[test]
fn every_client_visible_system_card_is_worded_in_the_viewers_language() {
    let mut failures = vec![];
    for kind in SystemCardKind::ALL {
        if !CardType::System(kind).contract().readers.contains(&Reader::User) { continue; }
        let state = system_fixture(kind);
        if kind == SystemCardKind::NodePause {
            let index = alondra_domain_rules::ReadIndex::build(&state);
            assert!(index.waiting().iter().all(|item| item.id != "interaction:i-1"), "the task row carries the pause; its backing card never waits for the user");
            continue;
        }
        for client in [Client::Tui, Client::Ios, Client::Mac] {
            let english = render(&state, &LiveSnapshot::default(), client, Lang::En, "card:i-1", "");
            let spanish = render(&state, &LiveSnapshot::default(), client, Lang::Es, "card:i-1", "");
            let (english, spanish) = (card_of(&english), card_of(&spanish));
            if english.title == spanish.title {
                failures.push(format!("{kind:?} {client:?}: title stays English: {}", spanish.title));
            }
            if english.why.as_deref().is_some_and(|why| !why.is_empty()) && english.why == spanish.why
                && !matches!(kind, SystemCardKind::RequestDone | SystemCardKind::LaunchProblem)
            {
                failures.push(format!("{kind:?} {client:?}: explanation stays English: {:?}", spanish.why));
            }
            for action in english.actions.iter().chain(&english.more).filter(|action| action.option_id.is_some()) {
                if let Some(translated) = spanish.actions.iter().chain(&spanish.more).find(|translated| translated.option_id == action.option_id) {
                    if translated.label == action.label {
                        failures.push(format!("{kind:?} {client:?}: button stays English: {}", translated.label));
                    }
                    if action.description.is_some() && translated.description == action.description {
                        failures.push(format!("{kind:?} {client:?}: button explanation stays English: {:?}", translated.description));
                    }
                } else {
                    failures.push(format!("{kind:?} {client:?}: Spanish lost the {} action", action.label));
                }
            }
            match kind {
                SystemCardKind::RequestDone => assert_eq!(spanish.title, "Hecho", "{client:?}"),
                SystemCardKind::RequestTasks => assert!(spanish.title.starts_with("He creado la tarea"), "{client:?}: {spanish:?}"),
                SystemCardKind::ClaudeUsage => assert!(spanish.title.contains("límite"), "{client:?}: {spanish:?}"),
                _ => {}
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
