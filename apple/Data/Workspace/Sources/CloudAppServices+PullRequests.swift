import AlondraContracts
import AlondraDomain
import AlondraAPIClient
import AlondraCommands
import Foundation

public extension CloudAppServices {
    func pullRequestOperations() -> PullRequestOperations {
        PullRequestOperations(
            diff: { try await self.pullRequestSnapshot($0) },
            merge: { id, method in
                await self.performPRCommand(.mergePullRequest(id), prID: id) { api, operationID in
                    _ = try await api.send(AlondraEndpoints.mergePullRequest(id, method: method), operationID: operationID.uuidString)
                }
            },
            resolveConflicts: { id in
                await self.performPRCommand(.resolveConflicts(id), prID: id) { api, operationID in
                    _ = try await api.send(AlondraEndpoints.resolveConflicts(id), operationID: operationID.uuidString)
                }
            },
            setAside: { id in
                await self.performPRCommand(.dismissPullRequest(id), prID: id) { api, operationID in
                    _ = try await api.send(AlondraEndpoints.dismissPullRequest(id), operationID: operationID.uuidString)
                }
            },
            review: { id, text, attachments, kind, headSHA in
                if let headSHA, !Self.isCommitSHA(headSHA) {
                    return .rejected("Load the PR diff before sending a review")
                }
                var stored: [String] = []
                do {
                    for attachment in attachments {
                        if let id = attachment.storedID { stored.append(id) }
                        else { stored.append(try await self.uploadAttachment(attachment.data, name: attachment.name, transferID: attachment.id)) }
                    }
                } catch { return .rejected(error.localizedDescription) }
                let attachmentIDs = stored
                let command: CommandKind = kind == .ask ? .askOnPullRequest(id) : .requestChanges(id)
                return await self.performPRCommand(command, prID: id) { api, operationID in
                    var fields: [String: Any] = ["text": text, "attachments": attachmentIDs]
                    fields["expected_head_sha"] = headSHA
                    let body = try JSONSerialization.data(withJSONObject: fields)
                    let endpoint = Endpoint<Data>.data(.post,
                        path: ["v1", "prs", String(id), kind == .ask ? "ask" : "changes"],
                        body: body, contentType: "application/json")
                    _ = try await api.send(endpoint, operationID: operationID.uuidString)
                }
            }
        )
    }

    private func pullRequestSnapshot(_ id: UInt64) async throws -> PRDiffSnapshot {
        try checkActive()
        let data = try await api.send(Endpoint<Data>.data(.get, path: ["v1", "prs", String(id), "diff-snapshot"]))
        try checkActive()
        let snapshot = try JSONDecoder().decode(WirePRDiffSnapshot.self, from: data)
        guard Self.isCommitSHA(snapshot.headSHA) else { throw PRSnapshotError.invalidHead }
        return PRDiffSnapshot(diff: snapshot.diff, headSHA: snapshot.headSHA, truncated: snapshot.truncated)
    }

    private static func isCommitSHA(_ value: String) -> Bool {
        value.count == 40 && value.utf8.allSatisfy { (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }
    }

    private func performPRCommand(_ kind: CommandKind, prID: UInt64,
        operation: @escaping @Sendable (AlondraAPIClient, UUID) async throws -> Void) async -> PRMutationOutcome {
        switch await submitCommand(kind, itemID: "p\(prID)", operation: operation) {
        case .confirmed: return .confirmed
        case .obsolete: return .obsolete
        case .unknown(let command): return .unknown(command.id)
        case .rejected(let error):
            if case APIError.conflict(let kind, _, _) = error, kind == "pr_not_open" { return .closed }
            if case APIError.conflict(let kind, _, _) = error, kind == "head_changed" {
                return .rejected("The PR changed; reload the diff before sending your review")
            }
            return .rejected(error.localizedDescription)
        }
    }
}

private struct WirePRDiffSnapshot: Decodable {
    let diff: String
    let headSHA: String
    let truncated: Bool
    enum CodingKeys: String, CodingKey { case diff, headSHA = "head_sha", truncated }
}

private enum PRSnapshotError: Error { case invalidHead }
