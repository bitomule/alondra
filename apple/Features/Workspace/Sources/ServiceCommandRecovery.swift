import AlondraRecovery
import AlondraQuestionsFeature
import ComposableArchitecture
import Foundation

public struct ServiceCommandRecoveryClient: Sendable {
    public var observe: @Sendable () async -> AsyncStream<CommandResolution>
    public var observeFeedback: @Sendable () async -> AsyncStream<[ReportFeedbackRecoveryEvent]>
    public var observeDelivery: @Sendable () async -> AsyncStream<DeliveryStatus>
    public var acknowledgeDelivery: @Sendable ([UUID: DeliveryStatus.Phase], CommandScope) async throws -> Void
    public init(observe: @escaping @Sendable () async -> AsyncStream<CommandResolution>,
                observeFeedback: @escaping @Sendable () async -> AsyncStream<[ReportFeedbackRecoveryEvent]> = { AsyncStream { $0.finish() } },
                observeDelivery: @escaping @Sendable () async -> AsyncStream<DeliveryStatus> = { AsyncStream { $0.finish() } },
                acknowledgeDelivery: @escaping @Sendable ([UUID: DeliveryStatus.Phase], CommandScope) async throws -> Void = { _, _ in throw DeliveryAcknowledgementUnavailable() }) {
        self.observe = observe
        self.observeFeedback = observeFeedback
        self.observeDelivery = observeDelivery
        self.acknowledgeDelivery = acknowledgeDelivery
    }
}
public struct DeliveryAcknowledgementUnavailable: Error { public init() {} }
private enum ServiceCommandRecoveryKey: DependencyKey {
    static let liveValue = ServiceCommandRecoveryClient(observe: { AsyncStream { $0.finish() } })
}
public extension DependencyValues {
    var serviceCommandRecovery: ServiceCommandRecoveryClient {
        get { self[ServiceCommandRecoveryKey.self] }
        set { self[ServiceCommandRecoveryKey.self] = newValue }
    }
}

