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
                    _ => return Err(Refusal::InvalidState),
                };
                let stage = match &self.state {
                    State::InProgress(Progress::Queued { stage, .. }) => *stage,
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
                        }) => pending.iter().skip(1).cloned().collect(),
                        _ => VecDeque::new(),
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
                self.wait_on_card(card, WaitReason::Question)?;
            }
            Event::Pause { card } => {
                if self
                    .cards()
                    .iter()
                    .any(|(_, reason)| *reason == WaitReason::Pause)
                {
                    return Err(Refusal::DuplicateObligation);
                }
                valid_id(&card.0)?;
                self.wait_on_card(card, WaitReason::Pause)?;
            }
            Event::Answer { card, input } => self.answer(card, input, false)?,
            Event::Resume { card, input } => self.answer(card, input, true)?,
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
                    State::InProgress(Progress::Waiting { cards, session, .. }) => {
                        (session.clone(), cards.map(), DeliveryGate::Review)
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
                if !delivered.decisions.is_empty() {
                    return Err(Refusal::InvalidState);
                }
                self.state = progress_with_input(
                    delivered.session.clone(),
                    WorkStage::ChangesRequested,
                    input,
                )?;
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
                        Session::Absent(VecDeque::from([input.clone()])),
                    ),
                    State::InProgress(Progress::Queued {
                        stage,
                        input,
                        remaining,
                    }) => (
                        *stage,
                        BTreeMap::new(),
                        Session::Absent(
                            std::iter::once(input.clone())
                                .chain(remaining.iter().cloned())
                                .collect(),
                        ),
                    ),
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
                    let mut session = session.clone();
                    match &mut session {
                        Session::Absent(pending) => {
                            if pending.contains(&input) {
                                return Err(Refusal::DuplicateObligation);
                            }
                            pending.push_back(input);
                        }
                        Session::Present(execution) => {
                            if execution.resource != ResourceEvidence::Available {
                                return Err(Refusal::InvalidState);
                            }
                            if decisions
                                .values()
                                .any(|card| card.reason == WaitReason::Pause)
                            {
                                if execution.inbox.contains(&input) {
                                    return Err(Refusal::DuplicateObligation);
                                }
                                execution.inbox.push_back(input);
                            } else {
                                queue_input(execution, input)?;
                            }
                        }
                    }
                    State::InProgress(Progress::Waiting {
                        stage: *stage,
                        cards: Decisions::new(decisions.clone())?,
                        session,
                    })
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
                        session: Session::Absent(VecDeque::from([input])),
                    })
                } else if let Some(tasks) = dependencies {
                    State::InProgress(Progress::Dependency {
                        stage,
                        tasks,
                        decisions: cards,
                        session: Session::Absent(VecDeque::from([input])),
                    })
                } else if cards.is_empty() {
                    State::InProgress(Progress::Queued {
                        stage,
                        input,
                        remaining: VecDeque::new(),
                    })
                } else {
                    State::InProgress(Progress::Waiting {
                        stage,
                        cards: Decisions::new(cards)?,
                        session: Session::Absent(VecDeque::from([input])),
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
                let mut pending: VecDeque<InputId> =
                    execution_pending(execution).into_iter().cloned().collect();
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
                    remaining: VecDeque::new(),
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
        }
        Ok(())
    }

    fn wait_on_card(&mut self, id: InteractionId, reason: WaitReason) -> Result<(), Refusal> {
        let card = Card {
            id,
            reason,
            callback: None,
        };
        let (stage, mut cards, session) = match &self.state {
            State::Unstarted(input) => (
                WorkStage::Plan,
                BTreeMap::new(),
                Session::Absent(VecDeque::from([input.clone()])),
            ),
            State::InProgress(Progress::Queued {
                stage,
                input,
                remaining,
            }) => (
                *stage,
                BTreeMap::new(),
                Session::Absent(
                    std::iter::once(input.clone())
                        .chain(remaining.iter().cloned())
                        .collect(),
                ),
            ),
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

    fn answer(&mut self, id: InteractionId, input: InputId, resume: bool) -> Result<(), Refusal> {
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
        if card.reason == WaitReason::Recovery {
            return Err(Refusal::InvalidState);
        }
        if let Some(callback) = &card.callback {
            let execution = session.execution_mut().ok_or(Refusal::InvalidState)?;
            if execution.resource != ResourceEvidence::Available {
                return Err(Refusal::InvalidState);
            }
            let state = execution
                .callbacks
                .iter_mut()
                .find(|state| callback_id(state) == callback)
                .ok_or(Refusal::UnknownObligation)?;
            *state = CallbackState::Answered(callback.clone());
            add_callback_wake(&mut execution.activity, callback.clone())?;
        } else if let Some(execution) = session.execution_mut() {
            if cards.values().any(|card| card.reason == WaitReason::Pause)
                || execution.resource != ResourceEvidence::Available
            {
                return Err(Refusal::InvalidState);
            }
            queue_input(execution, input.clone())?;
        } else if let Session::Absent(pending) = &mut session {
            if pending.contains(&input) {
                return Err(Refusal::DuplicateObligation);
            }
            pending.push_back(input.clone());
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
                        } => {
                            let mut work = work.clone();
                            work.remove(ended);
                            Activity::Turn {
                                id: id.clone(),
                                work,
                                continuations: continuations.clone(),
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
            && !self.pending_inputs().is_empty()
        {
            return Err(Refusal::InvalidState);
        }
        let session = self
            .session()
            .cloned()
            .map_or(Session::Absent(VecDeque::new()), Session::Present);
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
