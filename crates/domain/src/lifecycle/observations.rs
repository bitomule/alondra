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
        let closing = matches!(self.state, State::Finishing(_));
        if closing && !matches!(fact, RuntimeFact::HostEnded { .. } | RuntimeFact::HostStopped { .. } | RuntimeFact::TakenOver | RuntimeFact::TakeoverUnanswered) {
            return self.unchanged(cursor);
        }
        // A fact that shows the session runs shows its resource is up, whatever came first.
        if matches!(
            fact,
            RuntimeFact::Ask { .. }
                | RuntimeFact::AnsweredByPolicy { .. }
                | RuntimeFact::TurnStarted { .. }
                | RuntimeFact::WorkStarted { .. }
                | RuntimeFact::InputConsumed { .. }
                | RuntimeFact::TurnEnded { .. }
        ) {
            if execution.resource == ResourceEvidence::Reserved {
                execution.resource = ResourceEvidence::Available;
            }
            if let Activity::Launching(input) = &execution.activity {
                execution.activity = Activity::InputPending(input.clone());
            }
        }
        let mut forced_card = None;
        let mut aborted_callback = None;
        let mut requested_card = None;
        let mut resting_card = None;
        let mut resume_instead = None;
        let mut wake_instead = None;
        let mut unheard_owner = Owner::User;
        let mut ended_callbacks = Vec::new();
        let mut host_end = None;
        let mut cut_by_end = false;
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
            return self.unchanged(cursor);
        }
        let stopped_host = matches!(&fact, RuntimeFact::HostStopped { .. });
        let host_resume = match &fact {
            RuntimeFact::HostEnded { resume, .. } => resume.clone(),
            _ => None,
        };
        let takeover_fact = matches!(fact, RuntimeFact::TakenOver | RuntimeFact::TakeoverUnanswered);
        // Only what the host itself shows answers a takeover: its end, a lost connection or a
        // released resource are what Alondra saw of it, not anything it did.
        let shown_by_host = !takeover_fact && !matches!(fact,
            RuntimeFact::HostEnded { .. } | RuntimeFact::HostStopped { .. } | RuntimeFact::ConnectionLost { .. } | RuntimeFact::ResourceReleased { .. });
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
                    return self.unchanged(cursor);
                }
                execution
                    .callbacks
                    .push(CallbackState::AwaitingAnswer(callback.clone()));
                requested_card = Some(Card {
                    id: card,
                    reason,
                    owner: Owner::User,
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
                    return self.unchanged(cursor);
                }
                execution
                    .callbacks
                    .push(CallbackState::Answered(callback.clone()));
                add_callback_wake(&mut execution.activity, callback)?;
            }
            RuntimeFact::ResourceAvailable => {
                if execution.resource != ResourceEvidence::Reserved {
                    return self.unchanged(cursor);
                }
                if let Activity::Launching(input) = &execution.activity {
                    execution.activity = Activity::InputPending(input.clone());
                }
                execution.resource = ResourceEvidence::Available;
            }
            RuntimeFact::InputConsumed { input } => {
                if execution.resource != ResourceEvidence::Available {
                    return self.unchanged(cursor);
                }
                if matches!(execution.activity, Activity::Turn { .. }) {
                    if !execution.inbox.replace(&input, None) {
                        return self.unchanged(cursor);
                    }
                } else {
                    let unseen = TurnId::new(format!("unseen-{cursor}")).map_err(|_| Refusal::EmptyIdentity)?;
                    execution.open_unseen_turn(unseen, &Trigger::Input(input))?;
                    unprompted = true;
                }
            }
            RuntimeFact::InputDropped { input, card } => {
                valid_id(&card.0)?;
                if execution.resource != ResourceEvidence::Available {
                    return self.unchanged(cursor);
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
                            return self.unchanged(cursor);
                        }
                    }
                }
                requested_card = Some(Card {
                    id: card,
                    reason: WaitReason::Dropped,
                    owner: Owner::User,
                    callback: None,
                });
            }
            RuntimeFact::TurnStarted { turn, trigger } => {
                if matches!(&execution.activity, Activity::Turn { id, .. } if *id == turn) {
                    return self.unchanged(cursor);
                }
                let trigger_unprompted = trigger == Trigger::Unprompted;
                let mut went_on_unexpected = false;
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
                    (Activity::Awaiting(_) | Activity::Resting, Trigger::Unprompted) => (BTreeSet::new(), BTreeSet::new()),
                    (Activity::InputPending(input), Trigger::Unprompted) => {
                        execution.inbox.push_front(input.clone());
                        (BTreeSet::new(), BTreeSet::new())
                    }
                    (Activity::Continuation(pending), Trigger::Unprompted) => (BTreeSet::new(), pending.values()),
                    (Activity::OwnWork { work, continuations }, Trigger::Unprompted) => (work.values(), continuations.clone()),
                    (_, trigger) => {
                        went_on_unexpected = true;
                        execution.provider_turn_started(&turn, &trigger)?
                    }
                };
                // Claude Code opens the turn of a notification before it reads anything else, and
                // that turn reads every notification waiting: whatever turn starts takes them up.
                // They stay pending until its end says what comes next, as journals before this
                // rule recorded them.
                let taken: BTreeSet<Wake> = continuations.iter().filter(|wake| matches!(wake, Wake::Owned(_))).cloned().collect();
                unprompted = trigger_unprompted || went_on_unexpected;
                execution.activity = Activity::Turn {
                    id: turn,
                    work,
                    continuations,
                    taken,
                    stop: TurnStop::Running,
                };
            }
            RuntimeFact::WorkStarted {
                turn,
                work: new_work,
            } => {
                if !matches!(&execution.activity, Activity::Turn { id, .. } if *id == turn) {
                    execution.open_unseen_turn(turn, &Trigger::Unprompted)?;
                    unprompted = true;
                }
                let Activity::Turn { work, .. } = &mut execution.activity else {
                    return Err(Refusal::InvalidState);
                };
                work.insert(new_work);
            }
            // Work the journal never saw start, or saw end, leaves nothing running to end: the
            // turn its notification opens is taken as it comes.
            RuntimeFact::WorkEnded { work: ended, .. } if !execution.runs_work(&ended) => {}
            RuntimeFact::WorkEnded {
                work: ended,
                continuation,
            } => {
                let (mut work, mut continuations, root) = match &execution.activity {
                    Activity::Turn {
                        id,
                        work,
                        continuations,
                        taken,
                        stop,
                    } => (work.clone(), continuations.clone(), Some((id.clone(), taken.clone(), *stop))),
                    Activity::OwnWork {
                        work,
                        continuations,
                    } => (work.values(), continuations.clone(), None),
                    _ => return Err(Refusal::InvalidState),
                };
                work.remove(&ended);
                continuations.insert(Wake::Owned(continuation));
                execution.activity = if let Some((id, taken, stop)) = root {
                    Activity::Turn {
                        id,
                        work,
                        continuations,
                        taken,
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
                let wake = Wake::Owned(continuation);
                match &mut execution.activity {
                    // A notification the journal never held pending was read: nothing waits on it.
                    Activity::Turn { continuations, taken, .. } => {
                        continuations.remove(&wake);
                        taken.remove(&wake);
                    }
                    Activity::OwnWork { continuations, .. } => {
                        continuations.remove(&wake);
                    }
                    _ => {}
                }
            }
            // The provider runs one root turn at a time: the end of the one it reports is the end
            // of the one the journal runs, whatever start of it went unseen.
            RuntimeFact::TurnEnded { turn, unheard_card, wake, awaits, stalled } => {
                let wake = wake.filter(|input| !self.settled.contains(input));
                if !matches!(execution.activity, Activity::Turn { .. }) {
                    execution.open_unseen_turn(turn, &Trigger::Unprompted)?;
                    unprompted = true;
                }
                let Activity::Turn {
                    work,
                    continuations,
                    taken,
                    ..
                } = &execution.activity
                else {
                    return Err(Refusal::InvalidState);
                };
                let continuations: BTreeSet<Wake> = if wake.is_some() || awaits.is_some() {
                    continuations.difference(taken).cloned().collect()
                } else {
                    continuations.clone()
                };
                execution.activity = if !work.is_empty() {
                    Activity::OwnWork {
                        work: NonEmpty::new(work.clone())?,
                        continuations: continuations.clone(),
                    }
                } else if !continuations.is_empty() {
                    Activity::Continuation(NonEmpty::new(continuations.clone())?)
                } else if let Some(awaits) = awaits {
                    match &awaits {
                        Await::User(card) | Await::Orchestrator(card) => valid_id(&card.0)?,
                        Await::Tasks(tasks) if tasks.iter().any(|task| *task == self.task) => return Err(Refusal::SelfDependency),
                        Await::Tasks(_) | Await::Machine | Await::Instruction => {}
                    }
                    self.woken = None;
                    Activity::Awaiting(awaits)
                } else {
                    Activity::Resting
                };
                valid_id(&unheard_card.0)?;
                resting_card = Some(unheard_card);
                wake_instead = wake;
                if stalled {
                    unheard_owner = Owner::Orchestrator;
                }
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
                    owner: Owner::User,
                    callback: None,
                });
            }
            RuntimeFact::ResourceReleased { problem_card } => {
                valid_id(&problem_card.0)?;
                execution.resource = ResourceEvidence::Released;
                forced_card = Some(Card {
                    id: problem_card,
                    reason: WaitReason::Recovery,
                    owner: Owner::User,
                    callback: None,
                });
            }
            RuntimeFact::CallbackSettled {
                callback,
                continuation,
            } => {
                let Some(index) = execution
                    .callbacks
                    .iter()
                    .position(|state| matches!(state, CallbackState::Answered(id) | CallbackState::Decided(id) if *id == callback))
                else {
                    return self.unchanged(cursor);
                };
                execution.callbacks.remove(index);
                remove_callback_wake(&mut execution.activity, &callback)?;
                execution.activity = match &execution.activity {
                    Activity::Turn {
                        id,
                        work,
                        continuations,
                        taken,
                        stop,
                    } => {
                        let mut continuations = continuations.clone();
                        continuations.insert(Wake::Owned(continuation));
                        Activity::Turn {
                            id: id.clone(),
                            work: work.clone(),
                            continuations,
                            taken: taken.clone(),
                            stop: *stop,
                        }
                    }
                    Activity::OwnWork {
                        work,
                        continuations,
                    } => {
                        let mut continuations = continuations.clone();
                        continuations.insert(Wake::Owned(continuation));
                        Activity::OwnWork {
                            work: work.clone(),
                            continuations,
                        }
                    }
                    Activity::Continuation(pending) => {
                        let mut pending = pending.values();
                        pending.insert(Wake::Owned(continuation));
                        Activity::Continuation(NonEmpty::new(pending)?)
                    }
                    Activity::Awaiting(_) | Activity::Resting => {
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
                let Some(index) = execution
                    .callbacks
                    .iter()
                    .position(|state| callback_id(state) == &callback)
                else {
                    return self.unchanged(cursor);
                };
                execution.callbacks.remove(index);
                remove_callback_wake(&mut execution.activity, &callback)?;
                aborted_callback = Some((callback, unheard_card));
            }
            RuntimeFact::EffectAccepted { operation } => {
                if execution.resource != ResourceEvidence::Available || !execution.effects.insert(operation) {
                    return self.unchanged(cursor);
                }
            }
            // The provider interrupts the one root turn it runs, whichever id the journal gave it.
            RuntimeFact::InterruptAccepted { .. } => {
                let Activity::Turn { stop, .. } = &mut execution.activity else {
                    return self.unchanged(cursor);
                };
                if *stop != TurnStop::Running {
                    return self.unchanged(cursor);
                }
                *stop = TurnStop::InterruptAccepted;
            }
            RuntimeFact::EffectObserved { operation } => {
                if !execution.effects.remove(&operation) {
                    return self.unchanged(cursor);
                }
            }
            RuntimeFact::Notification => {}
            RuntimeFact::TakenOver => {
                if execution.takeover == Takeover::Pending {
                    return Err(Refusal::RepeatedTakeover);
                }
                execution.takeover = Takeover::Pending;
            }
            RuntimeFact::TakeoverUnanswered => {
                if execution.takeover != Takeover::Pending {
                    return Err(Refusal::InvalidState);
                }
                execution.takeover = Takeover::Unanswered;
            }
            RuntimeFact::HostEnded { unheard_card, .. } | RuntimeFact::HostStopped { unheard_card } => {
                valid_id(&unheard_card.0)?;
                let unlaunched_stop = stopped_host
                    && execution.resource == ResourceEvidence::Reserved
                    && matches!(self.state, State::InProgress(Progress::Executing { .. }));
                if unlaunched_stop || !matches!(execution.resource, ResourceEvidence::Reserved | ResourceEvidence::Available) {
                    return self.unchanged(cursor);
                }
                let resume = host_resume.filter(|input| execution.resource == ResourceEvidence::Available && !self.settled.contains(input));
                host_end = Some(unheard_card.clone());
                let cut = matches!(
                    execution.activity,
                    Activity::Turn { .. } | Activity::OwnWork { .. } | Activity::Continuation(_)
                );
                ended_callbacks = execution.callbacks.drain(..).map(|state| callback_id(&state).clone()).collect();
                if stopped_host && self.cards().iter().any(|(_, reason)| *reason == WaitReason::Pause) {
                    execution.activity = Activity::Resting;
                } else if stopped_host {
                    execution.activity = Activity::Awaiting(Await::Instruction);
                } else if cut {
                    execution.activity = Activity::Resting;
                }
                cut_by_end = cut;
                if cut || !ended_callbacks.is_empty() {
                    resting_card = Some(unheard_card);
                }
                resume_instead = resume;
            }
        }
        if shown_by_host {
            execution.takeover = Takeover::None;
        }
        let held = self
            .cards()
            .iter()
            .any(|(_, reason)| *reason == WaitReason::Pause)
            || matches!(self.state, State::InProgress(Progress::Dependency { .. }));
        if execution.activity.rests()
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
                queue_unless_waiting(&mut execution, input)?;
            } else if let Some(input) = wake_instead.take_if(|_| self.woken.is_none()) {
                queue_unless_waiting(&mut execution, input.clone())?;
                self.woken = Some(input);
            } else {
                self.woken = None;
                let id = resting_card
                    .clone()
                    .or_else(|| aborted_callback.as_ref().map(|(_, id)| id.clone()))
                    .ok_or(Refusal::InvalidState)?;
                forced_card = Some(Card {
                    id,
                    reason: WaitReason::Unheard,
                    owner: unheard_owner,
                    callback: None,
                });
            }
        }
        let owes_post_merge = self.post_merge.is_some();
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
                        queue_unless_waiting(&mut execution, input)?;
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
                                owner: unheard_owner,
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
                // The work after the merge can only be reported by the session that ran it: once
                // that session ends, a resumed one goes on with it or the user is asked how it goes
                // on. What the session was doing for the user on the delivery goes on the same way
                // when a resumed session is told to; without one, the delivery is the user's again.
                if let Some(id) = host_end.filter(|_| {
                    (owes_post_merge || cut_by_end)
                        && delivered.decisions.is_empty()
                        && matches!(execution.activity, Activity::Resting)
                }) {
                    match resume_instead.take() {
                        Some(input) => queue_unless_waiting(&mut execution, input)?,
                        None if !owes_post_merge => {}
                        None => {
                            delivered.decisions.insert(
                                id.clone(),
                                Card {
                                    id,
                                    reason: WaitReason::Unheard,
                                    owner: Owner::User,
                                    callback: None,
                                },
                            );
                        }
                    }
                }
                delivered.session = Session::Present(execution);
            }
            // A task that is closing has nobody to ask how it goes on: the end of its host, or its
            // takeover, is kept for the drain, which still owes everything the task owed when it began to close.
            State::Finishing(finishing) if host_end.is_some() || takeover_fact => {
                finishing.session = Session::Present(execution);
            }
            State::Finishing(_)
            | State::Unstarted { .. }
            | State::InProgress(Progress::Queued { .. })
            | State::Completed
            | State::Cancelled => return Err(Refusal::InvalidState),
        }
        Ok(())
    }

    /// A fact that changes nothing is still taken: the cursor moves past it.
    fn unchanged(&mut self, cursor: u64) -> Result<(), Refusal> {
        self.session_mut().ok_or(Refusal::StaleExecution)?.cursor = cursor;
        Ok(())
    }
}

