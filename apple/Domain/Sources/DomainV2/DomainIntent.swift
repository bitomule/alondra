import AlondraContracts
import Foundation

/// What the user asked for, before it becomes a command. Each intent maps to exactly one
/// semantic command; reading never replies and authorizing a merge never merges.
public enum DomainIntent: Equatable, Sendable {
    case markRead(DomainV2.InteractionId)
    case answer(DomainV2.InteractionId, optionID: String, inputs: [String: DomainV2.InputValue], attachments: [DomainV2.AttachmentRef] = [])
    case acceptDelivery(DomainV2.InteractionId)
    case submitFeedback(DomainV2.InteractionId, body: String, attachments: [DomainV2.AttachmentRef] = [])
    case authorizeMerge(DomainV2.InteractionId, method: DomainV2.MergeMethod)
    case cancelTask(DomainV2.TaskId)
    case reopenTask(DomainV2.TaskId)
    case archiveTask(DomainV2.TaskId, archived: Bool)
    case createRequest(body: String, project: String? = nil, options: [String: OptionChoice] = [:],
                       issue: String? = nil, attachments: [DomainV2.AttachmentRef] = [])
    case closeRequest(DomainV2.RequestId)
    // Client contracts the daemon does not accept yet; see DomainV2.Command.
    case requestMoreContext(DomainV2.InteractionId, body: String, attachments: [DomainV2.AttachmentRef] = [])
    case replyToInformation(DomainV2.InteractionId, body: String, attachments: [DomainV2.AttachmentRef] = [])
    case askAboutPullRequest(DomainV2.PrId, head: String, body: String, attachments: [DomainV2.AttachmentRef] = [])
    case requestConflictResolution(DomainV2.PrId, head: String)
    case messageAgent(DomainV2.AgentId, body: String, attachments: [DomainV2.AttachmentRef] = [])
    case closeAgent(DomainV2.AgentId, force: Bool)
    case retryOrchestratorRun(String)
    case cancelOrchestratorRun(String)
    case acknowledgeOrchestratorRun(String)

    public var actionKind: DomainV2.ActionKind? {
        switch self {
        case .markRead: .markRead
        case .answer: .answerQuestion
        case .acceptDelivery: .acceptDelivery
        case .submitFeedback: .submitFeedback
        case .authorizeMerge: .authorizeMerge
        case .cancelTask: .cancelTask
        case .reopenTask: .reopenTask
        case .archiveTask(_, let archived): archived ? .archiveTask : .unarchiveTask
        default: nil
        }
    }

    /// The wire command kind, used to describe a pending or uncertain command.
    public var commandKind: String {
        if let actionKind { return actionKind == .unarchiveTask ? "archive_task" : actionKind.rawValue }
        return switch self {
        case .createRequest: "create_request"
        case .closeRequest: "close_request"
        case .requestMoreContext: "request_more_context"
        case .replyToInformation: "reply_to_information"
        case .askAboutPullRequest: "ask_about_pull_request"
        case .requestConflictResolution: "request_conflict_resolution"
        case .messageAgent: "message_agent"
        case .closeAgent: "close_agent"
        case .retryOrchestratorRun: "retry_orchestrator_run"
        case .cancelOrchestratorRun: "cancel_orchestrator_run"
        case .acknowledgeOrchestratorRun: "acknowledge_orchestrator_run"
        default: ""
        }
    }

    /// The durable subject a pending or uncertain command blocks until it resolves.
    public var subject: String {
        switch self {
        case .markRead(let id), .answer(let id, _, _, _), .acceptDelivery(let id), .submitFeedback(let id, _, _),
             .authorizeMerge(let id, _), .requestMoreContext(let id, _, _), .replyToInformation(let id, _, _):
            "interaction:" + id.rawValue
        case .cancelTask(let id), .reopenTask(let id), .archiveTask(let id, _): "task:" + id.rawValue
        case .createRequest: "request:new"
        case .closeRequest(let id): "request:" + id.rawValue
        case .askAboutPullRequest(let id, _, _, _), .requestConflictResolution(let id, _):
            "pr:" + id.rawValue
        case .messageAgent(let id, _, _), .closeAgent(let id, _): "agent:" + id.rawValue
        case .retryOrchestratorRun(let id), .cancelOrchestratorRun(let id), .acknowledgeOrchestratorRun(let id):
            "orchestrator:" + id
        }
    }
}

public enum DomainIntentError: Error, Equatable, Sendable {
    case notFound
    case unavailable(reason: String?)
    case optionRequired
    case inputRequired(String)
    case inputInvalid(String)
    case feedbackRequired
    case requestRequired
    case methodNotAllowed
}

/// Builds the one envelope for an intent from the projection the user was looking at.
/// The server re-validates everything; this refuses locally only what the server already
/// marked unavailable or what the form contract makes incomplete.
public struct DomainCommandFactory: Sendable {
    public init() {}

