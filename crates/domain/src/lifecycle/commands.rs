use super::*;

impl TaskLifecycle {
    pub(super) fn transition(&mut self, event: Event) -> Result<(), Refusal> {
        match event {
            Event::Admit {
                node,
                conversation,
                instance,
                host,
            } => {
                valid_id(&node.0)?;
                if !self.plan_reviews.is_empty() {
                    return Err(Refusal::PlanReviewPending);
                }
                if self
                    .cards()
                    .iter()
                    .any(|(_, reason)| *reason == WaitReason::Pause)
                {
                    return Err(Refusal::InvalidState);
                }
                let input = match &self.state {
                    State::Unstarted(input) => input.clone(),
                    State::InProgress(Progress::Queued { input, .. }) => input.clone(),
                    State::Delivered(Delivered {
                        gate,
                        session: Session::Absent(pending),
                        ..
                    }) if !matches!(gate, DeliveryGate::Blocked(_)) => {
                        pending.front().cloned().ok_or(Refusal::EmptyObligation)?
                    }
                    // A released execution left only cards that ask how to go on: the new
                    // execution is how it goes on, and those cards end with its admission.
                    State::InProgress(Progress::Waiting {
                        cards,
                        session: Session::Absent(pending),
                        ..
                    }) if cards.iter().all(|card| matches!(card.reason, WaitReason::Unheard | WaitReason::Recovery)) => {
                        pending.front().cloned().ok_or(Refusal::EmptyObligation)?
                    }
                    _ => return Err(Refusal::InvalidState),
                };
                let stage = match &self.state {
                    State::InProgress(Progress::Queued { stage, .. } | Progress::Waiting { stage, .. }) => *stage,
                    State::Delivered(_) => WorkStage::Implement,
                    _ => WorkStage::Plan,
                };
                self.generation = self
                    .generation
                    .checked_add(1)
                    .ok_or(Refusal::GenerationExhausted)?;
                let stamp = ExecutionStamp::new(
                    self.task.clone(),
                    node,
                    conversation,
                    instance,
                    host,
                    self.generation,
                )?;
                let execution = Execution {
                    stamp,
                    cursor: 0,
                    resource: ResourceEvidence::Reserved,
                    activity: Activity::Launching(input),
                    callbacks: vec![],
                    effects: BTreeSet::new(),
                    inbox: match &self.state {
                        State::InProgress(Progress::Queued { remaining, .. }) => remaining.clone(),
                        State::Delivered(Delivered {
                            session: Session::Absent(pending),
                            ..
                        })
                        | State::InProgress(Progress::Waiting {
                            session: Session::Absent(pending),
                            ..
                        }) => {
                            let mut inbox = pending.clone();
                            inbox.pop_front();
                            inbox
                        }
                        _ => Inputs::new(),
                    },
                };
                if let State::Delivered(delivered) = &mut self.state {
                    delivered.session = Session::Present(execution);
                } else {
                    self.state = State::InProgress(Progress::Executing {
                        stage,
                        execution: ActiveExecution::new(execution)?,
                    });
                }
            }
            Event::Runtime {
                stamp,
                cursor,
                fact,
            } => self.runtime_fact(stamp, cursor, fact)?,
            Event::Ask { card } => {
                valid_id(&card.0)?;
                self.wait_on_card(card, WaitReason::Question, Owner::User)?;
            }
            Event::Pause { card, owner } => {
                if owner == Owner::Capacity(Capacity::Gate) {
                    return Err(Refusal::InvalidState);
                }
                if self
                    .cards()
                    .iter()
                    .any(|(_, reason)| *reason == WaitReason::Pause)
                {
                    return Err(Refusal::DuplicateObligation);
                }
                valid_id(&card.0)?;
                self.wait_on_card(card, WaitReason::Pause, owner)?;
            }
            Event::DoubtHandoff { card, input } => {
                valid_id(&card.0)?;
                if self.doubts.contains_key(&card) || self.doubts.values().any(|held| held == &input) {
                    return Err(Refusal::DuplicateObligation);
                }
                if self.session().is_none() || !self.is_pending(&input) {
                    return Err(Refusal::UnknownObligation);
                }
                self.wait_on_card(card.clone(), WaitReason::Doubt, Owner::User)?;
                self.doubts.insert(card, input);
            }
            Event::SettleHandoff { card, resend, unheard_card } => self.settle_handoff(card, resend, unheard_card)?,
            Event::Discard { card, unheard_card, stalled } => self.discard(card, unheard_card, unheard_owner(stalled))?,
            Event::AwaitEnded { unheard_card, wake, stalled } => self.await_ended(unheard_card, wake, unheard_owner(stalled))?,
            Event::ResendUntaken { input, resend } => self.resend(&input, resend)?,
            Event::ResendDoubtful { input, resend } => {
                self.resend(&input, resend)?;
                self.settled.insert(input);
            }
            Event::Tell { input } => {
                if self.is_pending(&input) || self.settled.contains(&input) {
                    return Err(Refusal::DuplicateObligation);
                }
                match &mut self.state {
                    // Cards that only ask how the task goes on are answered by what the node is told.
                    State::InProgress(Progress::Waiting { stage, cards, session }) if cards.iter().all(|card| card.reason == WaitReason::Unheard) => {
                        let (stage, mut session) = (*stage, session.clone());
                        tell(&mut session, input)?;
                        self.state = State::InProgress(match session {
                            Session::Absent(mut pending) => Progress::Queued {
                                stage,
                                input: pending.pop_front().ok_or(Refusal::EmptyObligation)?,
                                remaining: pending,
                            },
                            Session::Present(execution) => Progress::Executing { stage, execution: ActiveExecution::new(execution)? },
                        });
                    }
                    State::InProgress(Progress::Executing { execution, .. }) => queue_input(&mut execution.0, input)?,
                    State::InProgress(Progress::Queued { remaining, .. }) => remaining.push_back(input),
                    State::InProgress(Progress::Waiting { session, .. } | Progress::Dependency { session, .. }) => tell(session, input)?,
                    State::Delivered(delivered) => {
                        delivered.decisions.retain(|_, card| card.reason != WaitReason::Unheard);
                        tell(&mut delivered.session, input)?;
                    }
                    State::Finishing(finishing) => tell(&mut finishing.session, input)?,
                    State::Unstarted(_) | State::Completed | State::Cancelled => return Err(Refusal::InvalidState),
                }
            }
            Event::Answer { card, input } => self.answer(card, Reply::Input(input), false)?,
            Event::Resume { card, input } => self.answer(card, Reply::Input(input), true)?,
            Event::EndPause { card } => self.answer(card, Reply::Nothing, true)?,
            Event::Decide { card, decision } => {
                valid_decision(&decision)?;
                self.answer(card, Reply::Decision(decision), false)?
            }
            Event::MoveCard { from, to } => {
                valid_id(&to.0)?;
                self.move_card(from, to)?;
            }
            Event::BlockDelivery {
                delivery,
                dependencies,
            } => {
                if dependencies.contains(&self.task) {
                    return Err(Refusal::SelfDependency);
                }
                for task in &dependencies {
                    valid_id(&task.0)?;
                }
                let tasks = NonEmpty::new(dependencies)?;
                let State::Delivered(delivered) = &mut self.state else {
                    return Err(Refusal::InvalidState);
                };
                if delivered.delivery != delivery || delivered.gate != DeliveryGate::Review {
                    return Err(Refusal::CorrelationMismatch);
                }
                delivered.gate = DeliveryGate::Blocked(tasks);
            }
            Event::DeliveryUnblocked { delivery } => {
                let State::Delivered(delivered) = &mut self.state else {
                    return Err(Refusal::InvalidState);
                };
                if delivered.delivery != delivery
                    || !matches!(delivered.gate, DeliveryGate::Blocked(_))
                {
                    return Err(Refusal::CorrelationMismatch);
                }
                delivered.gate = DeliveryGate::Review;
            }
            Event::PlanApproved => {
                let State::InProgress(progress) = &mut self.state else {
                    return Err(Refusal::InvalidState);
                };
                let stage = match progress {
                    Progress::Queued { stage, .. }
                    | Progress::Executing { stage, .. }
                    | Progress::Waiting { stage, .. }
                    | Progress::Dependency { stage, .. } => stage,
                };
                if *stage != WorkStage::Plan {
                    return Err(Refusal::InvalidState);
                }
                *stage = WorkStage::Implement;
            }
            Event::Present { delivery } => {
                valid_id(&delivery.0)?;
                let (session, decisions, gate) = match &self.state {
                    State::InProgress(Progress::Executing { execution, .. }) => (
                        Session::Present(execution.0.clone()),
                        BTreeMap::new(),
                        DeliveryGate::Review,
                    ),
                    // A delivery is what a stopped node was missing: the orchestrator's case about it ends.
                    State::InProgress(Progress::Waiting { cards, session, .. }) => {
                        let mut cards = cards.map();
                        cards.retain(|_, card| card.owner != Owner::Orchestrator);
                        (session.clone(), cards, DeliveryGate::Review)
                    }
                    State::InProgress(Progress::Dependency {
                        tasks,
                        decisions,
                        session,
                        ..
                    }) => (
                        session.clone(),
                        decisions.clone(),
                        DeliveryGate::Blocked(tasks.clone()),
                    ),
                    _ => return Err(Refusal::InvalidState),
                };
                self.state = State::Delivered(Delivered {
                    delivery,
                    gate,
                    decisions,
                    session,
                });
            }
            Event::Accept { delivery, merge } => {
                let State::Delivered(delivered) = &mut self.state else {
                    return Err(Refusal::InvalidState);
                };
                if delivered.delivery != delivery {
                    return Err(Refusal::CorrelationMismatch);
                }
                if delivered.gate != DeliveryGate::Review {
                    return Err(Refusal::InvalidState);
                }
                delivered.gate = merge.map_or(DeliveryGate::Accepted, DeliveryGate::AwaitingMerge);
            }
            Event::MergeObserved { operation } => {
                let State::Delivered(delivered) = &mut self.state else {
                    return Err(Refusal::InvalidState);
                };
                if delivered.gate != DeliveryGate::AwaitingMerge(operation) {
                    return Err(Refusal::CorrelationMismatch);
                }
                delivered.gate = DeliveryGate::Accepted;
            }
            Event::HandBack { delivery, input } => {
                let State::Delivered(delivered) = &self.state else {
                    return Err(Refusal::InvalidState);
                };
                if delivered.delivery != delivery || delivered.gate != DeliveryGate::Review {
                    return Err(Refusal::CorrelationMismatch);
                }
                let mut decisions = delivered.decisions.clone();
                decisions.retain(|_, card| card.reason != WaitReason::Unheard);
                self.state = if decisions.is_empty() {
                    progress_with_input(delivered.session.clone(), WorkStage::ChangesRequested, input)?
                } else {
                    waiting_with_input(delivered.session.clone(), WorkStage::ChangesRequested, decisions, input)?
                };
            }
            Event::WaitForDependencies { dependencies } => {
                if dependencies.contains(&self.task) {
                    return Err(Refusal::SelfDependency);
                }
                for task in &dependencies {
                    valid_id(&task.0)?;
                }
                let tasks = NonEmpty::new(dependencies)?;
                let (stage, decisions, session) = match &self.state {
                    State::Unstarted(input) => (
                        WorkStage::Plan,
                        BTreeMap::new(),
                        Session::Absent(Inputs::one(input.clone())),
                    ),
                    State::InProgress(Progress::Queued {
                        stage,
                        input,
                        remaining,
                    }) => (*stage, BTreeMap::new(), {
                        let mut pending = remaining.clone();
                        pending.push_front(input.clone());
                        Session::Absent(pending)
                    }),
                    State::InProgress(Progress::Executing { stage, execution }) => (
                        *stage,
                        BTreeMap::new(),
                        Session::Present(execution.0.clone()),
                    ),
                    State::InProgress(Progress::Waiting {
                        stage,
                        cards,
                        session,
                    }) => (*stage, cards.map(), session.clone()),
                    _ => return Err(Refusal::InvalidState),
                };
                self.state = State::InProgress(Progress::Dependency {
                    stage,
                    tasks,
                    decisions,
                    session,
                });
            }
            Event::DependenciesResolved { input } => {
                let State::InProgress(Progress::Dependency {
                    stage,
                    decisions,
                    session,
                    ..
                }) = &self.state
                else {
                    return Err(Refusal::InvalidState);
                };
                self.state = if decisions.is_empty() {
                    progress_with_input(session.clone(), *stage, input)?
                } else {
                    waiting_with_input(session.clone(), *stage, decisions.clone(), input)?
                };
            }
            Event::Finish {
                intent,
                cleanup,
                post_merge,
            } => self.finish(intent, cleanup, post_merge)?,
            Event::PostMergeDeclared { operation } => {
                if matches!(self.state, State::Unstarted(_) | State::Finishing(_) | State::Completed | State::Cancelled) {
                    return Err(Refusal::InvalidState);
                }
                if self.post_merge.is_some() {
                    return Err(Refusal::DuplicateObligation);
                }
                self.post_merge = Some(operation);
            }
            Event::PostMergeEnded { operation, fact: _ } => {
                if self.post_merge.as_ref() == Some(&operation) {
                    self.post_merge = None;
                    if let State::Delivered(delivered) = &mut self.state {
                        delivered.decisions.retain(|_, card| card.reason != WaitReason::Unheard);
                    }
                } else {
                    self.discharge(DrainObligation::PostMerge(operation))?;
                }
            }
            Event::DrainObserved { obligation: DrainObligation::PostMerge(_) } => return Err(Refusal::DrainWithoutFact),
            Event::DrainObserved { obligation } => self.discharge(obligation)?,
            Event::RetireReleasedExecution {
                problem_card,
                input,
            } => {
                let (stage, mut cards, session, dependencies) = match &self.state {
                    State::InProgress(Progress::Waiting {
                        stage,
                        cards,
                        session,
                    }) => (*stage, cards.map(), session, None),
                    State::InProgress(Progress::Dependency {
                        stage,
                        tasks,
                        decisions,
                        session,
                    }) => (*stage, decisions.clone(), session, Some(tasks.clone())),
                    State::Delivered(delivered) => (
                        WorkStage::Implement,
                        delivered.decisions.clone(),
                        &delivered.session,
                        None,
                    ),
                    _ => return Err(Refusal::InvalidState),
                };
                let card = cards
                    .get(&problem_card)
                    .ok_or(Refusal::CorrelationMismatch)?;
                if card.reason != WaitReason::Recovery {
                    return Err(Refusal::CorrelationMismatch);
                }
                let execution = session.execution().ok_or(Refusal::InvalidState)?;
                if execution.resource != ResourceEvidence::Released
                    || execution.activity.owns_work()
                    || !execution.callbacks.is_empty()
                    || !execution.effects.is_empty()
                    || !execution_pending(execution).is_empty()
                {
                    return Err(Refusal::InvalidState);
                }
                cards.retain(|_, card| card.reason != WaitReason::Recovery);
                self.state = if let State::Delivered(delivered) = &self.state {
                    State::Delivered(Delivered {
                        delivery: delivered.delivery.clone(),
                        gate: delivered.gate.clone(),
                        decisions: cards,
                        session: Session::Absent(Inputs::one(input)),
                    })
                } else if let Some(tasks) = dependencies {
                    State::InProgress(Progress::Dependency {
                        stage,
                        tasks,
                        decisions: cards,
                        session: Session::Absent(Inputs::one(input)),
                    })
                } else if cards.is_empty() {
                    State::InProgress(Progress::Queued {
                        stage,
                        input,
                        remaining: Inputs::new(),
                    })
                } else {
                    State::InProgress(Progress::Waiting {
                        stage,
                        cards: Decisions::new(cards)?,
                        session: Session::Absent(Inputs::one(input)),
                    })
                };
            }
            Event::ExecutionReleased { input } => {
                let execution = self.session().ok_or(Refusal::InvalidState)?;
                if execution.activity.owns_work()
                    || !execution.callbacks.is_empty()
                    || !execution.effects.is_empty()
                {
                    return Err(Refusal::InvalidState);
                }
                let mut pending = execution_pending(execution);
                if pending.is_empty() {
                    pending.push_back(input);
                }
                self.state = match &self.state {
                    State::InProgress(Progress::Executing { stage, .. }) => {
                        let first = pending.pop_front().ok_or(Refusal::EmptyObligation)?;
                        State::InProgress(Progress::Queued {
                            stage: *stage,
                            input: first,
                            remaining: pending,
                        })
                    }
                    State::InProgress(Progress::Waiting { stage, cards, .. }) => {
                        State::InProgress(Progress::Waiting {
                            stage: *stage,
                            cards: cards.clone(),
                            session: Session::Absent(pending),
                        })
                    }
                    State::InProgress(Progress::Dependency {
                        stage,
                        tasks,
                        decisions,
                        ..
                    }) => State::InProgress(Progress::Dependency {
                        stage: *stage,
                        tasks: tasks.clone(),
                        decisions: decisions.clone(),
                        session: Session::Absent(pending),
                    }),
                    State::Delivered(delivered) => State::Delivered(Delivered {
                        delivery: delivered.delivery.clone(),
                        gate: delivered.gate.clone(),
                        decisions: delivered.decisions.clone(),
                        session: Session::Absent(pending),
                    }),
                    _ => return Err(Refusal::InvalidState),
                };
            }
            Event::Reopen { input } => {
                if !matches!(self.state, State::Completed | State::Cancelled) {
                    return Err(Refusal::InvalidState);
                }
                self.state = State::InProgress(Progress::Queued {
                    stage: WorkStage::Implement,
                    input,
                    remaining: Inputs::new(),
                });
            }
            Event::HoldForPlanReview { review } => {
                valid_id(&review.0)?;
                let not_admitted = matches!(self.state, State::Unstarted(_)
                    | State::InProgress(Progress::Queued { .. } | Progress::Dependency { session: Session::Absent(_), .. }));
                if !not_admitted {
                    return Err(Refusal::InvalidState);
                }
                if self.plan_reviews.insert(review, self.plan_revision).is_some() {
                    return Err(Refusal::DuplicateObligation);
                }
            }
            Event::PlanReviewSettled { review, plan_revision, outcome } => {
                if !self.plan_reviews.contains_key(&review) {
                    return Err(Refusal::UnknownObligation);
                }
                if plan_revision != self.plan_revision {
                    return Err(Refusal::StaleRevision);
                }
                self.plan_reviews.remove(&review);
                if outcome == PlanReviewOutcome::PlanChanged {
                    self.plan_revision = self.plan_revision.checked_add(1).ok_or(Refusal::RevisionExhausted)?;
                }
            }
            Event::RequestControl { control, kind } => {
                valid_id(control.as_str())?;
                if !matches!(self.state, State::InProgress(_) | State::Delivered(_)) {
                    return Err(Refusal::InvalidState);
                }
                let execution = self.session().ok_or(Refusal::InvalidState)?;
                if execution.resource != ResourceEvidence::Available {
                    return Err(Refusal::InvalidState);
                }
                match (&kind, &execution.activity) {
                    (Control::Interrupt { turn: Some(turn) }, Activity::Turn { id, .. }) if turn == id => {}
                    (Control::Interrupt { turn: None }, Activity::OwnWork { .. } | Activity::Continuation(_) | Activity::InputPending(_)) => {}
                    (Control::Interrupt { .. }, Activity::Turn { .. } | Activity::OwnWork { .. } | Activity::Continuation(_) | Activity::InputPending(_)) => {
                        return Err(Refusal::CorrelationMismatch);
                    }
                    (Control::Interrupt { .. }, _) => return Err(Refusal::InvalidState),
                    (Control::SetModel { model }, _) => valid_model(model)?,
                    (Control::SetPermissionMode { .. } | Control::ReadMcpStatus, _) => {}
                    (Control::ReconnectMcp { server } | Control::StartMcpSignIn { server } | Control::ClearMcpSignIn { server }, _) => valid_mcp_server(server)?,
                    (Control::ForceStop { turn }, Activity::Turn { id, stop, .. }) if turn == id => match stop {
                        TurnStop::InterruptAccepted => {}
                        TurnStop::Running => return Err(Refusal::InvalidState),
                        TurnStop::ForceRequested => return Err(Refusal::DuplicateObligation),
                    },
                    (Control::ForceStop { .. }, Activity::Turn { .. }) => return Err(Refusal::CorrelationMismatch),
                    (Control::ForceStop { .. }, _) => return Err(Refusal::InvalidState),
                }
                if let Control::ForceStop { .. } = kind
                    && let Some(Activity::Turn { stop, .. }) = self.session_mut().map(|execution| &mut execution.activity)
                {
                    *stop = TurnStop::ForceRequested;
                }
            }
            Event::Compact { input } => {
                if self
                    .cards()
                    .iter()
                    .any(|(_, reason)| *reason == WaitReason::Pause)
                {
                    return Err(Refusal::InvalidState);
                }
                let session = match &mut self.state {
                    State::InProgress(Progress::Waiting { session, .. }) => session,
                    State::Delivered(delivered) => &mut delivered.session,
                    _ => return Err(Refusal::InvalidState),
                };
                let execution = session.execution_mut().ok_or(Refusal::InvalidState)?;
                if execution.resource != ResourceEvidence::Available
                    || !execution.activity.rests()
                {
                    return Err(Refusal::InvalidState);
                }
                queue_input(execution, input)?;
            }
        }
        Ok(())
    }

