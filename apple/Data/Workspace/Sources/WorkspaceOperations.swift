import AlondraAPIClient
import AlondraLoopbackSession
import AlondraRelaySession

/// The transport beneath one workspace: the relay's durable outbox for a remote Mac, or signed
/// loopback HTTP to the daemon on this Mac.
protocol WorkspaceOperations: ServiceOperationTransport {
    func recoveryStatus(requestID: String) async throws -> RelayRecoveryStatus
    func latestOutcome(requestID: String) async throws -> RelayOperationResult
    func deactivate() async
}

extension RelaySession: WorkspaceOperations {
    func latestOutcome(requestID: String) async throws -> RelayOperationResult {
        try await resume(requestID: requestID, statusPolls: 1)
    }
}

/// Without a durable outbox, a command this process did not see complete is unknown, never
/// "not dispatched": recovery must not send it again.
extension LoopbackSession: WorkspaceOperations {
    func recoveryStatus(requestID: String) async throws -> RelayRecoveryStatus {
        switch outcome(requestID: requestID) {
        case let .completed(status, body): .completed(status: status, body: body)
        case .unknown: .unknown
        }
    }

    func latestOutcome(requestID: String) async throws -> RelayOperationResult {
        switch outcome(requestID: requestID) {
        case let .completed(status, body): .completed(status: status, body: body)
        case .unknown: .unknown
        }
    }
}
