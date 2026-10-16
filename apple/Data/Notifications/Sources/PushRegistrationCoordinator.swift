import AlondraTransport
import Foundation

/// One APNs token fans out to every approved binding. A selection change does not
/// replace another Mac's registration; logout cancels the whole generation.
@MainActor
public final class PushRegistrationCoordinator {
    public enum State: Equatable, Sendable {
        case idle, sending, registered, failed(String)
    }
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
            while true {
                var problems: [Problem] = []
                for client in next {
                    guard current == generation, !Task.isCancelled else { return }
                    let fingerprint = "\(client.identity)|\(client.binding.value)|\(token)|\(environment)|\(quietStart ?? "off")|\(quietEnd ?? "off")"
                    if registered[client.binding] == fingerprint { continue }
                    do {
                        let revision = try await preferences.reserve(identity: client.identity, binding: client.binding.value,
                            quietStart: quietStart, quietEnd: quietEnd)
                        try Task.checkCancellation()
                        do { try await client.register(token, environment, quietStart, quietEnd, revision) }
                        catch PushPreferencesError.stale(let minimum) {
                            try Task.checkCancellation()
                            let newer = try await preferences.reserve(identity: client.identity, binding: client.binding.value,
                                quietStart: quietStart, quietEnd: quietEnd, newerThan: minimum)
                            try Task.checkCancellation()
                            try await client.register(token, environment, quietStart, quietEnd, newer)
                        }
                        guard current == generation, !Task.isCancelled,
                              clients[client.binding]?.identity == client.identity else { return }
                        registered[client.binding] = fingerprint
                    } catch {
                        guard current == generation, !Task.isCancelled else { return }
                        let problem = Self.problem(error)
                        if !problems.contains(problem) { problems.append(problem) }
                    }
                }
                guard current == generation, !Task.isCancelled else { return }
                if problems.isEmpty { setState(.registered); return }
                setState(.failed(problems.map(\.message).joined(separator: "\n")))
                guard problems.contains(where: \.retries) else { return }
                do { try await pause(Self.retryDelay(attempt: attempt)) } catch { return }
                attempt += 1
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

    /// Network and relay trouble clears on its own, so it is retried in the background;
    /// a refusal from the Mac or a fault on this device waits for the app to open again.
    enum Problem: Equatable, Sendable {
        case unreachable, relay, rejected, local

        var retries: Bool { self == .unreachable || self == .relay }

        var message: String {
            switch self {
            case .unreachable: String(localized: "Can’t reach your Mac. Retrying…")
            case .relay: String(localized: "Alondra’s relay isn’t responding. Retrying…")
            case .rejected:
                String(localized: "Your Mac didn’t accept this device’s alerts. Alondra tries again when you reopen the app.")
            case .local:
                String(localized: "Alerts couldn’t be set up on this device. Alondra tries again when you reopen the app.")
            }
        }
    }

    nonisolated static func problem(_ error: any Error) -> Problem {
        switch error {
        case is URLError, RelayWireError.outcomeUnknown, SealedRequestError.outcomeUnknown, OperationError.outcomeUnknown:
            return .unreachable
        case is RelayWireError, SealedRequestError.invalidResponse:
            return .relay
        case let error as APIError:
            switch error {
            case .unavailable: return .unreachable
            case .unexpected(let status) where status >= 500: return .unreachable
            case .server(let status, _, _, _) where status >= 500: return .unreachable
            case .unauthorized, .forbidden, .conflict, .server, .unexpected: return .rejected
            }
        case is PushPreferencesError:
            return .rejected
        default:
            return .local
        }
    }

    nonisolated static func retryDelay(attempt: Int) -> Duration {
        .seconds(min(5 * (1 << min(attempt, 6)), 300))
    }

    enum Failure: LocalizedError, Equatable {
        case duplicateIdentity
        var errorDescription: String? {
            String(localized: "Two linked Macs look like the same Mac. Remove one in Settings and link it again.")
        }
    }
}
