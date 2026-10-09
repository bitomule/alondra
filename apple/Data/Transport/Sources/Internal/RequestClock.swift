import Foundation

struct RequestClock: Sendable {
    let now: @Sendable () -> ContinuousClock.Instant
    let waitUntil: @Sendable (ContinuousClock.Instant) async throws -> Void

    static let continuous = Self(now: { ContinuousClock().now }, waitUntil: { try await ContinuousClock().sleep(until: $0) })
}
