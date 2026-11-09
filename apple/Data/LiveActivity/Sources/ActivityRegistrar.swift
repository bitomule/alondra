import AlondraTransport
import Foundation

/// What iOS says about the Live Activity, in the order it says it.
public enum ActivityRegistrationEvent: Equatable, Sendable {
    case startToken(String)
    case updateToken(String, activityID: String, startedAt: Date)
    case activityEnded(String)
    case enabled(Bool)
    case frequentUpdates(Bool)
    /// Everything iOS already knew at launch was said: a registration before this would tell the
    /// relay the phone has no tokens.
    case ready
}

/// One pairing with a relay: the phone speaks to the relay once through each of them, so the relay
/// knows which Macs feed this phone's Live Activity.
public struct ActivityRelayPairing: Sendable {
    public let id: String
    public let register: @Sendable (RelayActivityRegistration) async throws -> Void
    public let unregister: @Sendable (String) async throws -> Void
    /// The public half of this pairing's key, and how the Mac hears it.
    public let publicKey: String?
    public let giveKey: @Sendable (String) async throws -> Void

    public init(id: String, register: @escaping @Sendable (RelayActivityRegistration) async throws -> Void,
                unregister: @escaping @Sendable (String) async throws -> Void, publicKey: String? = nil,
                giveKey: @escaping @Sendable (String) async throws -> Void = { _ in }) {
        self.id = id; self.register = register; self.unregister = unregister; self.publicKey = publicKey; self.giveKey = giveKey
    }
}

/// Keeps every relay pairing told of this phone's tokens. Each pairing is told only what it has
/// not heard yet. Temporary failures retry independently of the key delivery to each Mac.
@MainActor
public final class ActivityRegistrar {
    private struct Update: Equatable {
        let token: String
        let activityID: String
        let startedAtMs: UInt64
    }

    private let phoneID: () throws -> String
    private let forgetPhoneID: () -> Void
    private let environment: String
    private var pairings: [ActivityRelayPairing] = []
    private var told: [String: RelayActivityRegistration] = [:]
    private var keysGiven: [String: String] = [:]
    private var startToken: String?
    private var updates: [String: Update] = [:]
    private var ready = false
    private var enabled = true
    private var frequentUpdates = true
    private var saidGone = false
    private var tail: Task<Void, Never>?
    private var keyTail: Task<Void, Never>?

    public var onChange: (@MainActor () -> Void)?
    public private(set) var relayState: RegistrationState = .idle { didSet { onChange?() } }
    public private(set) var keyState: RegistrationState = .idle { didSet { onChange?() } }
    private var generation = 0
    private var cleanupTask: Task<Void, Never>?
    private let pause: @Sendable (Duration) async throws -> Void

