import AlondraContracts
import Foundation

extension AlondraAPIClient {
    /// The command ID is the semantic idempotency key: a retry resends the whole envelope
    /// unchanged. `transportID` identifies one transmission and is new for every attempt.
    public func executeDomainCommand(_ envelope: DomainV2.CommandEnvelope,
                                     transportID: String = UUID().uuidString) async throws -> DomainV2.CommandOutcome {
        guard envelope.protocolVersion == DomainV2.protocolVersion else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Unsupported domain protocol", currentRevision: nil, subject: nil)
        }
        guard envelope.expectedRevision <= DomainV2.maximumRevision,
              !envelope.commandId.rawValue.isEmpty, !envelope.dataEpoch.rawValue.isEmpty else {
            throw DomainV2.DomainError(code: "invalid_input", message: "Invalid command identity or revision", currentRevision: nil, subject: nil)
        }
        let resource = try resourceCommand(envelope)
        let endpoint = Endpoint<DomainV2.CommandOutcome>(
            resource.method, path: resource.path,
            body: try JSONSerialization.data(withJSONObject: resource.body), contentType: "application/json",
            requiresJSONObjectResponse: true
        ) { try DomainV2.decode(DomainV2.CommandOutcome.self, from: $0) }
        return try await send(endpoint, operationID: transportID)
    }

    private func resourceCommand(_ envelope: DomainV2.CommandEnvelope) throws -> (method: HTTPMethod, path: [String], body: [String: Any]) {
        var root = try JSONSerialization.jsonObject(with: DomainV2.encode(envelope)) as? [String: Any]
        guard var root, var command = root["command"] as? [String: Any],
              let kind = command["kind"] as? String else { throw unsupportedCommand() }
        func id(_ key: String) throws -> String {
            guard let value = command[key] as? String, !value.isEmpty else { throw unsupportedCommand() }
            command.removeValue(forKey: key)
            return value
        }
        var method: HTTPMethod = .post
        var path: [String]
        switch kind {
        case "create_request", "create_request_with_context": path = ["requests"]
        case "close_request": path = ["requests", try id("request_id"), "close"]
        case "create_task": path = ["requests", try id("request_id"), "tasks"]
        case "retitle_task": method = .patch; path = ["tasks", try id("task_id")]
        case "assign_task": method = .put; path = ["tasks", try id("task_id"), "assignee"]
        case "set_dependencies": method = .put; path = ["tasks", try id("task_id"), "dependencies"]
        case "start_task": path = ["tasks", try id("task_id"), "start"]
        case "create_document": path = ["tasks", try id("task_id"), "documents"]
        case "revise_document": method = .put; path = ["tasks", try id("task_id"), "documents", try id("document_id")]
        case "link_task_document": path = ["tasks", try id("task_id"), "document-links"]
        case "decide_document_review": method = .put; path = ["tasks", try id("task_id"), "documents", try id("document_id"), "review"]
        case "register_pr": path = ["tasks", try id("task_id"), "pull-requests"]
        case "present_result": path = ["tasks", try id("task_id"), "results"]
        case "publish_stack_readiness": path = ["tasks", try id("task_id"), "stack-readiness"]
        case "complete_task": path = ["tasks", try id("task_id"), "completion"]
        case "cancel_task": path = ["tasks", try id("task_id"), "cancellation"]
        case "reopen_task": path = ["tasks", try id("task_id"), "reopening"]
        case "archive_task": path = ["tasks", try id("task_id"), "archival"]
        case "ask_question": path = try scopedPath("questions", command: &command)
        case "request_action": path = try scopedPath("action-requests", command: &command)
        case "publish_information": path = try scopedPath("messages", command: &command)
        case "answer_question": path = ["interactions", try id("interaction_id"), "answers"]
        case "withdraw_question": path = ["interactions", try id("interaction_id"), "withdrawal"]
        case "link_interaction_subject": method = .put; path = ["interactions", try id("interaction_id"), "subject"]
        case "mark_read": method = .put; path = ["interactions", try id("interaction_id"), "read-state"]
        case "submit_feedback": path = ["interactions", try id("presentation_id"), "feedback"]
        case "accept_delivery": path = ["interactions", try id("presentation_id"), "acceptance"]
        case "authorize_merge": path = ["interactions", try id("presentation_id"), "merge-authorizations"]
        case "claim_merge_authorization": path = ["merge-authorizations", try id("authorization_id"), "claim"]
        default: throw unsupportedCommand()
        }
        command.removeValue(forKey: "kind")
        root.removeValue(forKey: "command")
        command["data_epoch"] = root["data_epoch"]
        command["command_id"] = root["command_id"]
        command["expected_revision"] = root["expected_revision"]
        return (method, path, command)
    }

    private func scopedPath(_ resource: String, command: inout [String: Any]) throws -> [String] {
        guard let scope = command["scope"] as? [String: Any], scope["kind"] as? String == "task",
              let taskID = scope["task_id"] as? String, !taskID.isEmpty else { return [resource] }
        return ["tasks", taskID, resource]
    }

    private func unsupportedCommand() -> DomainV2.DomainError {
        DomainV2.DomainError(code: "invalid_input", message: "Unsupported resource command", currentRevision: nil, subject: nil)
    }
}

extension AlondraAPIClient {
    public func domainCapabilities() async throws -> DomainV2.Capabilities {
        let endpoint = Endpoint<DomainV2.Capabilities>(.get, path: ["capabilities"]) {
            try DomainV2.decode(DomainV2.Capabilities.self, from: $0)
        }
        let capabilities = try await send(endpoint)
        guard capabilities.protocolVersion == DomainV2.protocolVersion else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Unsupported domain protocol", currentRevision: capabilities.revision, subject: nil)
        }
        return capabilities
    }
}

extension AlondraAPIClient {
    public func domainChanges(epoch: DomainV2.DataEpoch, after revision: DomainV2.Revision, wait seconds: UInt = 50) async throws -> DomainV2.Changes {
        let endpoint = Endpoint<DomainV2.Changes>(.get, path: ["changes"],
            query: [("data_epoch", epoch.rawValue), ("after", String(revision)), ("wait", String(min(seconds, 50)))]) {
                try DomainV2.decode(DomainV2.Changes.self, from: $0)
            }
        let changes = try await send(endpoint)
        guard changes.protocolVersion == DomainV2.protocolVersion,
              changes.dataEpoch == epoch, changes.revision >= revision,
              changes.revision <= DomainV2.maximumRevision else {
            throw DomainV2.DomainError(code: "incompatible_protocol", message: "Unsupported domain changes", currentRevision: nil, subject: nil)
        }
        return changes
    }
}