    fn wait_on_card(&mut self, id: InteractionId, reason: WaitReason, owner: Owner) -> Result<(), Refusal> {
        let card = Card {
            id,
            reason,
            owner,
            callback: None,
        };
        let (stage, mut cards, session) = match &self.state {
            State::Unstarted(input) => (
                WorkStage::Plan,
                BTreeMap::new(),
                Session::Absent(Inputs::one(input.clone())),
            ),
            State::InProgress(Progress::Queued {
                stage,
                input,
                remaining,
            }) => (*stage, BTreeMap::new(), {
                let mut pending = remaining.clone();
                pending.push_front(input.clone());
                Session::Absent(pending)
            }),
            State::InProgress(Progress::Executing { stage, execution }) => (
                *stage,
                BTreeMap::new(),
                Session::Present(execution.0.clone()),
            ),
            State::InProgress(Progress::Waiting {
                stage,
                cards,
                session,
            }) => (*stage, cards.map(), session.clone()),
            State::InProgress(Progress::Dependency {
                stage,
                decisions,
                session,
                ..
            }) => (*stage, decisions.clone(), session.clone()),
            State::Delivered(delivered) => (
                WorkStage::Implement,
                delivered.decisions.clone(),
                delivered.session.clone(),
            ),
            _ => return Err(Refusal::InvalidState),
        };
        if cards.contains_key(&card.id) {
            return Err(Refusal::DuplicateObligation);
        }
        cards.insert(card.id.clone(), card);
        if let State::Delivered(delivered) = &mut self.state {
            delivered.decisions = cards;
            delivered.session = session;
        } else if let State::InProgress(Progress::Dependency {
            decisions,
            session: current,
            ..
        }) = &mut self.state
        {
            *decisions = cards;
            *current = session;
        } else {
            self.state = State::InProgress(Progress::Waiting {
                stage,
                cards: Decisions::new(cards)?,
                session,
            });
        }
        Ok(())
    }

