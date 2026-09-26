import AlondraCommands
import ComposableArchitecture

public struct ServiceCommandRecoveryClient: Sendable {
    public var observe: @Sendable () async -> AsyncStream<CommandResolution>
    public var observeFeedback: @Sendable () async -> AsyncStream<[ReportFeedbackRecoveryEvent]>
    public init(observe: @escaping @Sendable () async -> AsyncStream<CommandResolution>,
                observeFeedback: @escaping @Sendable () async -> AsyncStream<[ReportFeedbackRecoveryEvent]> = { AsyncStream { $0.finish() } }) {
        self.observe = observe
        self.observeFeedback = observeFeedback
    }
}
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
        var effects: [Effect<Action>] = []
        if state.triage.triage.uncertainByItem.values.contains(id) {
            effects.append(.send(.triage(.triage(.commandResolved(id, rejection: rejection)))))
        }
        if state.triage.question?.uncertainCommandID == id {
            effects.append(.send(.triage(.question(.presented(.commandResolved(id, rejection: rejection))))))
        }
        if state.triage.document?.uncertainCommandID == id {
            effects.append(.send(.triage(.document(.presented(.commandResolved(id, rejection: rejection))))))
        }
        if state.newRequest.uncertainCommandID == id {
            effects.append(.send(.newRequest(.commandResolved(id, rejection: rejection))))
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
        if state.fleet.detail?.sendOutcome?.uncertainID == id || state.fleet.detail?.closeOutcome?.uncertainID == id {
            effects.append(.send(.fleet(.detail(.presented(.commandResolved(id, rejection: rejection))))))
        }
        if state.pullRequest?.pr.mutationOutcome?.uncertainID == id {
            effects.append(.send(.pullRequest(.presented(.pr(.commandResolved(id, rejection: rejection))))))
        }
        if state.pullRequest?.diff.mutationOutcome?.uncertainID == id {
            effects.append(.send(.pullRequest(.presented(.diff(.commandResolved(id, rejection: rejection))))))
        }
        return .merge(effects)
    }
}


extension ServiceRootFeature {
    func feedbackResolutionEffects(_ events: [ReportFeedbackRecoveryEvent], state: State) -> Effect<Action> {
        guard let id = state.triage.document?.uncertainCommandID,
              let event = events.first(where: { $0.operationID == id }) else { return .none }
        switch event.outcome {
        case .confirmed:
            return .send(.triage(.document(.presented(.commandResolved(id, rejection: nil)))))
        case .messageRejected(let status):
            return .send(.triage(.document(.presented(.commandResolved(id, rejection: "The feedback was rejected (HTTP \(status)). Your draft has been kept.")))))
        case .awaitingDraft:
            return .send(.triage(.document(.presented(.commandResolved(id, rejection: "The feedback was not sent. Your draft has been kept; you can submit it again.")))))
        case .readRejected(let status):
            return .send(.triage(.document(.presented(.feedbackReadRejected(id, "Your feedback was delivered, but marking the report read failed (HTTP \(status)).")))))
        case .unknown: return .none
        }
    }
}