    public func envelope(
        for intent: DomainIntent, in workspace: DomainWorkspace,
        taskActions: [DomainV2.AvailableAction] = [],
        commandID: String, newID: () -> String = { UUID().uuidString.lowercased() }
    ) throws -> DomainV2.CommandEnvelope {
        let (command, revision) = try command(for: intent, in: workspace, taskActions: taskActions, newID: newID)
        return DomainV2.CommandEnvelope(protocolVersion: DomainV2.protocolVersion, dataEpoch: workspace.epoch,
                                        commandId: DomainV2.CommandId(rawValue: commandID),
                                        expectedRevision: revision, command: command)
    }

    private func command(
        for intent: DomainIntent, in workspace: DomainWorkspace,
        taskActions: [DomainV2.AvailableAction], newID: () -> String
    ) throws -> (DomainV2.Command, DomainV2.Revision) {
        switch intent {
        case .markRead(let id):
            let action = try available(.markRead, for: id, in: workspace)
            return (.markRead(interactionId: id), action.expectedRevision)
        case let .answer(id, optionID, inputs, attachments):
            let action = try available(.answerQuestion, for: id, in: workspace)
            guard let interaction = workspace.interaction(id) else { throw DomainIntentError.notFound }
            let options: [DomainV2.QuestionOption]
            switch interaction.payload {
            case .question(let values, _), .actionRequest(let values, _): options = values
            default: throw DomainIntentError.unavailable(reason: DomainV2.ErrorCode.invalidActionForKind.rawValue)
            }
            guard let option = options.first(where: { $0.id == optionID }) else { throw DomainIntentError.optionRequired }
            let checked = try AnswerForm(option: option).validated(inputs)
            return (.answerQuestion(attachments: attachments, interactionId: id, optionId: optionID, inputs: checked),
                    action.expectedRevision)
        case .acceptDelivery(let id):
            let action = try available(.acceptDelivery, for: id, in: workspace)
            return (.acceptDelivery(presentationId: try presentation(action, id), targets: try targets(id, in: workspace)),
                    action.expectedRevision)
        case let .submitFeedback(id, body, attachments):
            let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { throw DomainIntentError.feedbackRequired }
            let action = try available(.submitFeedback, for: id, in: workspace)
            return (.submitFeedback(attachments: attachments, presentationId: try presentation(action, id),
                                    targets: try targets(id, in: workspace), body: text), action.expectedRevision)
        case let .authorizeMerge(id, method):
            let action = try available(.authorizeMerge, for: id, in: workspace)
            guard let head = action.expectedHead,
                  let prID = try targets(id, in: workspace).lazy.compactMap({ target -> DomainV2.PrId? in
                      if case let .pullRequest(prId, targetHead) = target, targetHead == head { return prId }
                      return nil
                  }).first,
                  let pr = workspace.pullRequest(prID) else { throw DomainIntentError.notFound }
            guard pr.allowedMethods.contains(method) else { throw DomainIntentError.methodNotAllowed }
            return (.authorizeMerge(authorizationId: DomainV2.AuthorizationId(rawValue: newID()),
                                    presentationId: try presentation(action, id), prId: prID,
                                    expectedHead: head, method: method), action.expectedRevision)
        case .cancelTask(let id):
            return (.cancelTask(taskId: id), try taskAction(.cancelTask, id, workspace, taskActions).expectedRevision)
        case .reopenTask(let id):
            return (.reopenTask(taskId: id), try taskAction(.reopenTask, id, workspace, taskActions).expectedRevision)
        case let .archiveTask(id, archived):
            let kind: DomainV2.ActionKind = archived ? .archiveTask : .unarchiveTask
            return (.archiveTask(taskId: id, archived: archived),
                    try taskAction(kind, id, workspace, taskActions).expectedRevision)
        case let .createRequest(body, project, options, issue, attachments):
            let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { throw DomainIntentError.requestRequired }
            let id = DomainV2.RequestId(rawValue: newID())
            if project == nil, options.isEmpty, issue == nil, attachments.isEmpty {
                return (.createRequest(requestId: id, body: text), workspace.revision)
            }
            return (.createRequestWithContext(requestId: id, body: text, project: project, options: options,
                                              issue: issue, attachments: attachments), workspace.revision)
        case .closeRequest(let id):
            guard let request = workspace.request(id) else { throw DomainIntentError.notFound }
            guard !request.closed else { throw DomainIntentError.unavailable(reason: nil) }
            return (.closeRequest(requestId: id), workspace.revision)
        case let .requestMoreContext(id, body, attachments):
            guard workspace.interaction(id) != nil else { throw DomainIntentError.notFound }
            return (.requestMoreContext(attachments: attachments, interactionId: id,
                                        body: body.trimmingCharacters(in: .whitespacesAndNewlines)), workspace.revision)
        case let .replyToInformation(id, body, attachments):
            let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty || !attachments.isEmpty else { throw DomainIntentError.feedbackRequired }
            guard workspace.interaction(id) != nil else { throw DomainIntentError.notFound }
            return (.replyToInformation(attachments: attachments, interactionId: id, body: text), workspace.revision)
        case let .askAboutPullRequest(id, head, body, attachments):
            let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else { throw DomainIntentError.feedbackRequired }
            guard workspace.pullRequest(id) != nil else { throw DomainIntentError.notFound }
            return (.askAboutPullRequest(attachments: attachments, prId: id, head: head, body: text), workspace.revision)
        case let .requestConflictResolution(id, head):
            guard workspace.pullRequest(id) != nil else { throw DomainIntentError.notFound }
            return (.requestConflictResolution(prId: id, head: head), workspace.revision)
        case let .messageAgent(id, body, attachments):
            let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty || !attachments.isEmpty else { throw DomainIntentError.feedbackRequired }
            return (.messageAgent(attachments: attachments, agentId: id, body: text), workspace.revision)
        case let .closeAgent(id, force):
            return (.closeAgent(agentId: id, force: force), workspace.revision)
        case .retryOrchestratorRun(let id):
            return (.retryOrchestratorRun(runId: id), workspace.revision)
        case .cancelOrchestratorRun(let id):
            return (.cancelOrchestratorRun(runId: id), workspace.revision)
        case .acknowledgeOrchestratorRun(let id):
            return (.acknowledgeOrchestratorRun(runId: id), workspace.revision)
        }
    }