extension ServiceRootFeature {
    /// Delivery is replayable; each child accepts only its exact outstanding UUID.
    func resolutionEffects(_ resolution: CommandResolution, state: State) -> Effect<Action> {
        let id = resolution.id
        let rejection = resolution.succeeded ? nil : resolution.message
        let questionAction: QuestionFeature.Action = resolution.deliveryUnconfirmed == true
            ? .commandDeliveryUnconfirmed(id, attemptID: resolution.deliveryAttemptID) : .commandResolved(id, rejection: rejection)
        let documentAction: DocumentReviewFeature.Action = resolution.deliveryUnconfirmed == true
            ? .commandDeliveryUnconfirmed(id, attemptID: resolution.deliveryAttemptID) : .commandResolved(id, rejection: rejection)
        var effects: [Effect<Action>] = []
        if state.triage.triage.uncertainByItem.values.contains(id) {
            effects.append(.send(.triage(.triage(.commandResolved(id, rejection: rejection)))))
        }
        if state.triage.question?.uncertainCommandID == id {
            effects.append(.send(.triage(.question(.presented(questionAction)))))
        }
        for document in state.triage.documents where document.uncertainCommandID == id {
            effects.append(.send(.triage(.documents(.element(id: document.id, action: documentAction)))))
        }
        if state.newRequest.uncertainCommandID == id {
            effects.append(.send(.newRequest(.commandResolved(id, rejection: rejection))))
        }
        for pathID in state.fleet.path.ids {
            switch state.fleet.path[id: pathID]! {
            case .task(let task) where task.mutationOutcome?.uncertainID == id:
                effects.append(.send(.fleet(.path(.element(id: pathID, action: .task(.commandResolved(id, rejection: rejection)))))))
            default: break
            }
        }
        for pathID in state.tasks.taskPath.ids where state.tasks.taskPath[id: pathID]?.mutationOutcome?.uncertainID == id {
            effects.append(.send(.tasks(.taskPath(.element(id: pathID, action: .commandResolved(id, rejection: rejection))))))
        }
        if state.tasks.withdrawOutcome?.uncertainID == id {
            effects.append(.send(.tasks(.withdrawalResolved(id, rejection: rejection))))
        }
        if state.fleet.taskDetail?.mutationOutcome?.uncertainID == id {
            effects.append(.send(.fleet(.taskDetail(.presented(.commandResolved(id, rejection: rejection))))))
        }
        if state.tasks.board.archiveOutcome?.uncertainID == id {
            effects.append(.send(.tasks(.board(.commandResolved(id, rejection: rejection)))))
        }
        if state.tasks.detail?.mutationOutcome?.uncertainID == id {
            effects.append(.send(.tasks(.detail(.presented(.commandResolved(id, rejection: rejection))))))
        }
        if state.tasks.schedules.uncertainByTarget.values.contains(id) {
            effects.append(.send(.tasks(.schedules(.commandResolved(id, rejection: rejection)))))
        }
        if state.fleet.fleet.closeOutcomeByID.values.contains(where: { $0.uncertainID == id }) {
            effects.append(.send(.fleet(.fleet(.commandResolved(id, rejection: rejection)))))
        }
        for node in state.fleet.nodes where node.sendOutcome?.uncertainID == id || node.closeOutcome?.uncertainID == id || node.retryOutcome?.uncertainID == id || node.cancelOutcome?.uncertainID == id || node.acknowledgeOutcome?.uncertainID == id {
            effects.append(.send(.fleet(.nodes(.element(id: node.id, action: .commandResolved(id, rejection: rejection))))))
        }
        for card in state.triage.cards {
            if card.question?.uncertainCommandID == id {
                effects.append(.send(.triage(.cards(.element(id: card.id, action: .question(questionAction))))))
            }
            if card.document?.uncertainCommandID == id {
                effects.append(.send(.triage(.cards(.element(id: card.id, action: .document(documentAction))))))
            }
            if card.pullRequest?.diff.mutationOutcome?.uncertainID == id {
                effects.append(.send(.triage(.cards(.element(id: card.id, action: .pullRequest(.diff(.commandResolved(id, rejection: rejection))))))))
            }
            if card.pullRequest?.pr.mutationOutcome?.uncertainID == id {
                effects.append(.send(.triage(.cards(.element(id: card.id, action: .pullRequest(.pr(.commandResolved(id, rejection: rejection))))))))
            }
            for child in card.pullRequest?.questionFeatures ?? [] where child.question.uncertainCommandID == id {
                let action = ServicePullRequestFeature.Action.attachedQuestion(.element(
                    id: child.id, action: .question(questionAction)))
                effects.append(.send(.triage(.cards(.element(id: card.id, action: .pullRequest(action))))))
            }
        }
        for detail in state.pullRequests {
            for child in detail.questionFeatures where child.question.uncertainCommandID == id {
                effects.append(.send(.pullRequests(.element(id: detail.id, action: .attachedQuestion(.element(id: child.id, action: .question(questionAction)))))))
            }
            if detail.pr.mutationOutcome?.uncertainID == id {
                effects.append(.send(.pullRequests(.element(id: detail.id, action: .pr(.commandResolved(id, rejection: rejection))))))
            }
            if detail.diff.mutationOutcome?.uncertainID == id {
                effects.append(.send(.pullRequests(.element(id: detail.id, action: .diff(.commandResolved(id, rejection: rejection))))))
            }
        }
        return .merge(effects)
    }
}


