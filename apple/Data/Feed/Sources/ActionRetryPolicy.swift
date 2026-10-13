import Foundation

/// How long an undelivered answer waits before it goes again: a ceiling that doubles from one
/// second up to thirty, each wait drawn at random below it, so devices that lost the network
/// together do not all come back at the same instant.
public struct ActionRetryPolicy: Sendable {
    public var delay: @Sendable (_ attempt: Int) -> Duration
    public var sleep: @Sendable (Duration) async throws -> Void

    public init(delay: @escaping @Sendable (_ attempt: Int) -> Duration,
                sleep: @escaping @Sendable (Duration) async throws -> Void) {
        self.delay = delay
        self.sleep = sleep
    }

    public static let maximumDelay = Duration.seconds(30)

    public static func ceiling(attempt: Int) -> Duration {
        let doubling = Duration.seconds(1) * (1 << min(max(attempt, 1) - 1, 5))
        return min(doubling, maximumDelay)
    }

    public static let live = ActionRetryPolicy(
        delay: { attempt in
            let ceiling = ceiling(attempt: attempt).components.seconds * 1000
            return .milliseconds(Int64.random(in: 250...max(250, ceiling)))
        },
        sleep: { try await Task.sleep(for: $0) }
    )
}
