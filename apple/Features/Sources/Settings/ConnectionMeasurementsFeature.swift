import AlondraFeed
import ComposableArchitecture
import Foundation

/// Settings' "Connection measurements": how long requests to the Macs took, by path and network.
@Reducer public struct ConnectionMeasurementsFeature: Sendable {
    @ObservableState public struct State: Equatable {
        public var summary: [ConnectionMeasurementSummary] = []
        public var file: URL?
        public init() {}
    }

    public enum Action {
        case task, loaded([ConnectionMeasurementSummary]), clear
    }

    @Dependency(\.connectionMeasurements) var measurements

    public init() {}

    public var body: some ReducerOf<Self> {
        Reduce { state, action in
            switch action {
            case .task:
                state.file = measurements.file
                return .run { send in await send(.loaded(await measurements.summary())) }
            case .loaded(let summary):
                state.summary = summary
                return .none
            case .clear:
                state.summary = []
                return .run { _ in await measurements.clear() }
            }
        }
    }
}
