use crate::{Error, Result};
use alondra_domain::*;
use alondra_foundation::{AgentId, InteractionId, TaskId};
use serde::{Deserialize, Serialize};

pub(super) fn encode(event: &Event) -> Result<String> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > crate::MAX_PAYLOAD - self.0.len() {
                return Err(std::io::Error::other("journal event capacity"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded(vec![]);
    serde_json::to_writer(&mut writer, &EventRecord::from(event))
        .map_err(|_| Error::InvalidInput)?;
    String::from_utf8(writer.0).map_err(|_| Error::InvalidInput)
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum WaitRecord {
    Question,
    Permission,
    Pause,
    Recovery,
    Unheard,
}
impl From<&WaitReason> for WaitRecord {
    fn from(event: &WaitReason) -> Self {
        match event {
            WaitReason::Question => Self::Question,
            WaitReason::Permission => Self::Permission,
            WaitReason::Pause => Self::Pause,
            WaitReason::Recovery => Self::Recovery,
            WaitReason::Unheard => Self::Unheard,
        }
    }
}
impl From<WaitRecord> for WaitReason {
    fn from(event: WaitRecord) -> Self {
        match event {
            WaitRecord::Question => Self::Question,
            WaitRecord::Permission => Self::Permission,
            WaitRecord::Pause => Self::Pause,
            WaitRecord::Recovery => Self::Recovery,
            WaitRecord::Unheard => Self::Unheard,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum PlanReviewOutcomeRecord {
    Unchanged,
    PlanChanged,
    Cancelled,
    KeptByUser,
}
impl From<PlanReviewOutcome> for PlanReviewOutcomeRecord {
    fn from(outcome: PlanReviewOutcome) -> Self {
        match outcome {
            PlanReviewOutcome::Unchanged => Self::Unchanged,
            PlanReviewOutcome::PlanChanged => Self::PlanChanged,
            PlanReviewOutcome::Cancelled => Self::Cancelled,
            PlanReviewOutcome::KeptByUser => Self::KeptByUser,
        }
    }
}
impl From<PlanReviewOutcomeRecord> for PlanReviewOutcome {
    fn from(record: PlanReviewOutcomeRecord) -> Self {
        match record {
            PlanReviewOutcomeRecord::Unchanged => Self::Unchanged,
            PlanReviewOutcomeRecord::PlanChanged => Self::PlanChanged,
            PlanReviewOutcomeRecord::Cancelled => Self::Cancelled,
            PlanReviewOutcomeRecord::KeptByUser => Self::KeptByUser,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum FinishRecord {
    Complete,
    Cancel,
}
impl From<&FinishIntent> for FinishRecord {
    fn from(event: &FinishIntent) -> Self {
        match event {
            FinishIntent::Complete => Self::Complete,
            FinishIntent::Cancel => Self::Cancel,
        }
    }
}
impl From<FinishRecord> for FinishIntent {
    fn from(event: FinishRecord) -> Self {
        match event {
            FinishRecord::Complete => Self::Complete,
            FinishRecord::Cancel => Self::Cancel,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum DrainRecord {
    Execution(String),
    Work(String),
    Callback(String),
    Merge(String),
    Cleanup(String),
    ExternalEffect(String),
    PostMerge(String),
}
impl From<&DrainObligation> for DrainRecord {
    fn from(event: &DrainObligation) -> Self {
        match event {
            DrainObligation::Execution(value) => Self::Execution(value.as_str().to_owned()),
            DrainObligation::Work(value) => Self::Work(value.as_str().to_owned()),
            DrainObligation::Callback(value) => Self::Callback(value.as_str().to_owned()),
            DrainObligation::Merge(value) => Self::Merge(value.as_str().to_owned()),
            DrainObligation::Cleanup(value) => Self::Cleanup(value.as_str().to_owned()),
            DrainObligation::ExternalEffect(value) => {
                Self::ExternalEffect(value.as_str().to_owned())
            }
            DrainObligation::PostMerge(value) => Self::PostMerge(value.as_str().to_owned()),
        }
    }
}
impl DrainRecord {
    fn decode(self) -> Result<DrainObligation> {
        Ok(match self {
            DrainRecord::Execution(value) => {
                DrainObligation::Execution(InstanceId::new(value).map_err(Error::from)?)
            }
            DrainRecord::Work(value) => {
                DrainObligation::Work(WorkId::new(value).map_err(Error::from)?)
            }
            DrainRecord::Callback(value) => {
                DrainObligation::Callback(CallbackId::new(value).map_err(Error::from)?)
            }
            DrainRecord::Merge(value) => {
                DrainObligation::Merge(OperationId::new(value).map_err(Error::from)?)
            }
            DrainRecord::Cleanup(value) => {
                DrainObligation::Cleanup(OperationId::new(value).map_err(Error::from)?)
            }
            DrainRecord::ExternalEffect(value) => {
                DrainObligation::ExternalEffect(OperationId::new(value).map_err(Error::from)?)
            }
            DrainRecord::PostMerge(value) => {
                DrainObligation::PostMerge(OperationId::new(value).map_err(Error::from)?)
            }
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum TriggerRecord {
    Input(String),
    OwnedContinuation(String),
}
impl From<&Trigger> for TriggerRecord {
    fn from(event: &Trigger) -> Self {
        match event {
            Trigger::Input(value) => Self::Input(value.as_str().to_owned()),
            Trigger::OwnedContinuation(value) => Self::OwnedContinuation(value.as_str().to_owned()),
        }
    }
}
impl TriggerRecord {
    fn decode(self) -> Result<Trigger> {
        Ok(match self {
            TriggerRecord::Input(value) => {
                Trigger::Input(InputId::new(value).map_err(Error::from)?)
            }
            TriggerRecord::OwnedContinuation(value) => {
                Trigger::OwnedContinuation(ContinuationId::new(value).map_err(Error::from)?)
            }
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum FactRecord {
    Ask {
        card: String,
        callback: String,
        reason: WaitRecord,
    },
    ResourceAvailable,
    TurnStarted {
        turn: String,
        trigger: TriggerRecord,
    },
    InputConsumed {
        input: String,
    },
    WorkStarted {
        turn: String,
        work: String,
    },
    WorkEnded {
        work: String,
        continuation: String,
    },
    ContinuationConsumed {
        continuation: String,
    },
    TurnEnded {
        turn: String,
        unheard_card: String,
    },
    ConnectionLost {
        problem_card: String,
    },
    ResourceReleased {
        problem_card: String,
    },
    CallbackSettled {
        callback: String,
        continuation: String,
    },
    CallbackAborted {
        callback: String,
        unheard_card: String,
    },
    EffectAccepted {
        operation: String,
    },
    EffectObserved {
        operation: String,
    },
    Notification,
}
impl From<&RuntimeFact> for FactRecord {
    fn from(event: &RuntimeFact) -> Self {
        match event {
            RuntimeFact::Ask {
                card,
                callback,
                reason,
            } => Self::Ask {
                card: card.0.clone(),
                callback: callback.as_str().to_owned(),
                reason: WaitRecord::from(reason),
            },
            RuntimeFact::ResourceAvailable => Self::ResourceAvailable,
            RuntimeFact::TurnStarted { turn, trigger } => Self::TurnStarted {
                turn: turn.as_str().to_owned(),
                trigger: TriggerRecord::from(trigger),
            },
            RuntimeFact::InputConsumed { input } => Self::InputConsumed {
                input: input.as_str().to_owned(),
            },
            RuntimeFact::WorkStarted { turn, work } => Self::WorkStarted {
                turn: turn.as_str().to_owned(),
                work: work.as_str().to_owned(),
            },
            RuntimeFact::WorkEnded { work, continuation } => Self::WorkEnded {
                work: work.as_str().to_owned(),
                continuation: continuation.as_str().to_owned(),
            },
            RuntimeFact::ContinuationConsumed { continuation } => Self::ContinuationConsumed {
                continuation: continuation.as_str().to_owned(),
            },
            RuntimeFact::TurnEnded { turn, unheard_card } => Self::TurnEnded {
                turn: turn.as_str().to_owned(),
                unheard_card: unheard_card.0.clone(),
            },
            RuntimeFact::ConnectionLost { problem_card } => Self::ConnectionLost {
                problem_card: problem_card.0.clone(),
            },
            RuntimeFact::ResourceReleased { problem_card } => Self::ResourceReleased {
                problem_card: problem_card.0.clone(),
            },
            RuntimeFact::CallbackSettled {
                callback,
                continuation,
            } => Self::CallbackSettled {
                callback: callback.as_str().to_owned(),
                continuation: continuation.as_str().to_owned(),
            },
            RuntimeFact::CallbackAborted {
                callback,
                unheard_card,
            } => Self::CallbackAborted {
                callback: callback.as_str().to_owned(),
                unheard_card: unheard_card.0.clone(),
            },
            RuntimeFact::EffectAccepted { operation } => Self::EffectAccepted {
                operation: operation.as_str().to_owned(),
            },
            RuntimeFact::EffectObserved { operation } => Self::EffectObserved {
                operation: operation.as_str().to_owned(),
            },
            RuntimeFact::Notification => Self::Notification,
        }
    }
}
impl FactRecord {
    fn decode(self) -> Result<RuntimeFact> {
        Ok(match self {
            FactRecord::Ask {
                card,
                callback,
                reason,
            } => RuntimeFact::Ask {
                card: InteractionId(card),
                callback: CallbackId::new(callback).map_err(Error::from)?,
                reason: reason.into(),
            },
            FactRecord::ResourceAvailable => RuntimeFact::ResourceAvailable,
            FactRecord::TurnStarted { turn, trigger } => RuntimeFact::TurnStarted {
                turn: TurnId::new(turn).map_err(Error::from)?,
                trigger: trigger.decode()?,
            },
            FactRecord::InputConsumed { input } => RuntimeFact::InputConsumed {
                input: InputId::new(input).map_err(Error::from)?,
            },
            FactRecord::WorkStarted { turn, work } => RuntimeFact::WorkStarted {
                turn: TurnId::new(turn).map_err(Error::from)?,
                work: WorkId::new(work).map_err(Error::from)?,
            },
            FactRecord::WorkEnded { work, continuation } => RuntimeFact::WorkEnded {
                work: WorkId::new(work).map_err(Error::from)?,
                continuation: ContinuationId::new(continuation).map_err(Error::from)?,
            },
            FactRecord::ContinuationConsumed { continuation } => {
                RuntimeFact::ContinuationConsumed {
                    continuation: ContinuationId::new(continuation).map_err(Error::from)?,
                }
            }
            FactRecord::TurnEnded { turn, unheard_card } => RuntimeFact::TurnEnded {
                turn: TurnId::new(turn).map_err(Error::from)?,
                unheard_card: InteractionId(unheard_card),
            },
            FactRecord::ConnectionLost { problem_card } => RuntimeFact::ConnectionLost {
                problem_card: InteractionId(problem_card),
            },
            FactRecord::ResourceReleased { problem_card } => RuntimeFact::ResourceReleased {
                problem_card: InteractionId(problem_card),
            },
            FactRecord::CallbackSettled {
                callback,
                continuation,
            } => RuntimeFact::CallbackSettled {
                callback: CallbackId::new(callback).map_err(Error::from)?,
                continuation: ContinuationId::new(continuation).map_err(Error::from)?,
            },
            FactRecord::CallbackAborted {
                callback,
                unheard_card,
            } => RuntimeFact::CallbackAborted {
                callback: CallbackId::new(callback).map_err(Error::from)?,
                unheard_card: InteractionId(unheard_card),
            },
            FactRecord::EffectAccepted { operation } => RuntimeFact::EffectAccepted {
                operation: OperationId::new(operation).map_err(Error::from)?,
            },
            FactRecord::EffectObserved { operation } => RuntimeFact::EffectObserved {
                operation: OperationId::new(operation).map_err(Error::from)?,
            },
            FactRecord::Notification => RuntimeFact::Notification,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(super) enum EventRecord {
    Admit {
        node: String,
        conversation: String,
        instance: String,
        host: String,
    },
    Runtime {
        stamp: StampRecord,
        cursor: u64,
        fact: FactRecord,
    },
    Ask {
        card: String,
    },
    Answer {
        card: String,
        input: String,
    },
    PlanApproved,
    BlockDelivery {
        delivery: String,
        dependencies: Vec<String>,
    },
    DeliveryUnblocked {
        delivery: String,
    },
    Present {
        delivery: String,
    },
    Accept {
        delivery: String,
        merge: Option<String>,
    },
    MergeObserved {
        operation: String,
    },
    HandBack {
        delivery: String,
        input: String,
    },
    WaitForDependencies {
        dependencies: Vec<String>,
    },
    DependenciesResolved {
        input: String,
    },
    Pause {
        card: String,
    },
    Resume {
        card: String,
        input: String,
    },
    Finish {
        intent: FinishRecord,
        cleanup: String,
        post_merge: Option<String>,
    },
    DrainObserved {
        obligation: DrainRecord,
    },
    RetireReleasedExecution {
        problem_card: String,
        input: String,
    },
    ExecutionReleased {
        input: String,
    },
    Reopen {
        input: String,
    },
    HoldForPlanReview {
        review: String,
    },
    PlanReviewSettled {
        review: String,
        plan_revision: u64,
        outcome: PlanReviewOutcomeRecord,
    },
}
impl From<&Event> for EventRecord {
    fn from(event: &Event) -> Self {
        match event {
            Event::Admit {
                node,
                conversation,
                instance,
                host,
            } => Self::Admit {
                node: node.0.clone(),
                conversation: conversation.as_str().to_owned(),
                instance: instance.as_str().to_owned(),
                host: host.as_str().to_owned(),
            },
            Event::Runtime {
                stamp,
                cursor,
                fact,
            } => Self::Runtime {
                stamp: StampRecord::from(stamp),
                cursor: *cursor,
                fact: FactRecord::from(fact),
            },
            Event::Ask { card } => Self::Ask {
                card: card.0.clone(),
            },
            Event::Answer { card, input } => Self::Answer {
                card: card.0.clone(),
                input: input.as_str().to_owned(),
            },
            Event::PlanApproved => Self::PlanApproved,
            Event::BlockDelivery {
                delivery,
                dependencies,
            } => Self::BlockDelivery {
                delivery: delivery.0.clone(),
                dependencies: dependencies.iter().map(|value| value.0.clone()).collect(),
            },
            Event::DeliveryUnblocked { delivery } => Self::DeliveryUnblocked {
                delivery: delivery.0.clone(),
            },
            Event::Present { delivery } => Self::Present {
                delivery: delivery.0.clone(),
            },
            Event::Accept { delivery, merge } => Self::Accept {
                delivery: delivery.0.clone(),
                merge: merge.as_ref().map(|value| value.as_str().to_owned()),
            },
            Event::MergeObserved { operation } => Self::MergeObserved {
                operation: operation.as_str().to_owned(),
            },
            Event::HandBack { delivery, input } => Self::HandBack {
                delivery: delivery.0.clone(),
                input: input.as_str().to_owned(),
            },
            Event::WaitForDependencies { dependencies } => Self::WaitForDependencies {
                dependencies: dependencies.iter().map(|value| value.0.clone()).collect(),
            },
            Event::DependenciesResolved { input } => Self::DependenciesResolved {
                input: input.as_str().to_owned(),
            },
            Event::Pause { card } => Self::Pause {
                card: card.0.clone(),
            },
            Event::Resume { card, input } => Self::Resume {
                card: card.0.clone(),
                input: input.as_str().to_owned(),
            },
            Event::Finish {
                intent,
                cleanup,
                post_merge,
            } => Self::Finish {
                intent: FinishRecord::from(intent),
                cleanup: cleanup.as_str().to_owned(),
                post_merge: post_merge.as_ref().map(|value| value.as_str().to_owned()),
            },
            Event::DrainObserved { obligation } => Self::DrainObserved {
                obligation: DrainRecord::from(obligation),
            },
            Event::RetireReleasedExecution {
                problem_card,
                input,
            } => Self::RetireReleasedExecution {
                problem_card: problem_card.0.clone(),
                input: input.as_str().to_owned(),
            },
            Event::ExecutionReleased { input } => Self::ExecutionReleased {
                input: input.as_str().to_owned(),
            },
            Event::Reopen { input } => Self::Reopen {
                input: input.as_str().to_owned(),
            },
            Event::HoldForPlanReview { review } => Self::HoldForPlanReview {
                review: review.0.clone(),
            },
            Event::PlanReviewSettled {
                review,
                plan_revision,
                outcome,
            } => Self::PlanReviewSettled {
                review: review.0.clone(),
                plan_revision: *plan_revision,
                outcome: (*outcome).into(),
            },
        }
    }
}
impl EventRecord {
    pub(super) fn decode(self, current: &TaskLifecycle) -> Result<Event> {
        Ok(match self {
            EventRecord::Admit {
                node,
                conversation,
                instance,
                host,
            } => Event::Admit {
                node: AgentId(node),
                conversation: ConversationId::new(conversation).map_err(Error::from)?,
                instance: InstanceId::new(instance).map_err(Error::from)?,
                host: HostId::new(host).map_err(Error::from)?,
            },
            EventRecord::Runtime {
                stamp,
                cursor,
                fact,
            } => Event::Runtime {
                stamp: stamp.resolve(current)?,
                cursor: cursor,
                fact: fact.decode()?,
            },
            EventRecord::Ask { card } => Event::Ask {
                card: InteractionId(card),
            },
            EventRecord::Answer { card, input } => Event::Answer {
                card: InteractionId(card),
                input: InputId::new(input).map_err(Error::from)?,
            },
            EventRecord::PlanApproved => Event::PlanApproved,
            EventRecord::BlockDelivery {
                delivery,
                dependencies,
            } => Event::BlockDelivery {
                delivery: InteractionId(delivery),
                dependencies: dependencies.into_iter().map(TaskId).collect(),
            },
            EventRecord::DeliveryUnblocked { delivery } => Event::DeliveryUnblocked {
                delivery: InteractionId(delivery),
            },
            EventRecord::Present { delivery } => Event::Present {
                delivery: InteractionId(delivery),
            },
            EventRecord::Accept { delivery, merge } => Event::Accept {
                delivery: InteractionId(delivery),
                merge: merge
                    .map(|value| OperationId::new(value).map_err(Error::from))
                    .transpose()?,
            },
            EventRecord::MergeObserved { operation } => Event::MergeObserved {
                operation: OperationId::new(operation).map_err(Error::from)?,
            },
            EventRecord::HandBack { delivery, input } => Event::HandBack {
                delivery: InteractionId(delivery),
                input: InputId::new(input).map_err(Error::from)?,
            },
            EventRecord::WaitForDependencies { dependencies } => Event::WaitForDependencies {
                dependencies: dependencies.into_iter().map(TaskId).collect(),
            },
            EventRecord::DependenciesResolved { input } => Event::DependenciesResolved {
                input: InputId::new(input).map_err(Error::from)?,
            },
            EventRecord::Pause { card } => Event::Pause {
                card: InteractionId(card),
            },
            EventRecord::Resume { card, input } => Event::Resume {
                card: InteractionId(card),
                input: InputId::new(input).map_err(Error::from)?,
            },
            EventRecord::Finish {
                intent,
                cleanup,
                post_merge,
            } => Event::Finish {
                intent: intent.into(),
                cleanup: OperationId::new(cleanup).map_err(Error::from)?,
                post_merge: post_merge
                    .map(|value| OperationId::new(value).map_err(Error::from))
                    .transpose()?,
            },
            EventRecord::DrainObserved { obligation } => Event::DrainObserved {
                obligation: obligation.decode()?,
            },
            EventRecord::RetireReleasedExecution {
                problem_card,
                input,
            } => Event::RetireReleasedExecution {
                problem_card: InteractionId(problem_card),
                input: InputId::new(input).map_err(Error::from)?,
            },
            EventRecord::ExecutionReleased { input } => Event::ExecutionReleased {
                input: InputId::new(input).map_err(Error::from)?,
            },
            EventRecord::Reopen { input } => Event::Reopen {
                input: InputId::new(input).map_err(Error::from)?,
            },
            EventRecord::HoldForPlanReview { review } => Event::HoldForPlanReview {
                review: InteractionId(review),
            },
            EventRecord::PlanReviewSettled {
                review,
                plan_revision,
                outcome,
            } => Event::PlanReviewSettled {
                review: InteractionId(review),
                plan_revision,
                outcome: outcome.into(),
            },
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StampRecord {
    task: String,
    node: String,
    conversation: String,
    instance: String,
    host: String,
    generation: u64,
}
impl From<&ExecutionStamp> for StampRecord {
    fn from(stamp: &ExecutionStamp) -> Self {
        Self {
            task: stamp.task().0.clone(),
            node: stamp.node().0.clone(),
            conversation: stamp.conversation().as_str().to_owned(),
            instance: stamp.instance().as_str().to_owned(),
            host: stamp.host().as_str().to_owned(),
            generation: stamp.generation(),
        }
    }
}
impl StampRecord {
    fn resolve(self, current: &TaskLifecycle) -> Result<ExecutionStamp> {
        let stamp = current.execution().ok_or(Error::CorruptJournal)?;
        if self.task != stamp.task().0
            || self.node != stamp.node().0
            || self.conversation != stamp.conversation().as_str()
            || self.instance != stamp.instance().as_str()
            || self.host != stamp.host().as_str()
            || self.generation != stamp.generation()
        {
            return Err(Error::CorruptJournal);
        }
        Ok(stamp.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn aggregate() -> TaskLifecycle {
        let mut aggregate =
            TaskLifecycle::planned(TaskId::from("task"), InputId::new("initial").unwrap()).unwrap();
        aggregate
            .apply(
                0,
                Event::Admit {
                    node: "node".into(),
                    conversation: ConversationId::new("conversation").unwrap(),
                    instance: InstanceId::new("instance").unwrap(),
                    host: HostId::new("host").unwrap(),
                },
            )
            .unwrap();
        aggregate
    }
    fn roundtrip(current: &TaskLifecycle, event: Event) {
        let encoded = serde_json::to_vec(&EventRecord::from(&event)).unwrap();
        let decoded: EventRecord = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.decode(current).unwrap(), event);
    }
    #[test]
    fn every_event_fact_trigger_and_drain_variant_roundtrips() {
        let current = aggregate();
        roundtrip(
            &current,
            Event::Admit {
                node: "node".into(),
                conversation: ConversationId::new("conversation").unwrap(),
                instance: InstanceId::new("instance").unwrap(),
                host: HostId::new("host").unwrap(),
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 3,
                fact: RuntimeFact::Notification,
            },
        );
        roundtrip(
            &current,
            Event::Ask {
                card: "card".into(),
            },
        );
        roundtrip(
            &current,
            Event::Answer {
                card: "card".into(),
                input: InputId::new("input").unwrap(),
            },
        );
        roundtrip(&current, Event::PlanApproved);
        roundtrip(
            &current,
            Event::BlockDelivery {
                delivery: "delivery".into(),
                dependencies: vec!["dependencies-first".into(), "dependencies-second".into()],
            },
        );
        roundtrip(
            &current,
            Event::DeliveryUnblocked {
                delivery: "delivery".into(),
            },
        );
        roundtrip(
            &current,
            Event::Present {
                delivery: "delivery".into(),
            },
        );
        roundtrip(
            &current,
            Event::Accept {
                delivery: "delivery".into(),
                merge: Some(OperationId::new("merge").unwrap()),
            },
        );
        roundtrip(
            &current,
            Event::MergeObserved {
                operation: OperationId::new("operation").unwrap(),
            },
        );
        roundtrip(
            &current,
            Event::HandBack {
                delivery: "delivery".into(),
                input: InputId::new("input").unwrap(),
            },
        );
        roundtrip(
            &current,
            Event::WaitForDependencies {
                dependencies: vec!["dependencies-first".into(), "dependencies-second".into()],
            },
        );
        roundtrip(
            &current,
            Event::DependenciesResolved {
                input: InputId::new("input").unwrap(),
            },
        );
        roundtrip(
            &current,
            Event::Pause {
                card: "card".into(),
            },
        );
        roundtrip(
            &current,
            Event::Resume {
                card: "card".into(),
                input: InputId::new("input").unwrap(),
            },
        );
        roundtrip(
            &current,
            Event::Finish {
                intent: FinishIntent::Complete,
                cleanup: OperationId::new("cleanup").unwrap(),
                post_merge: Some(OperationId::new("post_merge").unwrap()),
            },
        );
        roundtrip(
            &current,
            Event::DrainObserved {
                obligation: DrainObligation::Cleanup(OperationId::new("cleanup").unwrap()),
            },
        );
        roundtrip(
            &current,
            Event::RetireReleasedExecution {
                problem_card: "problem_card".into(),
                input: InputId::new("input").unwrap(),
            },
        );
        roundtrip(
            &current,
            Event::ExecutionReleased {
                input: InputId::new("input").unwrap(),
            },
        );
        roundtrip(
            &current,
            Event::Reopen {
                input: InputId::new("input").unwrap(),
            },
        );
        roundtrip(
            &current,
            Event::HoldForPlanReview {
                review: "decision".into(),
            },
        );
        for outcome in [
            PlanReviewOutcome::Unchanged,
            PlanReviewOutcome::PlanChanged,
            PlanReviewOutcome::Cancelled,
            PlanReviewOutcome::KeptByUser,
        ] {
            roundtrip(
                &current,
                Event::PlanReviewSettled {
                    review: "decision".into(),
                    plan_revision: 3,
                    outcome,
                },
            );
        }
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::Ask {
                    card: "card".into(),
                    callback: CallbackId::new("callback").unwrap(),
                    reason: WaitReason::Permission,
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::ResourceAvailable,
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::TurnStarted {
                    turn: TurnId::new("turn").unwrap(),
                    trigger: Trigger::Input(InputId::new("trigger-input").unwrap()),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::InputConsumed {
                    input: InputId::new("input").unwrap(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::WorkStarted {
                    turn: TurnId::new("turn").unwrap(),
                    work: WorkId::new("work").unwrap(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::WorkEnded {
                    work: WorkId::new("work").unwrap(),
                    continuation: ContinuationId::new("continuation").unwrap(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::ContinuationConsumed {
                    continuation: ContinuationId::new("continuation").unwrap(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::TurnEnded {
                    turn: TurnId::new("turn").unwrap(),
                    unheard_card: "unheard_card".into(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::ConnectionLost {
                    problem_card: "problem_card".into(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::ResourceReleased {
                    problem_card: "problem_card".into(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::CallbackSettled {
                    callback: CallbackId::new("callback").unwrap(),
                    continuation: ContinuationId::new("continuation").unwrap(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::CallbackAborted {
                    callback: CallbackId::new("callback").unwrap(),
                    unheard_card: "unheard_card".into(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::EffectAccepted {
                    operation: OperationId::new("operation").unwrap(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::EffectObserved {
                    operation: OperationId::new("operation").unwrap(),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::Notification,
            },
        );
        roundtrip(
            &current,
            Event::DrainObserved {
                obligation: DrainObligation::Execution(InstanceId::new("tuple-value").unwrap()),
            },
        );
        roundtrip(
            &current,
            Event::DrainObserved {
                obligation: DrainObligation::Work(WorkId::new("tuple-value").unwrap()),
            },
        );
        roundtrip(
            &current,
            Event::DrainObserved {
                obligation: DrainObligation::Callback(CallbackId::new("tuple-value").unwrap()),
            },
        );
        roundtrip(
            &current,
            Event::DrainObserved {
                obligation: DrainObligation::Merge(OperationId::new("tuple-value").unwrap()),
            },
        );
        roundtrip(
            &current,
            Event::DrainObserved {
                obligation: DrainObligation::Cleanup(OperationId::new("tuple-value").unwrap()),
            },
        );
        roundtrip(
            &current,
            Event::DrainObserved {
                obligation: DrainObligation::ExternalEffect(
                    OperationId::new("tuple-value").unwrap(),
                ),
            },
        );
        roundtrip(
            &current,
            Event::DrainObserved {
                obligation: DrainObligation::PostMerge(OperationId::new("tuple-value").unwrap()),
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::TurnStarted {
                    turn: TurnId::new("turn").unwrap(),
                    trigger: Trigger::Input(InputId::new("tuple-value").unwrap()),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::TurnStarted {
                    turn: TurnId::new("turn").unwrap(),
                    trigger: Trigger::OwnedContinuation(
                        ContinuationId::new("tuple-value").unwrap(),
                    ),
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::Ask {
                    card: "card".into(),
                    callback: CallbackId::new("callback").unwrap(),
                    reason: WaitReason::Question,
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::Ask {
                    card: "card".into(),
                    callback: CallbackId::new("callback").unwrap(),
                    reason: WaitReason::Permission,
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::Ask {
                    card: "card".into(),
                    callback: CallbackId::new("callback").unwrap(),
                    reason: WaitReason::Pause,
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::Ask {
                    card: "card".into(),
                    callback: CallbackId::new("callback").unwrap(),
                    reason: WaitReason::Recovery,
                },
            },
        );
        roundtrip(
            &current,
            Event::Runtime {
                stamp: current.execution().unwrap().clone(),
                cursor: 1,
                fact: RuntimeFact::Ask {
                    card: "card".into(),
                    callback: CallbackId::new("callback").unwrap(),
                    reason: WaitReason::Unheard,
                },
            },
        );
        roundtrip(
            &current,
            Event::Finish {
                intent: FinishIntent::Complete,
                cleanup: OperationId::new("cleanup").unwrap(),
                post_merge: None,
            },
        );
        roundtrip(
            &current,
            Event::Finish {
                intent: FinishIntent::Cancel,
                cleanup: OperationId::new("cleanup").unwrap(),
                post_merge: None,
            },
        );
    }
    #[test]
    fn corrupted_runtime_correlations_and_unknown_record_fields_fail_closed() {
        let current = aggregate();
        let event = Event::Runtime {
            stamp: current.execution().unwrap().clone(),
            cursor: 1,
            fact: RuntimeFact::Notification,
        };
        for field in [
            "task",
            "node",
            "conversation",
            "instance",
            "host",
            "generation",
        ] {
            let mut value = serde_json::to_value(EventRecord::from(&event)).unwrap();
            value["data"]["stamp"][field] = if field == "generation" {
                serde_json::json!(2)
            } else {
                serde_json::json!("other")
            };
            let record: EventRecord = serde_json::from_value(value).unwrap();
            assert!(matches!(
                record.decode(&current),
                Err(Error::CorruptJournal)
            ));
        }
        let mut value = serde_json::to_value(EventRecord::from(&event)).unwrap();
        value["data"]["stamp"]["unrecognized"] = serde_json::json!(true);
        assert!(serde_json::from_value::<EventRecord>(value).is_err());
    }
    #[test]
    fn journal_event_capacity_rejects_large_valid_events_and_accepts_small_control() {
        for count in [1, 6_000] {
            let event = Event::BlockDelivery {
                delivery: "delivery".into(),
                dependencies: (0..count)
                    .map(|index| TaskId::from(format!("dependency-{index:08}-{}", "x".repeat(180))))
                    .collect(),
            };
            let mut current = aggregate();
            current
                .apply(
                    1,
                    Event::Present {
                        delivery: "delivery".into(),
                    },
                )
                .unwrap();
            current.apply(2, event.clone()).unwrap();
            let expected = serde_json::to_vec(&EventRecord::from(&event)).unwrap();
            assert_eq!(expected.len() > crate::MAX_PAYLOAD, count > 1);
            if count == 1 {
                assert_eq!(encode(&event).unwrap().as_bytes(), expected);
            } else {
                assert!(matches!(encode(&event), Err(Error::InvalidInput)));
            }
        }
    }
    #[test]
    fn identity_decode_keeps_all_nine_errors_in_the_domain_category() {
        let current = aggregate();
        let runtime = |fact| EventRecord::Runtime {
            stamp: StampRecord::from(current.execution().unwrap()),
            cursor: 1,
            fact,
        };
        let admit = |conversation: &str, instance: &str, host: &str| EventRecord::Admit {
            node: "node".into(),
            conversation: conversation.into(),
            instance: instance.into(),
            host: host.into(),
        };
        for value in [" \t\n", "remote:value"] {
            let records = [
                ("conversation", admit(value, "instance", "host")),
                ("instance", admit("conversation", value, "host")),
                ("host", admit("conversation", "instance", value)),
                (
                    "input",
                    EventRecord::Answer {
                        card: "question".into(),
                        input: value.into(),
                    },
                ),
                (
                    "turn",
                    runtime(FactRecord::TurnStarted {
                        turn: value.into(),
                        trigger: TriggerRecord::Input("initial".into()),
                    }),
                ),
                (
                    "work",
                    runtime(FactRecord::WorkStarted {
                        turn: "turn".into(),
                        work: value.into(),
                    }),
                ),
                (
                    "continuation",
                    runtime(FactRecord::ContinuationConsumed {
                        continuation: value.into(),
                    }),
                ),
                (
                    "callback",
                    runtime(FactRecord::Ask {
                        card: "question".into(),
                        callback: value.into(),
                        reason: WaitRecord::Permission,
                    }),
                ),
                (
                    "operation",
                    runtime(FactRecord::EffectAccepted {
                        operation: value.into(),
                    }),
                ),
            ];
            for (name, record) in records {
                let decoded = record.decode(&current);
                if value.trim().is_empty() {
                    assert!(
                        matches!(decoded, Err(Error::Domain(Refusal::EmptyIdentity))),
                        "{name}: {decoded:?}"
                    );
                } else {
                    assert!(decoded.is_ok(), "{name}: {decoded:?}");
                }
            }
        }
        assert!(matches!(
            Error::from(alondra_foundation::IdentityError::InvalidGeneration),
            Error::Domain(Refusal::InvalidState)
        ));
    }
    #[test]
    fn legacy_process_tags_are_rejected_instead_of_reinterpreted_as_resource_facts() {
        for encoded in [r#"{"kind":"initialized"}"#,
            r#"{"kind":"process_exited","data":{"problem_card":"recovery"}}"#] {
            assert!(serde_json::from_str::<FactRecord>(encoded).is_err(),"{encoded}");
        }
        assert!(serde_json::from_str::<FactRecord>(r#"{"kind":"resource_available"}"#).is_ok());
        assert!(serde_json::from_str::<FactRecord>(r#"{"kind":"resource_released","data":{"problem_card":"recovery"}}"#).is_ok());
        assert!(serde_json::from_str::<DrainRecord>(r#"{"kind":"process","data":"instance"}"#).is_err());
        assert!(serde_json::from_str::<DrainRecord>(r#"{"kind":"execution","data":"instance"}"#).is_ok());
        assert!(serde_json::from_str::<EventRecord>(r#"{"kind":"retire_exited_execution","data":{"problem_card":"recovery","input":"initial"}}"#).is_err());
    }

}
