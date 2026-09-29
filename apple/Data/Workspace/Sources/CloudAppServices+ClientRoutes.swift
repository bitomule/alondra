import AlondraAPIClient
import AlondraContracts
import AlondraDomain
import Foundation

/// Intents the v2 daemon serves as routes of its own rather than domain commands: text typed
/// into an agent's session, closing a session, and a request created with its context.
public enum ClientRoute: Equatable, Sendable {
    case message(DomainV2.AgentId, AgentMessage)
    case orchestratorMessage(AgentMessage)
    case close(DomainV2.AgentId, force: Bool)
    case request(NewRequestSubmission)

    /// Nil when the intent is a domain command.
    public static func route(for intent: DomainIntent, in workspace: DomainWorkspace,
                      newID: () -> String = { UUID().uuidString.lowercased() }) throws -> ClientRoute? {
        switch intent {
        case let .createRequest(body, project, options, issue, attachments):
            guard project != nil || !options.isEmpty || issue != nil || !attachments.isEmpty else { return nil }
            let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { throw DomainIntentError.requestRequired }
            return .request(NewRequestSubmission(requestID: newID(), text: text, project: project, options: options,
                                                 issue: issue, attachments: attachments))
        case let .messageAgent(agent, body, attachments):
            let message = AgentMessage(text: body, attachments: attachments)
            if workspace.runtime?.node(agent)?.isOrchestrator == true { return .orchestratorMessage(message) }
            return .message(agent, message)
        case let .closeAgent(agent, force):
            return .close(agent, force: force)
        case let .requestMoreContext(id, body, attachments):
            guard let interaction = workspace.interaction(id), let agent = author(interaction) ?? workspace.item(for: id)?.agentId else {
                throw DomainIntentError.notFound
            }
            let note = body.trimmingCharacters(in: .whitespacesAndNewlines)
            let missing = note.isEmpty ? "" : " What is missing: \(note)."
            let text = "I need more context before deciding.\(missing) Explain what is at stake, the options with their consequences and your recommendation, and ask again."
            return .message(agent, AgentMessage(kind: .moreContext, text: text, attachments: attachments, interactionId: id,
                                                taskId: task(of: interaction)))
        case let .replyToInformation(id, body, attachments):
            let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty || !attachments.isEmpty else { throw DomainIntentError.feedbackRequired }
            guard let interaction = workspace.interaction(id), let agent = author(interaction) ?? workspace.item(for: id)?.agentId else {
                throw DomainIntentError.notFound
            }
            return .message(agent, AgentMessage(kind: .reply, text: text, attachments: attachments, interactionId: id,
                                                taskId: task(of: interaction), markRead: true))
        case let .askAboutPullRequest(id, _, body, attachments):
            let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { throw DomainIntentError.feedbackRequired }
            let (agent, pr) = try owner(of: id, in: workspace)
            return .message(agent, AgentMessage(kind: .prQuestion, text: text, attachments: attachments, prId: id, taskId: pr.taskId))
        case let .requestConflictResolution(id, _):
            let (agent, pr) = try owner(of: id, in: workspace)
            let base = pr.base.isEmpty ? "its base branch" : pr.base
            let link = pr.webURL?.absoluteString ?? "#\(pr.number)"
            let text = "Resolve the conflicts of \(link) with \(base): bring \(base) into the branch, resolve them, run the tests and push."
            return .message(agent, AgentMessage(kind: .prChanges, text: text, prId: id, taskId: pr.taskId))
        default:
            return nil
        }
    }

    private static func author(_ interaction: DomainV2.Interaction) -> DomainV2.AgentId? {
        if case .agent(let agent) = interaction.author { return agent }
        return nil
    }

    private static func task(of interaction: DomainV2.Interaction) -> DomainV2.TaskId? {
        if case .task(let task) = interaction.scope { return task }
        return nil
    }

    private static func owner(of id: DomainV2.PrId, in workspace: DomainWorkspace) throws -> (DomainV2.AgentId, DomainV2.PullRequest) {
        guard let pr = workspace.pullRequest(id), let agent = workspace.task(pr.taskId)?.agentId else { throw DomainIntentError.notFound }
        return (agent, pr)
    }
}

extension CloudAppServices {
    /// Sends the route with the command ID as its operation ID, so the relay reconciles a lost
    /// response by status instead of sending it twice.
    func send(_ route: ClientRoute, in workspace: DomainWorkspace, commandID: String) async -> DomainCommandResult {
        let recorded = DomainV2.CommandOutcome(revision: workspace.revision, changed: false, effects: [], createdIds: [])
        do {
            switch route {
            case let .message(agent, message):
                let receipt = try await api.sendAgentMessage(agent, message, operationID: commandID)
                guard (try? checkActive()) != nil else { return .obsolete }
                return receipt.uncertain ? .uncertain : .recorded(recorded)
            case let .orchestratorMessage(message):
                let receipt = try await api.sendOrchestratorMessage(message, operationID: commandID)
                guard (try? checkActive()) != nil else { return .obsolete }
                return receipt.uncertain ? .uncertain : .recorded(recorded)
            case let .close(agent, force):
                try await api.closeAgent(agent, force: force, operationID: commandID)
            case let .request(submission):
                let outcome = try await api.createRequest(submission, operationID: commandID)
                guard (try? checkActive()) != nil else { return .obsolete }
                return .recorded(outcome)
            }
            guard (try? checkActive()) != nil else { return .obsolete }
            return .recorded(recorded)
        } catch {
            guard (try? checkActive()) != nil else { return .obsolete }
            return neverApplied(error) ? .rejected(Self.rejection(error)) : .uncertain
        }
    }
}
