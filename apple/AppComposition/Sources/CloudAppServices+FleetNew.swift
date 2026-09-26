import AlondraAPIClient
import AlondraCommands
import AlondraFleetFeature
import AlondraNewWorkFeature
import AlondraShared
import Foundation

extension CloudAppServices {
    public func nodeOperations() -> NodeOperations {
        NodeOperations(send: { session, message in
            guard message.files.isEmpty else { return .rejected("File uploads are not available through the service yet. Your draft has been kept.") }
            return await self.nodeCommand(.send(session), session: session) { api, id in
                _ = try await api.send(AlondraEndpoints.send(to: session, text: message.text, attachments: []), operationID: id.uuidString)
            }
        }, close: { session in
            await self.nodeCommand(.closeNode(session), session: session) { api, id in
                _ = try await api.send(AlondraEndpoints.closeNode(session, force: false), operationID: id.uuidString)
            }
        }, threads: { session in
            try await self.workspaceDecode(NodeThreads.self, endpoint: AlondraEndpoints.nodeActivity(session)).activity
        })
    }

    public func newWorkOperations() -> NewWorkOperations {
        NewWorkOperations(projects: {
            try await self.workspaceDecode(ProjectList.self, endpoint: AlondraEndpoints.projects()).projects.map(\.name)
        }, options: { project in
            try await self.workspaceDecode(OptionsCatalog.self, endpoint: AlondraEndpoints.options(project: project))
        }, issues: { refresh in
            try await self.workspaceDecode(IssueList.self, endpoint: AlondraEndpoints.issues(refresh: refresh))
        }, captureSubmissionScope: {
            guard (try? await self.checkActive()) != nil else { return nil }
            return NewWorkSubmissionScope(upload: { _ in
                throw ServiceWorkspaceError.uploadUnavailable
            }, isCurrent: {
                (try? await self.checkActive()) != nil
            }, create: { body in
                switch await self.submitCommand(.newRequest) { api, id in
                    _ = try await api.createRequest(body, operationID: id.uuidString)
                } {
                case .confirmed: return .confirmed
                case .unknown(let command): return .uncertainCommand(command.id, "The request may have been created. Check Tasks before trying again; your draft is kept.")
                case .obsolete: return .cancelled
                case .rejected(let error): return .failed(error.localizedDescription)
                }
            })
        })
    }

    private func nodeCommand(_ kind: CommandKind, session: String,
        operation: @escaping @Sendable (AlondraAPIClient, UUID) async throws -> Void) async -> NodeMutationOutcome {
        switch await submitCommand(kind, itemID: "node:" + session, operation: operation) {
        case .confirmed: return .confirmed
        case .unknown(let command): return .unknown(command.id)
        case .obsolete: return .obsolete
        case .rejected(let error): return .rejected(error.localizedDescription)
        }
    }

    private func workspaceDecode<Value: Decodable & Sendable>(_ type: Value.Type, endpoint: Endpoint<Data>) async throws -> Value {
        try checkActive()
        let data = try await api.send(endpoint)
        try checkActive()
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(type, from: data)
    }
}

private enum ServiceWorkspaceError: LocalizedError {
    case uploadUnavailable
    var errorDescription: String? { "File uploads are not available through the service yet. Your draft has been kept." }
}