    fn move_card(&mut self, from: InteractionId, to: InteractionId) -> Result<(), Refusal> {
        let (mut cards, session) = match &self.state {
            State::InProgress(Progress::Waiting { cards, session, .. }) => (cards.map(), session),
            State::InProgress(Progress::Dependency { decisions, session, .. }) => (decisions.clone(), session),
            State::Delivered(delivered) => (delivered.decisions.clone(), &delivered.session),
            _ => return Err(Refusal::InvalidState),
        };
        if cards.contains_key(&to) {
            return Err(Refusal::DuplicateObligation);
        }
        let card = cards.remove(&from).ok_or(Refusal::CorrelationMismatch)?;
        let callback = card.callback.clone().ok_or(Refusal::InvalidState)?;
        let awaiting = session.execution().is_some_and(|execution| {
            execution.callbacks.contains(&CallbackState::AwaitingAnswer(callback))
        });
        if !awaiting {
            return Err(Refusal::InvalidState);
        }
        cards.insert(to.clone(), Card { id: to, ..card });
        match &mut self.state {
            State::InProgress(Progress::Waiting { cards: held, .. }) => *held = Decisions::new(cards)?,
            State::InProgress(Progress::Dependency { decisions, .. }) => *decisions = cards,
            State::Delivered(delivered) => delivered.decisions = cards,
            _ => return Err(Refusal::InvalidState),
        }
        Ok(())
    }

