import AlondraCommands
import ComposableArchitecture
import Foundation

/// Presentation of uncertain operations. Dismissing a warning never releases a command reservation.
@Reducer
public struct DeliveryNoticeFeature {
    public init() {}
    @ObservableState
    public struct State: Equatable {
        public var status: DeliveryStatus?
        public var now = Date.distantPast
        public var isAcknowledging = false
        public var acknowledgementError: String?
        public init() {}
        public var pendingCount: Int {
            visible.filter { $0.phase == .unconfirmed && now.timeIntervalSince($0.firstObservedUncertainAt) < 120 }.count
        }
        public var warnings: [DeliveryStatus.Operation] {
            visible.filter { $0.phase != .unconfirmed || now.timeIntervalSince($0.firstObservedUncertainAt) >= 120 }
        }
        private var visible: [DeliveryStatus.Operation] { status?.operations.filter { !$0.acknowledged } ?? [] }
        var nextWarningAt: Date? {
            visible.filter { $0.phase == .unconfirmed }
                .map { $0.firstObservedUncertainAt.addingTimeInterval(120) }.filter { $0 > now }.min()
        }
        public var warningText: String {
            var parts: [String] = []
            let unconfirmed = warnings.filter { $0.phase == .unconfirmed }.count
            if unconfirmed > 0 { parts.append(String(localized: "\(unconfirmed) operations are still unconfirmed")) }
            let readRejected = warnings.filter { if case .feedbackReadRejected = $0.phase { true } else { false } }.count
            if readRejected > 0 { parts.append(String(localized: "Feedback delivered; \(readRejected) reports could not be marked read")) }
            let rejected = warnings.filter { if case .feedbackMessageRejected = $0.phase { true } else { $0.phase == .feedbackNeedsDraft } }.count
            if rejected > 0 { parts.append(String(localized: "\(rejected) feedback messages were not sent; review your drafts")) }
            return parts.joined(separator: ". ")
        }
    }
    public enum Action {
        case appeared, disappeared
        case updated(DeliveryStatus)
        case timeAdvanced(CommandScope, Date)
        case dismissWarnings
        case acknowledgementFinished(CommandScope, String?)
    }
    @Dependency(\.serviceCommandRecovery) var recovery
    @Dependency(\.continuousClock) var clock
    @Dependency(\.date.now) var now
    private enum CancelID { case observation, timer, acknowledgement }

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .appeared:
                return .run { send in
                    for await status in await recovery.observeDelivery() {
                        try Task.checkCancellation()
                        await send(.updated(status))
                    }
                }.cancellable(id: CancelID.observation, cancelInFlight: true)
            case .disappeared:
                state.isAcknowledging = false
                return .merge(.cancel(id: CancelID.observation), .cancel(id: CancelID.timer), .cancel(id: CancelID.acknowledgement))
            case .updated(let status):
                if state.status?.scope != status.scope {
                    state.isAcknowledging = false
                    state.acknowledgementError = nil
                }
                state.status = status
                state.now = now
                return scheduleWarning(state)
            case .timeAdvanced(let scope, let date):
                guard state.status?.scope == scope else { return .none }
                state.now = max(state.now, date)
                return scheduleWarning(state)
            case .dismissWarnings:
                guard !state.isAcknowledging, let status = state.status, !state.warnings.isEmpty else { return .none }
                let phases = Dictionary(uniqueKeysWithValues: state.warnings.map { ($0.id, $0.phase) })
                state.isAcknowledging = true
                state.acknowledgementError = nil
                return .run { send in
                    do {
                        try await recovery.acknowledgeDelivery(phases, status.scope)
                        await send(.acknowledgementFinished(status.scope, nil))
                    } catch is CancellationError { }
                    catch { await send(.acknowledgementFinished(status.scope, error.localizedDescription)) }
                }.cancellable(id: CancelID.acknowledgement, cancelInFlight: true)
            case .acknowledgementFinished(let scope, let error):
                guard state.status?.scope == scope else { return .none }
                state.isAcknowledging = false
                state.acknowledgementError = error
                // Only the service's persisted snapshot may hide a warning.
                return .none
            }
        }
    }
    private func scheduleWarning(_ state: State) -> Effect<Action> {
        guard let deadline = state.nextWarningAt, let scope = state.status?.scope else { return .cancel(id: CancelID.timer) }
        let delay = deadline.timeIntervalSince(state.now)
        return .run { send in
            try await clock.sleep(for: .seconds(delay))
            await send(.timeAdvanced(scope, deadline))
        }.cancellable(id: CancelID.timer, cancelInFlight: true)
    }
}
