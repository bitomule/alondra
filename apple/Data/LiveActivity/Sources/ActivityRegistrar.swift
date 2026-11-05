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
/// not heard yet; one that fails is told again at the next change or the next `retry`, so a relay
/// older than these routes costs nothing but the silent attempt.
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

    /// `forgetPhoneID` runs when the account ends: the relay keeps a phone to the account that
    /// registered it, so the next account needs another id.
    public init(phoneID: @escaping () throws -> String, forgetPhoneID: @escaping () -> Void = {}, environment: String) {
        self.phoneID = phoneID; self.forgetPhoneID = forgetPhoneID; self.environment = environment
    }

    /// While a duplicate waits to be ended, the relay speaks to the newest activity, the one kept.
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

    /// The pairings of the account now. With none left the phone leaves the relay through the
    /// last ones it had.
    public func set(_ pairings: [ActivityRelayPairing]) {
        if pairings.isEmpty {
            stop()
            return
        }
        self.pairings = pairings
        told = told.filter { id, _ in pairings.contains { $0.id == id } }
        keysGiven = keysGiven.filter { id, _ in pairings.contains { $0.id == id } }
        saidGone = false
        sync()
    }

    /// The account is ending: the relay hears it while the pairings can still sign.
    public func stop() {
        let leaving = pairings
        pairings = []
        told = [:]
        keysGiven = [:]
        enqueue { [self] in
            await unregister(through: leaving)
            forgetPhoneID()
            saidGone = false
        }
    }

    public func retry() { sync() }

    /// Waits until every change made so far has reached the relay.
    public func settled() async {
        await tail?.value
        await keyTail?.value
    }

    private func sync() {
        giveKeys()
        enqueue { [self] in
            guard ready else { return }
            if enabled { await register() } else { await unregister(through: pairings) }
        }
    }

    private func enqueue(_ work: @escaping @MainActor () async -> Void) {
        let previous = tail
        tail = Task {
            await previous?.value
            await work()
        }
    }

    /// A Mac asleep takes minutes to answer: its key never holds back the relay's registration.
    private func giveKeys() {
        let previous = keyTail
        keyTail = Task { [self] in
            await previous?.value
            for pairing in pairings {
                guard let key = pairing.publicKey, keysGiven[pairing.id] != key else { continue }
                if (try? await pairing.giveKey(key)) != nil { keysGiven[pairing.id] = key }
            }
        }
    }

    private func register() async {
        guard let id = try? phoneID(),
              let registration = try? RelayActivityRegistration(phoneID: id, startToken: startToken, updateToken: update?.token,
                  activityID: update?.activityID, startedAtMs: update?.startedAtMs, environment: environment,
                  frequentUpdates: frequentUpdates) else { return }
        for pairing in pairings where told[pairing.id] != registration {
            if (try? await pairing.register(registration)) != nil { told[pairing.id] = registration }
        }
    }

    /// The relay forgets the whole phone through any one pairing.
    private func unregister(through pairings: [ActivityRelayPairing]) async {
        guard !saidGone, let id = try? phoneID() else { return }
        for pairing in pairings where (try? await pairing.unregister(id)) != nil {
            told = [:]
            saidGone = true
            return
        }
    }
}
