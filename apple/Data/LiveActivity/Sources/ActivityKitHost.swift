#if os(iOS)
import ActivityKit
import Foundation

/// The type ActivityKit knows Alondra's Live Activity by: the app, the widget extension and a
/// push that starts one (its `attributes-type`) all name it.
public struct AlondraActivityAttributes: ActivityAttributes {
    public typealias ContentState = ActivityContentState

    public var startedAtMs: Int64

    public init(startedAt: Date) { startedAtMs = Int64((startedAt.timeIntervalSince1970 * 1000).rounded()) }

    /// A relay from before it sent the start time pushes none.
    public init(from decoder: any Decoder) throws {
        startedAtMs = try decoder.container(keyedBy: CodingKeys.self).decodeIfPresent(Int64.self, forKey: .startedAtMs) ?? 0
    }

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

    public func content(of id: String) -> ActivityContentState? {
        Activity<AlondraActivityAttributes>.activities.first { $0.id == id }?.content.state
    }

    public func request(_ state: ActivityContentState, startedAt: Date, staleDate: Date) throws -> RunningActivity {
        let activity = try Activity.request(attributes: AlondraActivityAttributes(startedAt: startedAt),
                                            content: ActivityContent(state: state, staleDate: staleDate), pushType: .token)
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

    /// Everything the relay has to hear from iOS: both kinds of token, and the two switches in
    /// Settings. It starts first thing at launch, because iOS can start an activity for a closed
    /// app and hand its token over only when the app runs.
    public nonisolated static func registrationEvents() -> AsyncStream<ActivityRegistrationEvent> {
        AsyncStream { continuation in
            if let token = Activity<AlondraActivityAttributes>.pushToStartToken { continuation.yield(.startToken(token.hexadecimal)) }
            for activity in Activity<AlondraActivityAttributes>.activities {
                guard let token = activity.pushToken else { continue }
                continuation.yield(.updateToken(token.hexadecimal, activityID: activity.id, startedAt: activity.attributes.startedAt))
            }
            let info = ActivityAuthorizationInfo()
            continuation.yield(.enabled(info.areActivitiesEnabled))
            continuation.yield(.frequentUpdates(info.frequentPushesEnabled))
            continuation.yield(.ready)
            let startTokens = Task {
                for await token in Activity<AlondraActivityAttributes>.pushToStartTokenUpdates {
                    continuation.yield(.startToken(token.hexadecimal))
                }
            }
            let switches = Task {
                await withTaskGroup(of: Void.self) { group in
                    group.addTask {
                        for await enabled in ActivityAuthorizationInfo().activityEnablementUpdates { continuation.yield(.enabled(enabled)) }
                    }
                    group.addTask {
                        for await enabled in ActivityAuthorizationInfo().frequentPushEnablementUpdates { continuation.yield(.frequentUpdates(enabled)) }
                    }
                }
            }
            let activities = Task {
                await withTaskGroup(of: Void.self) { group in
                    for id in Activity<AlondraActivityAttributes>.activities.map(\.id) {
                        group.addTask { await observe(id, continuation) }
                    }
                    for await activity in Activity<AlondraActivityAttributes>.activityUpdates {
                        let id = activity.id
                        group.addTask { await observe(id, continuation) }
                    }
                }
            }
            continuation.onTermination = { _ in
                startTokens.cancel()
                switches.cancel()
                activities.cancel()
            }
        }
    }

    private nonisolated static func observe(_ id: String, _ continuation: AsyncStream<ActivityRegistrationEvent>.Continuation) async {
        await withTaskGroup(of: Void.self) { group in
            group.addTask {
                guard let activity = Activity<AlondraActivityAttributes>.activities.first(where: { $0.id == id }) else { return }
                for await token in activity.pushTokenUpdates {
                    continuation.yield(.updateToken(token.hexadecimal, activityID: id, startedAt: activity.attributes.startedAt))
                }
            }
            group.addTask {
                guard let activity = Activity<AlondraActivityAttributes>.activities.first(where: { $0.id == id }) else { return }
                for await state in activity.activityStateUpdates where state == .ended || state == .dismissed {
                    continuation.yield(.activityEnded(id))
                }
            }
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

private extension Data {
    var hexadecimal: String { map { String(format: "%02x", $0) }.joined() }
}
#endif