    fn answer(&mut self, id: InteractionId, reply: Reply, resume: bool) -> Result<(), Refusal> {
        let (stage, mut cards, mut session) = match &self.state {
            State::InProgress(Progress::Waiting {
                stage,
                cards,
                session,
            }) => (*stage, cards.map(), session.clone()),
            State::InProgress(Progress::Dependency {
                stage,
                decisions,
                session,
                ..
            }) => (*stage, decisions.clone(), session.clone()),
            State::Delivered(delivered) => (
                WorkStage::Implement,
                delivered.decisions.clone(),
                delivered.session.clone(),
            ),
            _ => return Err(Refusal::InvalidState),
        };
        let card = cards.remove(&id).ok_or(Refusal::CorrelationMismatch)?;
        if (card.reason == WaitReason::Pause) != resume {
            return Err(Refusal::InvalidState);
        }
        if matches!(card.reason, WaitReason::Recovery | WaitReason::Doubt) {
            return Err(Refusal::InvalidState);
        }
        let input = match (&card.callback, reply) {
            (Some(callback), Reply::Decision(decision)) => {
                if !decides(card.reason, &decision) {
                    return Err(Refusal::InvalidDecision);
                }
                let execution = session.execution_mut().ok_or(Refusal::InvalidState)?;
                if execution.resource != ResourceEvidence::Available {
                    return Err(Refusal::InvalidState);
                }
                let state = execution
                    .callbacks
                    .iter_mut()
                    .find(|state| callback_id(state) == callback)
                    .ok_or(Refusal::UnknownObligation)?;
                if *state != CallbackState::AwaitingAnswer(callback.clone()) {
                    return Err(Refusal::InvalidState);
                }
                *state = CallbackState::Answered(callback.clone());
                add_callback_wake(&mut execution.activity, callback.clone())?;
                None
            }
            (None, Reply::Input(input)) => Some(input),
            (None, Reply::Nothing) => None,
            (Some(_), Reply::Input(_) | Reply::Nothing) | (None, Reply::Decision(_)) => return Err(Refusal::InvalidState),
        };
        match (input, &mut session) {
            (Some(input), Session::Present(execution)) => {
                if cards.values().any(|card| card.reason == WaitReason::Pause)
                    || !(execution.resource == ResourceEvidence::Available || (execution.resource == ResourceEvidence::Reserved && matches!(execution.activity,Activity::Launching(_))))
                {
                    return Err(Refusal::InvalidState);
                }
                queue_input(execution, input)?;
            }
            (Some(input), Session::Absent(pending)) => {
                if pending.contains(&input) {
                    return Err(Refusal::DuplicateObligation);
                }
                pending.push_back(input);
            }
            (None, Session::Present(execution))
                if resume
                    && !cards.values().any(|card| card.reason == WaitReason::Pause)
                    && execution.resource == ResourceEvidence::Available
                    && execution.activity.rests() =>
            {
                if let Some(input) = execution.inbox.pop_front() {
                    execution.activity = Activity::InputPending(input);
                }
            }
            (None, _) => {}
        }
        if let State::Delivered(delivered) = &mut self.state {
            delivered.decisions = cards;
            delivered.session = session;
        } else if let State::InProgress(Progress::Dependency {
            decisions,
            session: current,
            ..
        }) = &mut self.state
        {
            *decisions = cards;
            *current = session;
        } else if !cards.is_empty() {
            self.state = State::InProgress(Progress::Waiting {
                stage,
                cards: Decisions::new(cards)?,
                session,
            });
        } else {
            self.state = match session {
                Session::Absent(mut pending) => {
                    let first = pending.pop_front().ok_or(Refusal::EmptyObligation)?;
                    State::InProgress(Progress::Queued {
                        stage,
                        input: first,
                        remaining: pending,
                    })
                }
                Session::Present(execution) => State::InProgress(Progress::Executing {
                    stage,
                    execution: ActiveExecution::new(execution)?,
                }),
            };
        }
        Ok(())
    }

