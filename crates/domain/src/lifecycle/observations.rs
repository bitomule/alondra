use super::*;

impl TaskLifecycle {
    pub(super) fn runtime_fact(
        &mut self,
        stamp: ExecutionStamp,
        cursor: u64,
        fact: RuntimeFact,
    ) -> Result<(), Refusal> {
        let previous = self.session().ok_or(Refusal::StaleExecution)?;
        if previous.stamp != stamp {
            return Err(Refusal::StaleExecution);
        }
        if cursor <= previous.cursor {
            return Err(Refusal::DuplicateFact);
        }
        if previous.cursor.checked_add(1) != Some(cursor) {
            return Err(Refusal::CursorGap);
        }
        let mut execution = previous.clone();
        execution.cursor = cursor;
        let mut forced_card = None;
        let mut aborted_callback = None;
        let mut requested_card = None;
        let mut resting_card = None;
        let mut resume_instead = None;
        let mut ended_callbacks = Vec::new();
        // Claude went on by itself: the cards that asked the user how it goes on no longer hold.
        let mut unprompted = false;
        if execution.resource != ResourceEvidence::Available
            && matches!(
                fact,
                RuntimeFact::Ask { .. }
                    | RuntimeFact::AnsweredByPolicy { .. }
                    | RuntimeFact::TurnStarted { .. }
                    | RuntimeFact::WorkStarted { .. }
            )
        {
            return Err(Refusal::InvalidState);
        }
        match fact {
            RuntimeFact::Ask {
                card,
                callback,
                reason,
            } => {
                valid_id(&card.0)?;
                if !matches!(reason, WaitReason::Question | WaitReason::Permission) {
                    return Err(Refusal::InvalidState);
                }
                if execution
                    .callbacks
                    .iter()
                    .any(|state| callback_id(state) == &callback)
                {
                    return Err(Refusal::DuplicateObligation);
                }
                execution
                    .callbacks
                    .push(CallbackState::AwaitingAnswer(callback.clone()));
                requested_card = Some(Card {
                    id: card,
                    reason,
                    callback: Some(callback),
                });
            }
            RuntimeFact::AnsweredByPolicy { callback, decision } => {
                valid_decision(&decision)?;
                if execution
                    .callbacks
                    .iter()
                    .any(|state| callback_id(state) == &callback)
                {
                    return Err(Refusal::DuplicateObligation);
                }
                execution
                    .callbacks
                    .push(CallbackState::Answered(callback.clone()));
                add_callback_wake(&mut execution.activity, callback)?;
            }
            RuntimeFact::ResourceAvailable => {
                if execution.resource != ResourceEvidence::Reserved {
                    return Err(Refusal::InvalidState);
                }
                let Activity::Launching(input) = &execution.activity else {
                    return Err(Refusal::InvalidState);
                };
                execution.activity = Activity::InputPending(input.clone());
                execution.resource = ResourceEvidence::Available;
            }
            RuntimeFact::InputConsumed { input } => {
                if execution.resource != ResourceEvidence::Available
                    || !matches!(execution.activity, Activity::Turn { .. })
                {
                    return Err(Refusal::InvalidState);
                }
                if execution.inbox.front() != Some(&input) {
                    return Err(Refusal::CorrelationMismatch);
                }
                execution.inbox.pop_front();
            }
            RuntimeFact::InputDropped { input, card } => {
                valid_id(&card.0)?;
                if execution.resource != ResourceEvidence::Available {
                    return Err(Refusal::InvalidState);
                }
                match &execution.activity {
                    Activity::InputPending(held) if *held == input => {
                        execution.activity = match execution.inbox.pop_front() {
                            Some(next) => Activity::InputPending(next),
                            None => Activity::Resting,
                        };
                    }
                    _ => {
                        if !execution.inbox.replace(&input, None) {
                            return Err(Refusal::UnknownObligation);
                        }
                    }
                }
                requested_card = Some(Card {
                    id: card,
                    reason: WaitReason::Dropped,
                    callback: None,
                });
            }
            RuntimeFact::TurnStarted { turn, trigger } => {
                if execution.resource != ResourceEvidence::Available {
                    return Err(Refusal::InvalidState);
                }
                let trigger_unprompted = trigger == Trigger::Unprompted;
                let (work, continuations) = match (&execution.activity, trigger) {
                    (Activity::InputPending(expected), Trigger::Input(input))
                        if expected == &input =>
                    {
                        (BTreeSet::new(), BTreeSet::new())
                    }
                    (Activity::Continuation(pending), Trigger::OwnedContinuation(id))
                        if pending.contains(&Wake::Owned(id.clone())) =>
                    {
                        let mut pending = pending.values();
                        pending.remove(&Wake::Owned(id));
                        (BTreeSet::new(), pending)
                    }
                    (
                        Activity::OwnWork {
                            work,
                            continuations,
                        },
                        Trigger::OwnedContinuation(id),
                    ) if continuations.contains(&Wake::Owned(id.clone())) => {
                        let mut pending = continuations.clone();
                        pending.remove(&Wake::Owned(id));
                        (work.values(), pending)
                    }
                    (
                        Activity::OwnWork {
                            work,
                            continuations,
                        },
                        Trigger::Input(input),
                    ) if execution.inbox.front() == Some(&input) => {
                        execution.inbox.pop_front();
                        (work.values(), continuations.clone())
                    }
                    (Activity::Continuation(pending), Trigger::Input(input))
                        if execution.inbox.front() == Some(&input) =>
                    {
                        execution.inbox.pop_front();
                        (BTreeSet::new(), pending.values())
                    }
                    (Activity::Resting, Trigger::Unprompted) => (BTreeSet::new(), BTreeSet::new()),
                    (Activity::InputPending(input), Trigger::Unprompted) => {
                        execution.inbox.push_front(input.clone());
                        (BTreeSet::new(), BTreeSet::new())
                    }
                    (Activity::Continuation(pending), Trigger::Unprompted) => (BTreeSet::new(), pending.values()),
                    (Activity::OwnWork { work, continuations }, Trigger::Unprompted) => (work.values(), continuations.clone()),
                    _ => return Err(Refusal::CorrelationMismatch),
                };
                unprompted = trigger_unprompted;
                execution.activity = Activity::Turn {
                    id: turn,
                    work,
                    continuations,
                    stop: TurnStop::Running,
                };
            }
            RuntimeFact::WorkStarted {
                turn,
                work: new_work,
            } => {
                let Activity::Turn { id, work, .. } = &mut execution.activity else {
                    return Err(Refusal::InvalidState);
                };
                if id != &turn {
                    return Err(Refusal::CorrelationMismatch);
                }
                if !work.insert(new_work) {
                    return Err(Refusal::DuplicateObligation);
                }
            }
            RuntimeFact::WorkEnded {
                work: ended,
                continuation,
            } => {
                let (mut work, mut continuations, root) = match &execution.activity {
                    Activity::Turn {
                        id,
                        work,
                        continuations,
                        stop,
                    } => (work.clone(), continuations.clone(), Some((id.clone(), *stop))),
                    Activity::OwnWork {
                        work,
                        continuations,
                    } => (work.values(), continuations.clone(), None),
                    _ => return Err(Refusal::InvalidState),
                };
                if !work.remove(&ended) {
                    return Err(Refusal::UnknownObligation);
                }
                if !continuations.insert(Wake::Owned(continuation)) {
                    return Err(Refusal::DuplicateObligation);
                }
                execution.activity = if let Some((id, stop)) = root {
                    Activity::Turn {
                        id,
                        work,
                        continuations,
                        stop,
                    }
                } else if !work.is_empty() {
                    Activity::OwnWork {
                        work: NonEmpty::new(work)?,
                        continuations,
                    }
                } else {
                    Activity::Continuation(NonEmpty::new(continuations)?)
                };
            }
            RuntimeFact::ContinuationConsumed { continuation } => {
                let (Activity::Turn { continuations, .. } | Activity::OwnWork { continuations, .. }) = &mut execution.activity else {
                    return Err(Refusal::InvalidState);
                };
                if !continuations.remove(&Wake::Owned(continuation)) {
                    return Err(Refusal::UnknownObligation);
                }
            }
            RuntimeFact::TurnEnded { turn, unheard_card } => {
                let Activity::Turn {
                    id,
                    work,
                    continuations,
                    ..
                } = &execution.activity
                else {
                    return Err(Refusal::InvalidState);
                };
                if id != &turn {
                    return Err(Refusal::CorrelationMismatch);
                }
                execution.activity = if !work.is_empty() {
                    Activity::OwnWork {
                        work: NonEmpty::new(work.clone())?,
                        continuations: continuations.clone(),
                    }
                } else if !continuations.is_empty() {
                    Activity::Continuation(NonEmpty::new(continuations.clone())?)
                } else {
                    Activity::Resting
                };
                valid_id(&unheard_card.0)?;
                resting_card = Some(unheard_card);
            }
            RuntimeFact::ConnectionLost { problem_card } => {
                valid_id(&problem_card.0)?;
                if execution.resource == ResourceEvidence::Released {
                    return Err(Refusal::InvalidState);
                }
                execution.resource = ResourceEvidence::ObservationPending;
                forced_card = Some(Card {
                    id: problem_card,
                    reason: WaitReason::Recovery,
                    callback: None,
                });
            }
            RuntimeFact::ResourceReleased { problem_card } => {
                valid_id(&problem_card.0)?;
                execution.resource = ResourceEvidence::Released;
                forced_card = Some(Card {
                    id: problem_card,
                    reason: WaitReason::Recovery,
                    callback: None,
                });
            }
            RuntimeFact::CallbackSettled {
                callback,
                continuation,
            } => {
                let index = execution
                    .callbacks
                    .iter()
                    .position(|state| *state == CallbackState::Answered(callback.clone()))
                    .ok_or(Refusal::UnknownObligation)?;
                execution.callbacks.remove(index);
                remove_callback_wake(&mut execution.activity, &callback)?;
                execution.activity = match &execution.activity {
                    Activity::Turn {
                        id,
                        work,
                        continuations,
                        stop,
                    } => {
                        let mut continuations = continuations.clone();
                        if !continuations.insert(Wake::Owned(continuation)) {
                            return Err(Refusal::DuplicateObligation);
                        }
                        Activity::Turn {
                            id: id.clone(),
                            work: work.clone(),
                            continuations,
                            stop: *stop,
                        }
                    }
                    Activity::OwnWork {
                        work,
                        continuations,
                    } => {
                        let mut continuations = continuations.clone();
                        if !continuations.insert(Wake::Owned(continuation)) {
                            return Err(Refusal::DuplicateObligation);
                        }
                        Activity::OwnWork {
                            work: work.clone(),
                            continuations,
                        }
                    }
                    Activity::Continuation(pending) => {
                        let mut pending = pending.values();
                        if !pending.insert(Wake::Owned(continuation)) {
                            return Err(Refusal::DuplicateObligation);
                        }
                        Activity::Continuation(NonEmpty::new(pending)?)
                    }
                    Activity::Resting => {
                        Activity::Continuation(NonEmpty::new([Wake::Owned(continuation)])?)
                    }
                    Activity::InputPending(input) => Activity::InputPending(input.clone()),
                    Activity::Launching(_) => return Err(Refusal::InvalidState),
                };
            }
            RuntimeFact::CallbackAborted {
                callback,
                unheard_card,
            } => {
                valid_id(&unheard_card.0)?;
                let index = execution
                    .callbacks
                    .iter()
                    .position(|state| callback_id(state) == &callback)
                    .ok_or(Refusal::UnknownObligation)?;
                execution.callbacks.remove(index);
                remove_callback_wake(&mut execution.activity, &callback)?;
                aborted_callback = Some((callback, unheard_card));
            }
            RuntimeFact::EffectAccepted { operation } => {
                if execution.resource != ResourceEvidence::Available {
                    return Err(Refusal::InvalidState);
                }
                if !execution.effects.insert(operation) {
                    return Err(Refusal::DuplicateObligation);
                }
            }
            RuntimeFact::InterruptAccepted { turn } => {
                if execution.resource != ResourceEvidence::Available {
                    return Err(Refusal::InvalidState);
                }
                let Activity::Turn { id, stop, .. } = &mut execution.activity else {
                    return Err(Refusal::InvalidState);
                };
                if id != &turn {
                    return Err(Refusal::CorrelationMismatch);
                }
                if *stop != TurnStop::Running {
                    return Err(Refusal::DuplicateObligation);
                }
                *stop = TurnStop::InterruptAccepted;
            }
            RuntimeFact::EffectObserved { operation } => {
                if !execution.effects.remove(&operation) {
                    return Err(Refusal::UnknownObligation);
                }
            }
            RuntimeFact::Notification => {}
            RuntimeFact::HostEnded { unheard_card, resume } => {
                valid_id(&unheard_card.0)?;
                if !matches!(execution.resource, ResourceEvidence::Reserved | ResourceEvidence::Available) {
                    return Err(Refusal::InvalidState);
                }
                let cut = matches!(
                    execution.activity,
                    Activity::Turn { .. } | Activity::OwnWork { .. } | Activity::Continuation(_)
                );
                ended_callbacks = execution.callbacks.drain(..).map(|state| callback_id(&state).clone()).collect();
                if cut {
                    execution.activity = Activity::Resting;
                }
                if cut || !ended_callbacks.is_empty() {
                    resting_card = Some(unheard_card);
                }
                if resume.as_ref().is_some_and(|input| execution.resource != ResourceEvidence::Available || self.settled.contains(input)) {
                    return Err(Refusal::InvalidState);
                }
                resume_instead = resume;
            }
        }
        let held = self
            .cards()
            .iter()
            .any(|(_, reason)| *reason == WaitReason::Pause)
            || matches!(self.state, State::InProgress(Progress::Dependency { .. }));
        if matches!(execution.activity, Activity::Resting)
            && execution.resource == ResourceEvidence::Available
            && !held
        {
            if let Some(input) = execution.inbox.pop_front() {
                execution.activity = Activity::InputPending(input);
            }
        }
        if forced_card.is_none()
            && requested_card.is_none()
            && matches!(execution.activity, Activity::Resting)
            && matches!(self.state, State::InProgress(Progress::Executing { .. }))
        {
            if let Some(input) = resume_instead.take() {
                queue_input(&mut execution, input)?;
            } else {
                let id = resting_card
                    .clone()
                    .or_else(|| aborted_callback.as_ref().map(|(_, id)| id.clone()))
                    .ok_or(Refusal::InvalidState)?;
                forced_card = Some(Card {
                    id,
                    reason: WaitReason::Unheard,
                    callback: None,
                });
            }
        }
        match &mut self.state {
            State::InProgress(Progress::Executing { stage, .. }) => {
                self.state = if let Some(card) = forced_card.or(requested_card) {
                    State::InProgress(Progress::Waiting {
                        stage: *stage,
                        cards: Decisions::one(card),
                        session: Session::Present(execution),
                    })
                } else {
                    State::InProgress(Progress::Executing {
                        stage: *stage,
                        execution: ActiveExecution::new(execution)?,
                    })
                };
            }
            State::InProgress(Progress::Waiting {
                stage,
                cards,
                session,
            }) => {
                let mut updated = cards.map();
                if unprompted {
                    updated.retain(|_, card| card.reason != WaitReason::Unheard);
                }
                if let Some((callback, _)) = &aborted_callback {
                    updated.retain(|_, card| card.callback.as_ref() != Some(callback));
                }
                updated.retain(|_, card| card.callback.as_ref().is_none_or(|callback| !ended_callbacks.contains(callback)));
                if let Some(card) = requested_card {
                    if updated.insert(card.id.clone(), card).is_some() {
                        return Err(Refusal::DuplicateObligation);
                    }
                }
                if let Some(card) = forced_card {
                    if updated
                        .get(&card.id)
                        .is_some_and(|existing| existing.reason != card.reason)
                    {
                        return Err(Refusal::DuplicateObligation);
                    }
                    updated.entry(card.id.clone()).or_insert(card);
                }
                if updated.is_empty() {
                    if let Some(input) = resume_instead.take().filter(|_| matches!(execution.activity, Activity::Resting)) {
                        queue_input(&mut execution, input)?;
                        self.state = State::InProgress(Progress::Executing {
                            stage: *stage,
                            execution: ActiveExecution::new(execution)?,
                        });
                    } else if matches!(execution.activity, Activity::Resting) {
                        let id = aborted_callback.map(|(_, id)| id).or(resting_card).ok_or(Refusal::InvalidState)?;
                        updated.insert(
                            id.clone(),
                            Card {
                                id,
                                reason: WaitReason::Unheard,
                                callback: None,
                            },
                        );
                        *cards = Decisions::new(updated)?;
                        *session = Session::Present(execution);
                    } else {
                        self.state = State::InProgress(Progress::Executing {
                            stage: *stage,
                            execution: ActiveExecution::new(execution)?,
                        });
                    }
                } else {
                    *cards = Decisions::new(updated)?;
                    *session = Session::Present(execution);
                }
            }
            State::InProgress(Progress::Dependency {
                decisions, session, ..
            }) => {
                if unprompted {
                    decisions.retain(|_, card| card.reason != WaitReason::Unheard);
                }
                if let Some((callback, _)) = aborted_callback {
                    decisions.retain(|_, card| card.callback.as_ref() != Some(&callback));
                }
                decisions.retain(|_, card| card.callback.as_ref().is_none_or(|callback| !ended_callbacks.contains(callback)));
                if let Some(card) = requested_card {
                    if decisions.insert(card.id.clone(), card).is_some() {
                        return Err(Refusal::DuplicateObligation);
                    }
                }
                if let Some(card) = forced_card {
                    if decisions
                        .get(&card.id)
                        .is_some_and(|existing| existing.reason != card.reason)
                    {
                        return Err(Refusal::DuplicateObligation);
                    }
                    decisions.entry(card.id.clone()).or_insert(card);
                }
                *session = Session::Present(execution);
            }
            State::Delivered(delivered) => {
                if unprompted {
                    delivered.decisions.retain(|_, card| card.reason != WaitReason::Unheard);
                }
                if let Some((callback, _)) = aborted_callback {
                    delivered
                        .decisions
                        .retain(|_, card| card.callback.as_ref() != Some(&callback));
                }
                delivered
                    .decisions
                    .retain(|_, card| card.callback.as_ref().is_none_or(|callback| !ended_callbacks.contains(callback)));
                if let Some(card) = requested_card {
                    if delivered.decisions.insert(card.id.clone(), card).is_some() {
                        return Err(Refusal::DuplicateObligation);
                    }
                }
                if let Some(card) = forced_card {
                    if delivered
                        .decisions
                        .get(&card.id)
                        .is_some_and(|existing| existing.reason != card.reason)
                    {
                        return Err(Refusal::DuplicateObligation);
                    }
                    delivered.decisions.entry(card.id.clone()).or_insert(card);
                }
                delivered.session = Session::Present(execution);
            }
            State::Finishing(_)
            | State::Unstarted(_)
            | State::InProgress(Progress::Queued { .. })
            | State::Completed
            | State::Cancelled => return Err(Refusal::InvalidState),
        }
        if resume_instead.is_some() {
            return Err(Refusal::InvalidState);
        }
        Ok(())
    }
}