extension ServiceRootFeature {
    func feedbackResolutionEffects(_ events: [ReportFeedbackRecoveryEvent], state: State) -> Effect<Action> {
        var effects: [Effect<Action>] = []
        for document in state.triage.documents {
            if let action = feedbackAction(events, document: document) {
                effects.append(.send(.triage(.documents(.element(id: document.id, action: action)))))
            }
        }
        for card in state.triage.cards {
            if let action = feedbackAction(events, document: card.document) {
                effects.append(.send(.triage(.cards(.element(id: card.id, action: .document(action))))))
            }
        }
        return .merge(effects)
    }

    private func feedbackAction(_ events: [ReportFeedbackRecoveryEvent], document: DocumentReviewFeature.State?) -> DocumentReviewFeature.Action? {
        guard let document, document.needsReconciliation, let id = document.uncertainCommandID,
              let event = events.first(where: { $0.operationID == id }) else { return nil }
        switch event.outcome {
        case .confirmed:
            return .commandResolved(id, rejection: nil)
        case .messageRejected(let status):
            return .commandResolved(id, rejection: "The feedback was rejected (HTTP \(status)). Your draft has been kept.")
        case .awaitingDraft:
            return .commandResolved(id, rejection: "The feedback was not sent. Your draft has been kept; you can submit it again.")
        case .readRejected(let status):
            return .feedbackReadRejected(id, "Your feedback was delivered, but marking the report read failed (HTTP \(status)).")
        case .unknown: return nil
        }
    }
}


extension ServiceRootFeature {
    func deliveryResolutionEffects(_ snapshot: DeliveryJournalSnapshot, state: State) -> Effect<Action> {
        func action(_ question: QuestionFeature.State) -> QuestionFeature.Action? {
            guard question.deliveryUnconfirmed, let id = question.deliveryAttemptID,
                  let attempt = snapshot.attempts.first(where: { $0.id == id && $0.sessionID == question.question.sessionId
                      && ($0.questionID == nil || $0.questionID == question.question.id) }),
                  ["retryable", "discarded", "handed"].contains(attempt.state) else { return nil }
            return .deliveryDecisionResolved(retry: attempt.state == "retryable", discarded: attempt.state == "discarded")
        }
        func documentAction(_ document: DocumentReviewFeature.State) -> DocumentReviewFeature.Action? {
            guard document.deliveryUnconfirmed, let id = document.deliveryAttemptID,
                  case .question(let question) = document.subject,
                  let attempt = snapshot.attempts.first(where: { $0.id == id && $0.sessionID == question.sessionId
                      && ($0.questionID == nil || $0.questionID == question.id) }),
                  ["retryable", "discarded", "handed"].contains(attempt.state) else { return nil }
            return .deliveryDecisionResolved(retry: attempt.state == "retryable", discarded: attempt.state == "discarded")
        }
        var effects: [Effect<Action>] = []
        for document in state.triage.documents {
            if let next = documentAction(document) {
                effects.append(.send(.triage(.documents(.element(id: document.id, action: next)))))
            }
        }
        if let question = state.triage.question, let next = action(question) {
            effects.append(.send(.triage(.question(.presented(next)))))
        }
        for card in state.triage.cards {
            if let document = card.document, let next = documentAction(document) {
                effects.append(.send(.triage(.cards(.element(id: card.id, action: .document(next))))))
            }
            if let question = card.question, let next = action(question) {
                effects.append(.send(.triage(.cards(.element(id: card.id, action: .question(next))))))
            }
            for child in card.pullRequest?.questionFeatures ?? [] {
                if let next = action(child.question) {
                    effects.append(.send(.triage(.cards(.element(id: card.id,
                        action: .pullRequest(.attachedQuestion(.element(id: child.id, action: .question(next)))))))))
                }
            }
        }
        for detail in state.pullRequests {
            for child in detail.questionFeatures {
                if let next = action(child.question) {
                    let childAction = ServicePullRequestFeature.Action.attachedQuestion(.element(id: child.id, action: .question(next)))
                    effects.append(.send(.pullRequests(.element(id: detail.id, action: childAction))))
                }
            }
        }
        return .merge(effects)
    }
}