    /// `resend` takes the place of the pending `input`, which no card holds, for the next host.
    fn resend(&mut self, input: &InputId, resend: InputId) -> Result<(), Refusal> {
        if self.doubts.values().any(|held| held == input) {
            return Err(Refusal::InvalidState);
        }
        if self.is_pending(&resend) || self.settled.contains(&resend) {
            return Err(Refusal::DuplicateObligation);
        }
        let execution = match &mut self.state {
            State::InProgress(Progress::Executing { execution, .. }) => &mut execution.0,
            State::InProgress(Progress::Waiting { session, .. } | Progress::Dependency { session, .. }) => {
                session.execution_mut().ok_or(Refusal::InvalidState)?
            }
            State::Delivered(delivered) => delivered.session.execution_mut().ok_or(Refusal::InvalidState)?,
            _ => return Err(Refusal::InvalidState),
        };
        replace_handoff(execution, input, Some(resend))
    }

    fn settle_handoff(&mut self, id: InteractionId, resend: Option<InputId>, unheard: InteractionId) -> Result<(), Refusal> {
        valid_id(&unheard.0)?;
        let input = self.doubts.get(&id).cloned().ok_or(Refusal::CorrelationMismatch)?;
        if resend.as_ref().is_some_and(|resend| self.is_pending(resend) || self.settled.contains(resend)) {
            return Err(Refusal::DuplicateObligation);
        }
        let (stage, mut cards, mut session) = match &self.state {
            State::InProgress(Progress::Waiting { stage, cards, session }) => (*stage, cards.map(), session.clone()),
            State::InProgress(Progress::Dependency { stage, decisions, session, .. }) => (*stage, decisions.clone(), session.clone()),
            State::Delivered(delivered) => (WorkStage::Implement, delivered.decisions.clone(), delivered.session.clone()),
            _ => return Err(Refusal::InvalidState),
        };
        if cards.remove(&id).is_none_or(|card| card.reason != WaitReason::Doubt) {
            return Err(Refusal::CorrelationMismatch);
        }
        match &mut session {
            Session::Present(execution) => replace_handoff(execution, &input, resend)?,
            Session::Absent(pending) => {
                if !pending.replace(&input, resend) {
                    return Err(Refusal::UnknownObligation);
                }
            }
        }
        self.doubts.remove(&id);
        self.settled.insert(input);
        self.leave_card(stage, cards, session, unheard, Owner::User)
    }

