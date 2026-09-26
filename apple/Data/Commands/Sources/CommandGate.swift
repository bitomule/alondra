import Foundation

/// Describes the effect for reconciliation. The payload stays with the caller until it has
/// been sent once; no closure or payload is retained for an automatic replay.
public enum CommandKind: Hashable, Sendable, Codable {
    case answer(UInt64)
    case moreContext(UInt64)
    case markRead(UInt64)
    case review(UInt64)
    case send(String)
    case replyToReport(UInt64)
    case newRequest
    case closeNode(String)
    case cancelTask(String)
    case reopenTask(String)
    case withdrawRequest(String)
    case archiveCompleted
    case mergePullRequest(UInt64)
    case resolveConflicts(UInt64)
    case requestChanges(UInt64)
    case askOnPullRequest(UInt64)
    case dismissPullRequest(UInt64)
    case task(String)


}

public struct CommandScope: Hashable, Sendable, Codable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String
    public let generation: UInt64

    public init(server: URL, deviceID: String, credentialRevision: String, generation: UInt64) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
        self.generation = generation
    }

    /// Generations can restart at launch; the credential revision is the durable identity.
    public func sameIdentity(as other: Self) -> Bool {
        server == other.server && deviceID == other.deviceID && credentialRevision == other.credentialRevision
    }
}

public struct UncertainCommand: Identifiable, Equatable, Sendable, Codable {
    public let id: UUID
    public let scope: CommandScope
    public let kind: CommandKind
    public let itemID: String?

    public init(id: UUID, scope: CommandScope, kind: CommandKind, itemID: String?) {
        self.id = id
        self.scope = scope
        self.kind = kind
        self.itemID = itemID
    }

    /// One unresolved card blocks all of its actions, even if the user taps another button.
    public func conflicts(with kind: CommandKind, itemID: String?, scope: CommandScope) -> Bool {
        guard self.scope.sameIdentity(as: scope) else { return false }
        if let itemID { return self.itemID == itemID }
        return self.itemID == nil && self.kind == kind
    }
}

public enum CommandResult: Sendable {
    case confirmed
    case unknown(UncertainCommand)
    case rejected(any Error)
    case obsolete
}

