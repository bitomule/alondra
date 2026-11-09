import AlondraTransport
import Foundation

/// One APNs token fans out to every approved binding. A selection change does not
/// replace another Mac's registration; logout cancels the whole generation.
@MainActor
public final class PushRegistrationCoordinator {
    public typealias State = RegistrationState
    public private(set) var state: State = .idle
    public var onChange: (@MainActor (State) -> Void)?
    private let preferences: PushPreferencesLedger
    private var clients: [NotificationPayload.Binding: NotificationClient] = [:]
    private var registered: [NotificationPayload.Binding: String] = [:]
    private var generation = 0
    private var task: Task<Void, Never>?
    private let pause: @Sendable (Duration) async throws -> Void

    public init(preferences: PushPreferencesLedger,
                pause: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }) {
        self.preferences = preferences
        self.pause = pause
    }

    public func update(clients next: [NotificationClient], token: String?, environment: String,
                       quietStart: String?, quietEnd: String?) throws {
        guard Set(next.map(\.binding)).count == next.count,
              Set(next.map(\.identity)).count == next.count else { throw Failure.duplicateIdentity }
        generation += 1
        let current = generation
        clients = Dictionary(uniqueKeysWithValues: next.map { ($0.binding, $0) })
        registered = registered.filter { clients[$0.key] != nil }
        let previous = task
        previous?.cancel()
        guard let token, !next.isEmpty else { setState(.idle); return }
        task = Task {
            // Keep token upserts ordered even if a transport ignores cancellation.
            await previous?.value
            guard current == generation, !Task.isCancelled else { return }
            setState(.sending)
            var attempt = 0
            var problems: [NotificationPayload.Binding: RegistrationFailure] = [:]
            var minimums: [NotificationPayload.Binding: UInt64] = [:]
            while true {
                for client in next {
                    guard current == generation, !Task.isCancelled else { return }
                    if let problem = problems[client.binding], !problem.retries { continue }
                    let fingerprint = "\(client.identity)|\(client.binding.value)|\(token)|\(environment)|\(quietStart ?? "off")|\(quietEnd ?? "off")"
                    if registered[client.binding] == fingerprint { continue }
                    do {
                        let revision = try await preferences.reserve(identity: client.identity, binding: client.binding.value,
                            quietStart: quietStart, quietEnd: quietEnd, newerThan: minimums[client.binding] ?? 0)
                        try Task.checkCancellation()
                        try await client.register(token, environment, quietStart, quietEnd, revision)
                        guard current == generation, !Task.isCancelled,
                              clients[client.binding]?.identity == client.identity else { return }
                        registered[client.binding] = fingerprint
                        problems[client.binding] = nil
                    } catch {
                        guard current == generation, !Task.isCancelled else { return }
                        let problem = RegistrationFailure.classify(error, operation: .mac)
                        if case .stalePreferences(let revision) = problem.reason { minimums[client.binding] = revision }
                        problems[client.binding] = problem
                    }
                }
                guard current == generation, !Task.isCancelled else { return }
                let remaining = next.compactMap { problems[$0.binding] }
                if remaining.isEmpty { setState(.registered); return }
                setState(.failed(remaining))
                guard remaining.contains(where: \.retries) else { return }
                do { try await pause(RegistrationFailure.retryDelay(attempt: attempt)) } catch { return }
                attempt = min(attempt + 1, 6)
            }
        }
    }

    /// The relay drops a registration once APNs calls its token dead, so a registration this
    /// process already sent may no longer exist: the next update sends every binding again.
    public func expireRegistrations() { registered = [:] }

    public func clear() {
        generation += 1
        clients = [:]; registered = [:]
        task?.cancel()
        setState(.idle)
    }

    func waitForPendingRegistration() async { await task?.value }
    private func setState(_ state: State) {
        guard state != self.state else { return }
        self.state = state
        onChange?(state)
    }

    enum Failure: LocalizedError, Equatable {
        case duplicateIdentity
        var errorDescription: String? {
            String(localized: "Two linked Macs look like the same Mac. Remove one in Settings and link it again.")
        }
    }
}