    fn discard(&mut self, id: InteractionId, unheard: InteractionId, owner: Owner) -> Result<(), Refusal> {
        valid_id(&unheard.0)?;
        let (stage, mut cards, session) = match &self.state {
            State::InProgress(Progress::Waiting { stage, cards, session }) => (*stage, cards.map(), session.clone()),
            State::InProgress(Progress::Dependency { stage, decisions, session, .. }) => (*stage, decisions.clone(), session.clone()),
            State::Delivered(delivered) => (WorkStage::Implement, delivered.decisions.clone(), delivered.session.clone()),
            _ => return Err(Refusal::InvalidState),
        };
        if cards.remove(&id).is_none_or(|card| card.reason != WaitReason::Dropped) {
            return Err(Refusal::CorrelationMismatch);
        }
        self.leave_card(stage, cards, session, unheard, owner)
    }

    fn await_ended(&mut self, unheard: InteractionId, wake: Option<InputId>, owner: Owner) -> Result<(), Refusal> {
        valid_id(&unheard.0)?;
        if wake.as_ref().is_some_and(|input| self.settled.contains(input)) {
            return Err(Refusal::InvalidState);
        }
        let execution = self.session_mut().ok_or(Refusal::InvalidState)?;
        if !matches!(execution.activity, Activity::Awaiting(_)) {
            return Err(Refusal::InvalidState);
        }
        execution.activity = Activity::Resting;
        let State::InProgress(Progress::Executing { stage, execution }) = &self.state else {
            return if wake.is_some() { Err(Refusal::InvalidState) } else { Ok(()) };
        };
        let (stage, mut execution) = (*stage, execution.0.clone());
        let woken = match wake {
            Some(_) if self.woken.is_some() => return Err(Refusal::InvalidState),
            wake => wake,
        };
        self.state = match &woken {
            Some(input) => {
                queue_input(&mut execution, input.clone())?;
                State::InProgress(Progress::Executing { stage, execution: ActiveExecution::new(execution)? })
            }
            None => State::InProgress(Progress::Waiting {
                stage,
                cards: Decisions::one(Card { id: unheard, reason: WaitReason::Unheard, owner, callback: None }),
                session: Session::Present(execution),
            }),
        };
        self.woken = woken;
        Ok(())
    }

