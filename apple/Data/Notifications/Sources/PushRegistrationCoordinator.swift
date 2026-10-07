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

    public init(preferences: PushPreferencesLedger) { self.preferences = preferences }

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
            var failures: [String] = []
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
                    let message = Self.failureMessage(error)
                    if !failures.contains(message) { failures.append(message) }
                }
            }
            guard current == generation, !Task.isCancelled else { return }
            setState(failures.isEmpty ? .registered : .failed(failures.joined(separator: "\n")))
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
    private func setState(_ state: State) { self.state = state; onChange?(state) }
    /// What the Settings row says about a Mac that did not take this device's alerts.
    nonisolated static func failureMessage(_ error: any Error) -> String {
        switch error {
        case is URLError, RelayWireError.outcomeUnknown, RelaySessionError.outcomeUnknown:
            String(localized: "Your Mac couldn’t be reached to turn on alerts. Alondra tries again when you reopen the app.")
        default:
            String(localized: "Your Mac didn’t accept this device’s alerts. Alondra tries again when you reopen the app.")
        }
    }

    enum Failure: LocalizedError, Equatable {
        case duplicateIdentity
        var errorDescription: String? {
            String(localized: "Two linked Macs look like the same Mac. Remove one in Settings and link it again.")
        }
    }
}