/// The node is told `input` unless it already waits to be told it.
fn queue_unless_waiting(execution: &mut Execution, input: InputId) -> Result<(), Refusal> {
    match queue_input(execution, input) {
        Err(Refusal::DuplicateObligation) => Ok(()),
        other => other,
    }
}

impl Execution {
    /// The provider started `turn` while the journal expected another start, or none: it runs all
    /// the same, and a fact the provider reports is never left out. Work and notifications that
    /// were pending go on in it, the input it took leaves the queue wherever it waited, and an
    /// input handed to the host that it did not take waits again at the front.
    fn provider_turn_started(&mut self, turn: &TurnId, trigger: &Trigger) -> Result<(BTreeSet<WorkId>, BTreeSet<Wake>), Refusal> {
        let (work, mut continuations) = match &self.activity {
            Activity::Turn { id, .. } if id == turn => return Err(Refusal::DuplicateObligation),
            Activity::Turn { work, continuations, .. } => (work.clone(), continuations.clone()),
            Activity::OwnWork { work, continuations } => (work.values(), continuations.clone()),
            Activity::Continuation(pending) => (BTreeSet::new(), pending.values()),
            Activity::InputPending(expected) => {
                self.inbox.push_front(expected.clone());
                (BTreeSet::new(), BTreeSet::new())
            }
            Activity::Launching(_) => return Err(Refusal::InvalidState),
            Activity::Awaiting(_) | Activity::Resting => (BTreeSet::new(), BTreeSet::new()),
        };
        match trigger {
            Trigger::Input(input) => {
                self.inbox.replace(input, None);
            }
            Trigger::OwnedContinuation(continuation) => {
                continuations.remove(&Wake::Owned(continuation.clone()));
            }
            Trigger::Unprompted => {}
        }
        Ok((work, continuations))
    }
}

impl Execution {
    fn open_unseen_turn(&mut self, turn: TurnId, trigger: &Trigger) -> Result<(), Refusal> {
        let (work, continuations) = self.provider_turn_started(&turn, trigger)?;
        let taken = continuations.iter().filter(|wake| matches!(wake, Wake::Owned(_))).cloned().collect();
        self.activity = Activity::Turn { id: turn, work, continuations, taken, stop: TurnStop::Running };
        Ok(())
    }
    fn runs_work(&self, work: &WorkId) -> bool {
        match &self.activity {
            Activity::Turn { work: running, .. } => running.contains(work),
            Activity::OwnWork { work: running, .. } => running.iter().any(|running| running == work),
            _ => false,
        }
    }
}