    /// The task without the card it no longer waits on: the cards left, else what it goes on with,
    /// else `unheard`, the card that asks how it goes on.
    fn leave_card(&mut self, stage: WorkStage, cards: BTreeMap<InteractionId, Card>, session: Session, unheard: InteractionId, owner: Owner) -> Result<(), Refusal> {
        if let State::Delivered(delivered) = &mut self.state {
            delivered.decisions = cards;
            delivered.session = session;
        } else if let State::InProgress(Progress::Dependency { decisions, session: current, .. }) = &mut self.state {
            *decisions = cards;
            *current = session;
        } else if !cards.is_empty() {
            self.state = State::InProgress(Progress::Waiting { stage, cards: Decisions::new(cards)?, session });
        } else {
            self.state = match session {
                Session::Absent(mut pending) => {
                    let first = pending.pop_front().ok_or(Refusal::EmptyObligation)?;
                    State::InProgress(Progress::Queued { stage, input: first, remaining: pending })
                }
                Session::Present(execution) if matches!(execution.activity, Activity::Resting) => State::InProgress(Progress::Waiting {
                    stage,
                    cards: Decisions::one(Card { id: unheard, reason: WaitReason::Unheard, owner, callback: None }),
                    session: Session::Present(execution),
                }),
                Session::Present(execution) => State::InProgress(Progress::Executing { stage, execution: ActiveExecution::new(execution)? }),
            };
        }
        Ok(())
    }