    private func available(_ kind: DomainV2.ActionKind, for id: DomainV2.InteractionId,
                           in workspace: DomainWorkspace) throws -> DomainV2.AvailableAction {
        guard let item = workspace.item(for: id) else { throw DomainIntentError.notFound }
        guard let action = item.availableActions.first(where: { $0.actionKind == kind }) else {
            throw DomainIntentError.unavailable(reason: DomainV2.ErrorCode.invalidActionForKind.rawValue)
        }
        guard action.enabled else { throw DomainIntentError.unavailable(reason: action.disabledReason) }
        return action
    }

    private func taskAction(_ kind: DomainV2.ActionKind, _ id: DomainV2.TaskId, _ workspace: DomainWorkspace,
                            _ actions: [DomainV2.AvailableAction]) throws -> DomainV2.AvailableAction {
        guard workspace.task(id) != nil else { throw DomainIntentError.notFound }
        guard let action = actions.first(where: { $0.actionKind == kind }) else {
            throw DomainIntentError.unavailable(reason: nil)
        }
        guard action.enabled else { throw DomainIntentError.unavailable(reason: action.disabledReason) }
        return action
    }

    private func presentation(_ action: DomainV2.AvailableAction, _ id: DomainV2.InteractionId) throws -> DomainV2.InteractionId {
        guard let presentation = action.presentationId, presentation == id else { throw DomainIntentError.notFound }
        return presentation
    }

    /// Acceptance and feedback refer to the exact versions presented, never to a newer head.
    private func targets(_ id: DomainV2.InteractionId, in workspace: DomainWorkspace) throws -> [DomainV2.DeliveryTarget] {
        guard case .delivery(let targets, _, _, _) = workspace.interaction(id)?.payload else {
            throw DomainIntentError.unavailable(reason: DomainV2.ErrorCode.invalidActionForKind.rawValue)
        }
        return targets
    }
}

/// The typed inputs an option declares. Nothing is inferred from option labels.
public struct AnswerForm: Equatable, Sendable {
    public let option: DomainV2.QuestionOption

    public init(option: DomainV2.QuestionOption) { self.option = option }

    public var requiresInput: Bool { option.inputs.contains(where: \.required) }

    public func validated(_ inputs: [String: DomainV2.InputValue]) throws -> [String: DomainV2.InputValue] {
        var checked: [String: DomainV2.InputValue] = [:]
        for (key, _) in inputs where !option.inputs.contains(where: { $0.id == key }) {
            throw DomainIntentError.inputInvalid(key)
        }
        for spec in option.inputs {
            switch (spec.kind, inputs[spec.id]) {
            case (_, nil):
                if spec.required { throw DomainIntentError.inputRequired(spec.id) }
            case (.text, .text(let value)?):
                let text = value.trimmingCharacters(in: .whitespacesAndNewlines)
                if text.isEmpty {
                    if spec.required { throw DomainIntentError.inputRequired(spec.id) }
                } else {
                    checked[spec.id] = .text(value: text)
                }
            case (.boolean, .boolean(let value)?): checked[spec.id] = .boolean(value: value)
            case (.integer, .integer(let value)?): checked[spec.id] = .integer(value: value)
            default: throw DomainIntentError.inputInvalid(spec.id)
            }
        }
        return checked
    }
}