/// One-shot submission. A lost response is kept as a fact to reconcile against a later
/// snapshot, never as an executable operation. Switching scope invalidates in-flight results.
public actor CommandGate {
    public private(set) var scope: CommandScope
    public private(set) var uncertain: [UncertainCommand] = []
    private var generation: UInt64 = 0
    private let journal: (any CommandJournaling)?
    private var loaded = false
    private var pending: [UncertainCommand] = []
    private var mutationVersion: UInt64 = 0
    private var dismissalInFlight = false
    private var dismissalWaiters: [CheckedContinuation<Void, Never>] = []

    public init(scope: CommandScope, journal: (any CommandJournaling)? = nil) {
        self.scope = scope
        self.journal = journal
    }

    /// Loads warnings for this identity. The gate also calls this before its first send.
    /// A failed read blocks submission; it must not silently forget a prior attempt.
    @discardableResult public func restore() async throws -> [UncertainCommand] {
        guard let journal else { loaded = true; return uncertain }
        let restoringScope = scope
        let restoringGeneration = generation
        let restoringVersion = mutationVersion
        let records = try await journal.records()
        guard restoringScope == scope, restoringGeneration == generation,
              restoringVersion == mutationVersion else { return uncertain }
        let relevant = records.filter { $0.command.scope.sameIdentity(as: restoringScope) }
        uncertain = relevant.filter { $0.state == .pending || $0.state == .unknown }.map(\.command)
        loaded = true
        return uncertain
    }

    public func use(_ next: CommandScope) async {
        // A dismissal accepted for the old scope must finish before a new scope activates.
        while dismissalInFlight {
            await withCheckedContinuation { dismissalWaiters.append($0) }
        }
        guard next != scope else { return }
        guard next.generation > scope.generation else { return }
        scope = next
        generation &+= 1
        mutationVersion &+= 1
        uncertain = []
        pending = []
        loaded = false
        // The next submission will fail closed if this identity cannot be read.
        _ = try? await restore()
    }

    /// A delayed UI dismissal must never remove a warning after its credential changed.
    public func dismiss(_ id: UUID, in expectedScope: CommandScope) async {
        guard expectedScope == scope, !dismissalInFlight,
              uncertain.contains(where: { $0.id == id }) else { return }
        dismissalInFlight = true
        defer {
            dismissalInFlight = false
            let waiters = dismissalWaiters
            dismissalWaiters = []
            for waiter in waiters { waiter.resume() }
        }
        if let journal {
            do { try await journal.remove(id) }
            catch { return } // Keep the warning if the file could not be removed.
        }
        uncertain.removeAll { $0.id == id }
        mutationVersion &+= 1
    }

    public func submit(
        _ kind: CommandKind,
        itemID: String? = nil,
        in expectedScope: CommandScope,
        isUnknown: @Sendable (any Error) -> Bool,
        operation: @Sendable () async throws -> Void
    ) async -> CommandResult {
        await submitIdentified(kind, itemID: itemID, in: expectedScope, isUnknown: isUnknown) { _ in
            try await operation()
        }
    }

    /// Supplies the identity reserved in the durable command journal. The service must use
    /// this same ID for its encrypted outbox; a second generated ID breaks crash recovery.
    public func submitIdentified(
        _ kind: CommandKind,
        itemID: String? = nil,
        in expectedScope: CommandScope,
        isUnknown: @Sendable (any Error) -> Bool,
        operation: @Sendable (UUID) async throws -> Void
    ) async -> CommandResult {
        guard expectedScope == scope else { return .obsolete }
        let submittedGeneration = generation
        if !loaded {
            do { try await restore() }
            catch { return .rejected(error) }
        }
        guard expectedScope == scope, submittedGeneration == generation else { return .obsolete }
        if let existing = (pending + uncertain).first(where: { $0.conflicts(with: kind, itemID: itemID, scope: expectedScope) }) {
            return .unknown(existing)
        }
        let command = UncertainCommand(id: UUID(), scope: expectedScope, kind: kind, itemID: itemID)
        pending.append(command)
        mutationVersion &+= 1
        if let journal {
            do {
                if case .existing(let existing) = try await journal.reserve(command) {
                    pending.removeAll { $0.id == command.id }
                    mutationVersion &+= 1
                    guard submittedGeneration == generation else { return .obsolete }
                    if !uncertain.contains(where: { $0.id == existing.id }) { uncertain.append(existing) }
                    return .unknown(existing)
                }
            }
            catch {
                pending.removeAll { $0.id == command.id }
                mutationVersion &+= 1
                return .rejected(error)
            }
        }
        // The actor may have reentered while persisting. Never send from a stale scope.
        guard expectedScope == scope, submittedGeneration == generation else {
            pending.removeAll { $0.id == command.id }
            mutationVersion &+= 1
            return .obsolete
        }
        do {
            try await operation(command.id)
            guard submittedGeneration == generation else { return .obsolete }
            if let journal {
                do { try await journal.save(.init(command: command, state: .confirmed)) }
                catch {
                    pending.removeAll { $0.id == command.id }
                    mutationVersion &+= 1
                    guard submittedGeneration == generation else { return .obsolete }
                    uncertain.append(command)
                    return .unknown(command)
                }
            }
            pending.removeAll { $0.id == command.id }
            mutationVersion &+= 1
            guard submittedGeneration == generation else { return .obsolete }
            uncertain.removeAll { $0.id == command.id }
            return .confirmed
        } catch {
            guard submittedGeneration == generation else { return .obsolete }
            let outcome: CommandJournalState = isUnknown(error) ? .unknown : .confirmed
            if let journal {
                do { try await journal.save(.init(command: command, state: outcome)) }
                catch {
                    pending.removeAll { $0.id == command.id }
                    mutationVersion &+= 1
                    guard submittedGeneration == generation else { return .obsolete }
                    uncertain.append(command)
                    return .unknown(command)
                }
            }
            pending.removeAll { $0.id == command.id }
            mutationVersion &+= 1
            guard submittedGeneration == generation else { return .obsolete }
            guard outcome == .unknown else { return .rejected(error) }
            uncertain.append(command)
            return .unknown(command)
        }
    }
}
