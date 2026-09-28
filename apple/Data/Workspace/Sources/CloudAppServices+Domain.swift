import AlondraAPIClient
import AlondraContracts
import AlondraDomain
import AlondraSync
import Foundation

extension CloudAppServices {
    public func domainWorkspaceClient() -> DomainWorkspaceClient {
        DomainWorkspaceClient(
            observe: { await self.workspaceUpdates() },
            perform: { intent, workspace, taskActions, commandID in
                await self.perform(intent, in: workspace, taskActions: taskActions, commandID: commandID)
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
                        workspace: state.projection.map { DomainWorkspace($0, runtime: state.runtime) },
                        connection: TriageConnection(state.connection)))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// The command ID is the semantic idempotency key. An envelope whose outcome is unknown is
    /// kept and resent unchanged on retry; a new command ID always builds a new envelope.
    func perform(_ intent: DomainIntent, in workspace: DomainWorkspace,
                 taskActions: [DomainV2.AvailableAction], commandID: String) async -> DomainCommandResult {
        guard (try? checkActive()) != nil else { return .obsolete }
        if let pending = unresolved[commandID] { return await send(pending) }
        do {
            return await send(try DomainCommandFactory().envelope(for: intent, in: workspace, taskActions: taskActions,
                                                                  commandID: commandID))
        } catch {
            return .rejected(Self.rejection(error))
        }
    }

    /// For commands with no card on screen (New, Fleet): the envelope is built against the
    /// latest projection this session accepted, whose epoch and revision the server checks.
    func performOnLatest(_ intent: DomainIntent, commandID: String) async -> DomainCommandResult {
        guard (try? checkActive()) != nil else { return .obsolete }
        if let pending = unresolved[commandID] { return await send(pending) }
        guard let projection = await domainSync.state.projection else {
            return .rejected(DomainCommandRejection(code: DomainV2.ErrorCode.staleEpoch.rawValue,
                                                    message: "The Mac's data is not loaded yet"))
        }
        return await perform(intent, in: DomainWorkspace(projection), taskActions: [], commandID: commandID)
    }

    /// Each attempt is a new transport request; the command ID inside is what the server
    /// deduplicates.
    func send(_ envelope: DomainV2.CommandEnvelope) async -> DomainCommandResult {
        let commandID = envelope.commandId.rawValue
        do {
            let outcome = try await api.executeDomainCommand(envelope, transportID: UUID().uuidString)
            unresolved[commandID] = nil
            guard (try? checkActive()) != nil else { return .obsolete }
            return .recorded(outcome)
        } catch {
            guard (try? checkActive()) != nil else { return .obsolete }
            if neverApplied(error) {
                unresolved[commandID] = nil
                return .rejected(Self.rejection(error))
            }
            unresolved[commandID] = envelope
            return .uncertain
        }
    }

    static func rejection(_ error: any Error) -> DomainCommandRejection {
        switch error {
        case let error as DomainV2.DomainError:
            DomainCommandRejection(code: error.code, message: error.message)
        case let error as DomainIntentError:
            DomainCommandRejection(code: error.code, message: String(describing: error))
        default:
            DomainCommandRejection(code: nil, message: error.localizedDescription)
        }
    }
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
}
