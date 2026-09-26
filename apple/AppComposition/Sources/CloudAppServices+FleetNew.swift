import AlondraAPIClient
import AlondraCommands
import AlondraFleetFeature
import AlondraNewWorkFeature
import AlondraShared
import Foundation

extension CloudAppServices {
    public func searchConversations(_ query: String) async throws -> SearchResult {
        try await workspaceDecode(SearchResult.self, endpoint: AlondraEndpoints.search(query))
    }

    public func nodeOperations() -> NodeOperations {
        NodeOperations(send: { session, message in
            var stored: [String] = []
            do {
                for file in message.files {
                    if let id = file.storedID { stored.append(id) }
                    else { stored.append(try await self.uploadAttachment(file.data, name: file.name, transferID: file.id)) }
                }
            } catch { return .rejected(error.localizedDescription) }
            let attachmentIDs = stored
            return await self.nodeCommand(.send(session), session: session) { api, id in
                _ = try await api.send(AlondraEndpoints.send(to: session, text: message.text, attachments: attachmentIDs), operationID: id.uuidString)
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
            return NewWorkSubmissionScope(upload: { attachment in
                if let id = attachment.storedID { return id }
                return try await self.uploadAttachment(attachment.data, name: attachment.name, transferID: attachment.id)
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
