#if os(iOS)
import ActivityKit
import Foundation

/// The type ActivityKit knows Alondra's Live Activity by: the app, the widget extension and a
/// push that starts one (its `attributes-type`) all name it.
public struct AlondraActivityAttributes: ActivityAttributes {
    public typealias ContentState = ActivityContentState

    public var startedAtMs: Int64

    public init(startedAt: Date) { startedAtMs = Int64((startedAt.timeIntervalSince1970 * 1000).rounded()) }

    public var startedAt: Date { Date(timeIntervalSince1970: Double(startedAtMs) / 1000) }

    enum CodingKeys: String, CodingKey { case startedAtMs = "started_at_ms" }
}

@MainActor
public final class ActivityKitHost: ActivityHost {
    public init() {}

    public var activitiesEnabled: Bool { ActivityAuthorizationInfo().areActivitiesEnabled }

    public func running() -> [RunningActivity] {
        Activity<AlondraActivityAttributes>.activities
            .filter { $0.activityState == .active || $0.activityState == .stale }
            .map { RunningActivity(id: $0.id, startedAt: $0.attributes.startedAt) }
    }

    public func request(_ state: ActivityContentState, startedAt: Date, staleDate: Date) throws -> RunningActivity {
        let activity = try Activity.request(attributes: AlondraActivityAttributes(startedAt: startedAt),
                                            content: ActivityContent(state: state, staleDate: staleDate))
        return RunningActivity(id: activity.id, startedAt: activity.attributes.startedAt)
    }

    public func update(_ id: String, to state: ActivityContentState, staleDate: Date) async {
        await Self.update(id, ActivityContent(state: state, staleDate: staleDate))
    }

    public func end(_ id: String, with state: ActivityContentState?, dismissal: ActivityDismissal) async {
        let policy: ActivityUIDismissalPolicy = switch dismissal {
        case .immediate: .immediate
        case .after(let date): .after(date)
        }
        await Self.end(id, state.map { ActivityContent(state: $0, staleDate: nil) }, policy)
    }

    /// Yields once for every Live Activity iOS starts, by the app or by a push.
    public nonisolated static func started() -> AsyncStream<Void> {
        AsyncStream { continuation in
            let task = Task {
                for await _ in Activity<AlondraActivityAttributes>.activityUpdates { continuation.yield() }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private nonisolated static func update(_ id: String, _ content: ActivityContent<ActivityContentState>) async {
        await Activity<AlondraActivityAttributes>.activities.first { $0.id == id }?.update(content)
    }

    private nonisolated static func end(_ id: String, _ content: ActivityContent<ActivityContentState>?,
                                        _ policy: ActivityUIDismissalPolicy) async {
        await Activity<AlondraActivityAttributes>.activities.first { $0.id == id }?.end(content, dismissalPolicy: policy)
    }
}
#endif
