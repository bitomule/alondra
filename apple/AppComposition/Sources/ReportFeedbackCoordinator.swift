import AlondraAPIClient
import AlondraCommands
import AlondraQuestionsFeature
import AlondraRelaySession
import Foundation
import CryptoKit

/// Headless orchestration shared by app scenes. Own one instance per approved identity.
public struct ReportFeedbackCoordinator: Sendable {
    private let transaction: ReportFeedbackTransaction
    private let scope: CommandScope
    private let api: AlondraAPIClient
    private let operations: RelaySession
    private let bindingKey: SymmetricKey

    public init(directory: URL, gate: CommandGate, scope: CommandScope,
                api: AlondraAPIClient, operations: RelaySession, bindingSecret: Data) {
        transaction = ReportFeedbackTransaction(directory: directory, gate: gate, scope: scope)
        self.scope = scope; self.api = api; self.operations = operations
        bindingKey = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: bindingSecret),
            salt: Data(), info: Data("alondra-feedback-binding-v1".utf8), outputByteCount: 32)
    }

    public func submit(_ request: ReportFeedbackRequest, identity: QuestionWorkIdentity) async -> QuestionWorkOutcome {
        guard identity.server == scope.server, identity.deviceID == scope.deviceID,
              identity.credentialRevision == scope.credentialRevision, identity.generation == scope.generation
        else { return .obsolete }
        let payload: Data
        do {
            payload = try JSONSerialization.data(withJSONObject: [
                "report": request.reportID, "session": request.sessionID,
                "text": request.text, "attachments": request.attachmentIDs
            ], options: [.sortedKeys])
        } catch { return .failed("Could not prepare report feedback") }
        let binding = Data(HMAC<SHA256>.authenticationCode(for: payload, using: bindingKey))
        let result = await transaction.submit(reportID: request.reportID, binding: binding, sendMessage: { id in
            _ = try await api.send(AlondraEndpoints.send(to: request.sessionID, text: request.text,
                attachments: request.attachmentIDs), operationID: id)
        }, markRead: { id in
            try await api.markRead(request.reportID, operationID: id)
        }, reconcile: { id in
            switch try await operations.recoveryStatus(requestID: id) {
            case .notDispatched: return .notDispatched
            case .unknown: return .unknown
            case .completed(let status, _):
                return (200..<300).contains(status) ? .confirmed : .rejected(status)
            }
        })
        switch result {
        case .confirmed: return .confirmed
        case .unknown: return .uncertain
        case .obsolete: return .obsolete
        case .rejected(let error): return .failed(error.localizedDescription)
        }
    }
}
