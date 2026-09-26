import AlondraAPIClient
import AlondraCommands
import AlondraQuestionsFeature
import AlondraRelaySession
import Foundation

/// Headless orchestration shared by app scenes. Own one instance per approved identity.
public struct ReportFeedbackCoordinator: Sendable {
    private let transaction: ReportFeedbackTransaction
    private let scope: CommandScope
    private let api: AlondraAPIClient
    private let operations: RelaySession

    public init(directory: URL, gate: CommandGate, scope: CommandScope,
                api: AlondraAPIClient, operations: RelaySession) {
        transaction = ReportFeedbackTransaction(directory: directory, gate: gate, scope: scope)
        self.scope = scope; self.api = api; self.operations = operations
    }

    public func submit(_ request: ReportFeedbackRequest, identity: QuestionWorkIdentity) async -> QuestionWorkOutcome {
        guard identity.server == scope.server, identity.deviceID == scope.deviceID,
              identity.credentialRevision == scope.credentialRevision, identity.generation == scope.generation
        else { return .obsolete }
        let result = await transaction.submit(reportID: request.reportID, sendMessage: { id in
            _ = try await api.send(AlondraEndpoints.send(to: request.sessionID, text: request.text,
                attachments: request.attachmentIDs), operationID: id)
        }, markRead: { id in
            try await api.markRead(request.reportID, operationID: id)
        }, reconcile: { id in
            if case .completed(let status, _) = try await operations.resume(requestID: id) {
                return (200..<300).contains(status)
            }
            return false
        })
        switch result {
        case .confirmed: return .confirmed
        case .unknown: return .uncertain
        case .obsolete: return .obsolete
        case .rejected(let error): return .failed(error.localizedDescription)
        }
    }
}
