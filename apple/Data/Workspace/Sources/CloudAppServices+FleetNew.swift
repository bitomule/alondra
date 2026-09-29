import AlondraContracts
import AlondraDomain
import AlondraAPIClient
import AlondraShared
import Foundation

/// Fleet and New use the v2 client contracts: `message_agent`, `close_agent`, the orchestrator
/// decision routes, `create_request` with its context, and the catalog and activity reads.
extension CloudAppServices {
    public func searchConversations(_ query: String) async throws -> SearchResult {
        try checkActive()
        let result = try await api.domainSearch(query)
        try checkActive()
        return result
    }

    public func nodeOperations() -> NodeOperations {
        NodeOperations(send: { agent, message, commandID in
            var stored: [DomainV2.AttachmentRef] = []
            do {
                for file in message.files {
                    if let ref = file.stored { stored.append(ref) }
                    else { stored.append(try await self.uploadAttachment(file.data, name: file.name, transferID: file.id)) }
                }
            } catch { return .rejected(error.localizedDescription) }
            return await self.nodeCommand(.messageAgent(DomainV2.AgentId(rawValue: agent), body: message.text, attachments: stored), commandID)
        }, close: { agent, commandID in
            await self.nodeCommand(.closeAgent(DomainV2.AgentId(rawValue: agent), force: false), commandID)
        }, threads: { agent in
            try await self.checkActive()
            let threads = try await self.api.domainAgentActivity(DomainV2.AgentId(rawValue: agent))
            try await self.checkActive()
            return threads
        }, retryOrchestrator: { run, commandID in
            await self.orchestratorDecision(.retry, run: run, operationID: commandID)
        }, cancelOrchestrator: { run, commandID in
            await self.orchestratorDecision(.cancel, run: run, operationID: commandID)
        }, acknowledgeOrchestrator: { run, commandID in
            await self.orchestratorDecision(.acknowledge, run: run, operationID: commandID)
        })
    }

    public func newWorkOperations() -> NewWorkOperations {
        NewWorkOperations(projects: {
            try await self.checkActive()
            return try await self.api.domainProjects()
        }, options: { project in
            try await self.checkActive()
            return try await self.api.domainOptions(project: project)
        }, issues: { refresh in
            try await self.checkActive()
            return try await self.api.domainIssues(refresh: refresh)
        }, captureSubmissionScope: {
            guard (try? await self.checkActive()) != nil else { return nil }
            return NewWorkSubmissionScope(upload: { attachment in
                if let stored = attachment.stored { return stored }
                return try await self.uploadAttachment(attachment.data, name: attachment.name, transferID: attachment.id)
            }, isCurrent: {
                (try? await self.checkActive()) != nil
            }, create: { body, commandID in
                let intent = DomainIntent.createRequest(body: body.text, project: body.project, options: body.options,
                                                        issue: body.issue, attachments: body.attachments)
                switch await self.performOnLatest(intent, commandID: commandID) {
                case .recorded: return .confirmed
                case .uncertain: return .uncertain(String(localized: "We couldn’t confirm the request. Your draft is kept; sending it again won’t create it twice."))
                case .obsolete: return .cancelled
                case .rejected(let rejection): return .failed(rejection.localizedMessage)
                }
            })
        })
    }

    /// The daemon applies the decision to the execution named by `run`; a conflict means no such
    /// execution waits any more.
    private func orchestratorDecision(_ decision: OrchestratorDecision, run: String, operationID: String) async -> NodeMutationOutcome {
        guard (try? checkActive()) != nil else { return .obsolete }
        do {
            try await api.decideOrchestratorRun(decision, runID: run, operationID: operationID)
            return (try? checkActive()) != nil ? .confirmed : .obsolete
        } catch {
            guard (try? checkActive()) != nil else { return .obsolete }
            return neverApplied(error) ? .rejected(Self.rejection(error).localizedMessage) : .unknown
        }
    }

    private func nodeCommand(_ intent: DomainIntent, _ commandID: String) async -> NodeMutationOutcome {
        switch await performOnLatest(intent, commandID: commandID) {
        case .recorded: .confirmed
        case .uncertain: .unknown
        case .obsolete: .obsolete
        case .rejected(let rejection): .rejected(rejection.localizedMessage)
        }
    }
}
