import AlondraAPIClient
import AlondraCommands
import AlondraContracts
import AlondraDomain
import AlondraSync
import Foundation

extension CloudAppServices {
    public func domainWorkspaceClient() -> DomainWorkspaceClient {
        DomainWorkspaceClient(
            observe: { await self.workspaceUpdates() },
            perform: { intent, workspace, taskActions in
                await self.perform(intent, in: workspace, taskActions: taskActions)
            },
            taskActions: { id, epoch in
                try await self.checkActive()
                let actions = try await self.api.domainTaskActions(id, epoch: epoch).actions
                try await self.checkActive()
                return actions
            },
            pullRequestDiff: { id, head in
                try await self.checkActive()
                let diff = try await self.api.domainPullRequestDiff(id, expectedHead: head)
                try await self.checkActive()
                return diff
            },
            documentContent: { id, digest in
                try await self.checkActive()
                let text = try await self.api.domainDocumentContent(id, expectedDigest: digest)
                try await self.checkActive()
                return text
            })
    }

    func workspaceUpdates() -> AsyncStream<DomainWorkspaceUpdate> {
        AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            let task = Task {
                guard (try? checkActive()) != nil else { continuation.finish(); return }
                let identity = triageIdentity
                for await state in await domainSync.subscribe() {
                    guard (try? checkActive()) != nil, !Task.isCancelled else { break }
                    continuation.yield(DomainWorkspaceUpdate(identity: identity,
                        workspace: state.projection.map(DomainWorkspace.init),
                        connection: TriageConnection(state.connection)))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// Validates locally first so a refused intent never reserves a journal entry. The
    /// command ID is the journal's operation ID, so relay recovery and server idempotency
    /// both see the same identity.
    func perform(_ intent: DomainIntent, in workspace: DomainWorkspace,
                 taskActions: [DomainV2.AvailableAction]) async -> DomainCommandResult {
        let factory = DomainCommandFactory()
        do { _ = try factory.envelope(for: intent, in: workspace, taskActions: taskActions, commandID: "validation") }
        catch let error as DomainIntentError { return .rejected(Self.rejection(error)) }
        catch { return .rejected(DomainCommandRejection(code: nil, message: error.localizedDescription)) }
        let received = OutcomeBox()
        let result = await submitCommand(.domain(DomainCommandKind(intent)), itemID: intent.subject) { api, id in
            let envelope = try factory.envelope(for: intent, in: workspace, taskActions: taskActions,
                                                commandID: id.uuidString)
            await received.store(try await api.executeDomainCommand(envelope))
        }
        switch result {
        case .confirmed:
            guard let outcome = await received.value else { return .obsolete }
            return .recorded(outcome)
        case .unknown(let command): return .uncertain(command.id)
        case .obsolete: return .obsolete
        case .rejected(let error): return .rejected(Self.rejection(error))
        }
    }

    static func rejection(_ error: any Error) -> DomainCommandRejection {
        switch error {
        case let error as DomainV2.DomainError:
            DomainCommandRejection(code: error.code, message: error.message)
        case let error as DomainIntentError:
            DomainCommandRejection(code: error.code, message: error.message)
        default:
            DomainCommandRejection(code: nil, message: error.localizedDescription)
        }
    }
}

private actor OutcomeBox {
    var value: DomainV2.CommandOutcome?
    func store(_ outcome: DomainV2.CommandOutcome) { value = outcome }
}

extension TriageConnection {
    init(_ connection: SyncConnection) {
        switch connection {
        case .connecting: self = .connecting
        case .online: self = .online
        case .offline(let problem): self = .offline(problem)
        case .invalidConfiguration: self = .offline(.unexpected)
        }
    }
}

extension DomainIntentError {
    var code: String? {
        switch self {
        case .unavailable(let reason): reason
        case .notFound: DomainV2.ErrorCode.notFound.rawValue
        default: DomainV2.ErrorCode.invalidInput.rawValue
        }
    }

    var message: String {
        switch self {
        case .notFound: String(localized: "This item is no longer available.")
        case .unavailable: String(localized: "This action is not available any more. Refresh and try again.")
        case .optionRequired: String(localized: "Choose an option.")
        case .inputRequired: String(localized: "Fill in the required field.")
        case .inputInvalid: String(localized: "One of the fields is not valid.")
        case .feedbackRequired: String(localized: "Write your feedback first.")
        case .requestRequired: String(localized: "Describe the request first.")
        case .methodNotAllowed: String(localized: "This merge method is not allowed for this pull request.")
        }
    }
}
