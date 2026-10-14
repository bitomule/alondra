import AlondraFeed
import ComposableArchitecture
import Foundation

public struct ConnectionMeasurementsClient: Sendable {
    public var summary: @Sendable () async -> [ConnectionMeasurementSummary]
    public var clear: @Sendable () async -> Void
    /// The file Share sends: one JSON line per request.
    public var file: URL?

    public init(summary: @escaping @Sendable () async -> [ConnectionMeasurementSummary],
                clear: @escaping @Sendable () async -> Void, file: URL?) {
        self.summary = summary; self.clear = clear; self.file = file
    }

    public static func live(_ measurements: ConnectionMeasurements) -> Self {
        Self(summary: { await measurements.summary() }, clear: { await measurements.clear() }, file: measurements.file)
    }
}

extension ConnectionMeasurementsClient: DependencyKey {
    public static let liveValue = Self(summary: { [] }, clear: {}, file: nil)
}

public extension DependencyValues {
    var connectionMeasurements: ConnectionMeasurementsClient {
        get { self[ConnectionMeasurementsClient.self] }
        set { self[ConnectionMeasurementsClient.self] = newValue }
    }
}
