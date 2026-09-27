import AlondraCommands
import AlondraContracts
import Foundation

public struct DeliveryJournalSnapshot: Equatable, Sendable {
    public let scope: CommandScope
    public let macID: String
    public let attempts: [DeliveryAttempt]
    public init(scope: CommandScope, macID: String, attempts: [DeliveryAttempt]) {
        self.scope = scope; self.macID = macID; self.attempts = attempts
    }
}

public enum DeliveryJournalError: Error, LocalizedError, Sendable {
    case obsolete, changed, confirmationRequired, invalidResponse, outcomeUnknown
    public var errorDescription: String? {
        switch self {
        case .obsolete: "This Mac connection is no longer active."
        case .changed: "This delivery changed. Refresh and inspect it before deciding."
        case .confirmationRequired: "Inspect the terminal and confirm the risk before retrying."
        case .invalidResponse: "The delivery result could not be verified. Inspect the terminal before deciding."
        case .outcomeUnknown: "The decision is unconfirmed. Its status will be checked without sending it again."
        }
    }
}

/// One approved Mac owns the closures and all in-flight work. Never falls back to another Mac.
public actor DeliveryJournal {
    private let scope: CommandScope
    private let macID: String
    private let read: @Sendable () async throws -> [DeliveryAttempt]
    private let decide: @Sendable (DeliveryAttempt, Bool) async -> CommandResult
    private var active = true
    private var resolving = false
    public init(scope: CommandScope, macID: String,
                read: @escaping @Sendable () async throws -> [DeliveryAttempt],
                decide: @escaping @Sendable (DeliveryAttempt, Bool) async -> CommandResult) {
        self.scope = scope; self.macID = macID; self.read = read; self.decide = decide
    }
    public func stop() { active = false }
    private func check() throws {
        try Task.checkCancellation()
        guard active else { throw DeliveryJournalError.obsolete }
    }
    public func refresh() async throws -> DeliveryJournalSnapshot {
        try check()
        let attempts = try await read()
        try check()
        guard Set(attempts.map(\.id)).count == attempts.count else { throw DeliveryJournalError.invalidResponse }
        return DeliveryJournalSnapshot(scope: scope, macID: macID, attempts: attempts)
    }
    public func resolve(_ observed: DeliveryAttempt, in expected: CommandScope, retry: Bool,
                        terminalChecked: Bool, duplicateRisk: Bool) async throws -> DeliveryJournalSnapshot {
        try check()
        guard expected == scope else { throw DeliveryJournalError.obsolete }
        guard terminalChecked, !retry || duplicateRisk else { throw DeliveryJournalError.confirmationRequired }
        guard !resolving else { throw DeliveryJournalError.outcomeUnknown }
        resolving = true
        defer { resolving = false }
        let fresh = try await refresh()
        guard let current = fresh.attempts.first(where: { $0.id == observed.id }), current == observed,
              current.canResolve else { throw DeliveryJournalError.changed }
        try check()
        let result = await decide(current, retry)
        try check()
        switch result {
        case .confirmed: return try await refresh()
        case .unknown: throw DeliveryJournalError.outcomeUnknown
        case .obsolete: throw DeliveryJournalError.obsolete
        case .rejected(let error): throw error
        }
    }
}