    /// `forgetPhoneID` runs when the account ends: the next account needs another id.
    public init(phoneID: @escaping () throws -> String, forgetPhoneID: @escaping () -> Void = {}, environment: String,
                pause: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }) {
        self.phoneID = phoneID; self.forgetPhoneID = forgetPhoneID; self.environment = environment; self.pause = pause
    }

    private var update: Update? { updates.values.max { $0.startedAtMs < $1.startedAtMs } }

    public func handle(_ event: ActivityRegistrationEvent) {
        switch event {
        case .startToken(let token): startToken = token
        case .updateToken(let token, let activityID, let startedAt):
            updates[activityID] = Update(token: token, activityID: activityID, startedAtMs: UInt64(max(0, startedAt.timeIntervalSince1970 * 1000)))
        case .activityEnded(let activityID): updates[activityID] = nil
        case .enabled(let enabled):
            self.enabled = enabled
            if enabled { saidGone = false }
        case .frequentUpdates(let enabled): frequentUpdates = enabled
        case .ready: ready = true
        }
        sync()
    }

    public func set(_ pairings: [ActivityRelayPairing]) {
        if pairings.isEmpty { stop(); return }
        self.pairings = pairings
        told = told.filter { id, _ in pairings.contains { $0.id == id } }
        keysGiven = keysGiven.filter { id, _ in pairings.contains { $0.id == id } }
        saidGone = false
        sync()
    }

    public func stop() {
        generation += 1
        let leaving = pairings
        let id = leaving.isEmpty ? nil : try? phoneID()
        pairings = []; told = [:]; keysGiven = [:]
        tail?.cancel(); keyTail?.cancel()
        relayState = .idle; keyState = .idle
        if !leaving.isEmpty { forgetPhoneID() }
        saidGone = false
        let previous = tail
        let previousCleanup = cleanupTask
        cleanupTask = Task {
            await previousCleanup?.value
            await previous?.value
            guard let id else { return }
            // Clean up the old account using its captured id, even if a new account has started.
            for pairing in leaving {
                if (try? await pairing.unregister(id)) != nil { return }
            }
        }
    }

    public func retry() { sync() }

    public func settled() async {
        await tail?.value
        await keyTail?.value
        await cleanupTask?.value
    }

    private func sync() {
        generation += 1
        let current = generation
        let active = pairings
        let previousKey = keyTail
        previousKey?.cancel()
        keyTail = Task { [self] in
            await previousKey?.value
            guard isCurrent(current) else { return }
            let keyed = active.filter { $0.publicKey != nil }
            await repeatRegistration(keyed, operation: .activityKey, generation: current) { pairing in
                guard let key = pairing.publicKey, keysGiven[pairing.id] != key else { return }
                try await pairing.giveKey(key)
                guard isCurrent(current) else { return }
                keysGiven[pairing.id] = key
            }
        }
        let previous = tail
        let cleanup = cleanupTask
        previous?.cancel()
        tail = Task { [self] in
            await previous?.value
            await cleanup?.value
            guard isCurrent(current) else { return }
            guard ready, !active.isEmpty else { relayState = .idle; return }
            do {
                let id = try phoneID()
                let registration = try RelayActivityRegistration(phoneID: id, startToken: startToken, updateToken: update?.token,
                    activityID: update?.activityID, startedAtMs: update?.startedAtMs, environment: environment,
                    frequentUpdates: frequentUpdates)
                let registering = enabled
                await repeatRegistration(active, operation: .relay, generation: current,
                    isComplete: { !registering && saidGone }) { pairing in
                    if registering {
                        guard told[pairing.id] != registration else { return }
                        try await pairing.register(registration)
                        guard isCurrent(current) else { return }
                        told[pairing.id] = registration
                    } else {
                        guard !saidGone else { return }
                        try await pairing.unregister(id)
                        guard isCurrent(current) else { return }
                        told = [:]; saidGone = true
                    }
                }
            } catch {
                guard isCurrent(current) else { return }
                relayState = .failed([RegistrationFailure.classify(error, operation: .relay)])
            }
        }
    }

    private func isCurrent(_ current: Int) -> Bool { current == generation && !Task.isCancelled }

    /// The key and token loops are independent: a sleeping Mac never holds back the relay.
    private func repeatRegistration(_ active: [ActivityRelayPairing], operation: RegistrationFailure.Operation, generation current: Int,
                                    isComplete: @MainActor () -> Bool = { false },
                                    send: @MainActor (ActivityRelayPairing) async throws -> Void) async {
        func state(_ value: RegistrationState) {
            if operation == .activityKey { keyState = value } else { relayState = value }
        }
        guard !active.isEmpty else { state(.idle); return }
        state(.sending)
        var failures: [String: RegistrationFailure] = [:]
        var attempt = 0
        while isCurrent(current) {
            for pairing in active {
                guard isCurrent(current) else { return }
                if let failure = failures[pairing.id], !failure.retries { continue }
                do {
                    try await send(pairing)
                    guard isCurrent(current) else { return }
                    failures[pairing.id] = nil
                    if isComplete() { state(.registered); return }
                } catch {
                    guard isCurrent(current) else { return }
                    failures[pairing.id] = RegistrationFailure.classify(error, operation: operation)
                }
            }
            guard isCurrent(current) else { return }
            let remaining = active.compactMap { failures[$0.id] }
            if remaining.isEmpty { state(.registered); return }
            state(.failed(remaining))
            guard remaining.contains(where: \.retries) else { return }
            do { try await pause(RegistrationFailure.retryDelay(attempt: attempt)) } catch { return }
            attempt = min(attempt + 1, 6)
        }
    }
}