    /// Takes `obligation` off what the ending still owes; the last one ends the task.
    fn discharge(&mut self, obligation: DrainObligation) -> Result<(), Refusal> {
        let State::Finishing(finishing) = &mut self.state else {
            return Err(Refusal::InvalidState);
        };
        let mut remaining = finishing.remaining.values();
        if !remaining.remove(&obligation) {
            return Err(Refusal::UnknownObligation);
        }
        if let Some(execution) = finishing.session.execution_mut() {
            match &obligation {
                DrainObligation::Execution(_) => execution.resource = ResourceEvidence::Released,
                DrainObligation::Work(ended) => {
                    execution.activity = match &execution.activity {
                        Activity::Turn {
                            id,
                            work,
                            continuations,
                            taken,
                            stop,
                        } => {
                            let mut work = work.clone();
                            work.remove(ended);
                            Activity::Turn {
                                id: id.clone(),
                                work,
                                continuations: continuations.clone(),
                                taken: taken.clone(),
                                stop: *stop,
                            }
                        }
                        Activity::OwnWork {
                            work,
                            continuations,
                        } => {
                            let mut work = work.values();
                            work.remove(ended);
                            if work.is_empty() {
                                Activity::Resting
                            } else {
                                Activity::OwnWork {
                                    work: NonEmpty::new(work)?,
                                    continuations: continuations.clone(),
                                }
                            }
                        }
                        activity => activity.clone(),
                    };
                }
                DrainObligation::Callback(id) => {
                    execution.callbacks.retain(|state| callback_id(state) != id);
                    remove_callback_wake(&mut execution.activity, id)?;
                }
                DrainObligation::ExternalEffect(operation) => {
                    execution.effects.remove(operation);
                }
                DrainObligation::Merge(_)
                | DrainObligation::Cleanup(_)
                | DrainObligation::PostMerge(_) => {}
            }
        }
        if remaining.is_empty() {
            self.state = match finishing.intent {
                FinishIntent::Complete => State::Completed,
                FinishIntent::Cancel => State::Cancelled,
            };
        } else {
            finishing.remaining = NonEmpty::new(remaining)?;
        }
        Ok(())
    }

    fn finish(
        &mut self,
        intent: FinishIntent,
        cleanup: OperationId,
        post_merge: Option<OperationId>,
    ) -> Result<(), Refusal> {
        if matches!(
            self.state,
            State::Finishing(_) | State::Completed | State::Cancelled
        ) {
            return Err(Refusal::InvalidState);
        }
        if intent == FinishIntent::Complete
            && !matches!(&self.state, State::Delivered(delivered) if delivered.gate == DeliveryGate::Accepted)
        {
            return Err(Refusal::InvalidState);
        }
        if intent == FinishIntent::Complete
            && self
                .cards()
                .iter()
                .any(|(_, reason)| *reason != WaitReason::Recovery)
        {
            return Err(Refusal::InvalidState);
        }
        if intent == FinishIntent::Complete
            && self.session().is_some()
            && self.has_pending()
        {
            return Err(Refusal::InvalidState);
        }
        let session = self
            .session()
            .cloned()
            .map_or(Session::Absent(Inputs::new()), Session::Present);
        if intent == FinishIntent::Cancel {
            let doubted = std::mem::take(&mut self.doubts);
            self.settled.extend(doubted.into_values());
        }
        let mut remaining = BTreeSet::from([DrainObligation::Cleanup(cleanup)]);
        let declared = self.post_merge.take();
        remaining.extend(post_merge.into_iter().chain(declared).map(DrainObligation::PostMerge));
        if let Some(execution) = session.execution() {
            if execution.resource != ResourceEvidence::Released {
                remaining.insert(DrainObligation::Execution(execution.stamp.instance().clone()));
            }
            remaining.extend(
                execution
                    .activity
                    .work()
                    .into_iter()
                    .map(DrainObligation::Work),
            );
            remaining.extend(
                execution
                    .callbacks
                    .iter()
                    .map(|state| DrainObligation::Callback(callback_id(state).clone())),
            );
            remaining.extend(
                execution
                    .effects
                    .iter()
                    .cloned()
                    .map(DrainObligation::ExternalEffect),
            );
        }
        if let State::Delivered(Delivered {
            gate: DeliveryGate::AwaitingMerge(operation),
            ..
        }) = &self.state
        {
            remaining.insert(DrainObligation::Merge(operation.clone()));
        }
        self.state = State::Finishing(Finishing {
            intent,
            remaining: NonEmpty::new(remaining)?,
            session,
        });
        Ok(())
    }
}

/// Puts `resend` where the doubtful `input` waited, or takes `input` out of the execution's queue.
/// A launch input cannot be taken as received: nothing would be left to start the session.
fn replace_handoff(execution: &mut Execution, input: &InputId, resend: Option<InputId>) -> Result<(), Refusal> {
    match (&mut execution.activity, resend) {
        (Activity::Launching(held) | Activity::InputPending(held), Some(resend)) if held == input => *held = resend,
        (Activity::Launching(held), None) if held == input => return Err(Refusal::InvalidState),
        (Activity::InputPending(held), None) if held == input => {
            execution.activity = match execution.inbox.pop_front() {
                Some(next) => Activity::InputPending(next),
                None => Activity::Resting,
            };
        }
        (_, resend) => {
            if !execution.inbox.replace(input, resend) {
                return Err(Refusal::UnknownObligation);
            }
        }
    }
    Ok(())
}

/// What answers a card: the input a task goes on with, or the decision of the callback it waits on.
enum Reply {
    Input(InputId),
    Decision(Decision),
    Nothing,
}

/// Who settles the card of a node left with nothing explaining its next step.
fn unheard_owner(stalled: bool) -> Owner {
    if stalled { Owner::Orchestrator } else { Owner::User }
}
