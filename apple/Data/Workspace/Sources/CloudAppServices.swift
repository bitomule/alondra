import AlondraContracts
import AlondraDomain
import AlondraAPIClient
import AlondraAttachmentStorage
import AlondraAuth
import AlondraCommands
import AlondraLoopbackSession
import AlondraRecovery
import AlondraRelayOutbox
import AlondraRelaySession
import AlondraServicePairing
import AlondraSync
import CryptoKit
import Foundation

/// One approved service identity. Scene stores own presentation; this actor owns I/O.
public actor CloudAppServices {
    public let api: AlondraAPIClient
    public let sync: SyncSession
    public let domainSync: DomainSyncSession
    private let operations: any WorkspaceOperations
    private let gate: CommandGate
    private let deliveryStatus: DeliveryStatusService
    private let commandRecovery: CommandRecovery
    private let reportFeedback: ReportFeedbackCoordinator
    let scope: CommandScope
    let macID: String
    var deliveryJournal: DeliveryJournal?
    let attachmentStorage = AttachmentDownloadStorage()
    private var active = true
    private var feedbackRecoveryTask: Task<Void, Never>?

    public init(approved: ApprovedServicePairing, auth: AuthSession, storageDirectory: URL) throws {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: approved.signingPrivateKey)
        let agreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: approved.agreementPrivateKey)
        let revision = SHA256.hash(data: signing.publicKey.rawRepresentation + Data(approved.macSigningKey.utf8)
            + Data(approved.ownerSub.utf8) + Data(String(approved.approvedAtMs).utf8))
            .map { String(format: "%02x", $0) }.joined()
        let directory = storageDirectory
            .appendingPathComponent("service-" + revision, isDirectory: true)
        let relay = try RelaySession(journalFile: directory.appendingPathComponent("outbox.json"),
            scope: RelayOutboxScope(relay: approved.relayURL, macID: approved.macID,
                                   deviceID: approved.deviceID, credentialRevision: revision),
            bearer: { try await auth.bearerToken() }, cachedBearer: { try await auth.cachedBearerLease() }, signing: signing, agreement: agreement,
            macSigning: approved.macSigningPublicKey(),
            macAgreement: approved.macAgreementPublicKey())
        self.init(operations: relay, server: approved.relayURL, deviceID: approved.deviceID, macID: approved.macID,
                  revision: revision, directory: directory, bindingSecret: approved.signingPrivateKey)
    }

    /// The daemon on this Mac, reached over loopback. No account, relay or pairing QR involved.
    public static func local(_ session: LoopbackSession, identity: LoopbackIdentityStore,
                             storageDirectory: URL) async throws -> CloudAppServices {
        let revision = try await identity.credentialRevision()
        return CloudAppServices(operations: session, server: session.base, deviceID: Self.localDeviceID,
            macID: Self.localMacID, revision: revision,
            directory: storageDirectory.appendingPathComponent("local-" + revision, isDirectory: true),
            bindingSecret: try await identity.bindingSecret())
    }

    public static let localDeviceID = "mac"
    public static let localMacID = "local"

    public nonisolated var isLocal: Bool { operations is LoopbackSession }

    private init(operations: any WorkspaceOperations, server: URL, deviceID: String, macID: String,
                 revision: String, directory: URL, bindingSecret: Data) {
        let scope = CommandScope(server: server, deviceID: deviceID, credentialRevision: revision, generation: 0)
        let identity = SyncIdentity(server: server, deviceID: deviceID, credentialRevision: revision)
        self.scope = scope
        self.macID = macID
        self.operations = operations
        api = AlondraAPIClient(serviceURL: server, deviceID: deviceID, operations: operations)
        let local = operations is LoopbackSession
        let classify: @Sendable (any Error) -> ConnectionProblem = { error in
            ConnectionClassifier.problem(for: error, local: local, networkOffline: NetworkReachability.shared.isOffline)
        }
        sync = SyncSession(transport: api, identity: identity, classify: classify)
        domainSync = DomainSyncSession(transport: api, identity: identity, classify: classify)
        gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory.appendingPathComponent("commands")))
        reportFeedback = ReportFeedbackCoordinator(directory: directory.appendingPathComponent("feedback"), gate: gate,
            scope: scope, api: api, recoveryStatus: { try await operations.recoveryStatus(requestID: $0) },
            bindingSecret: bindingSecret)
        let feedbackCoordinator = reportFeedback
        deliveryStatus = DeliveryStatusService(gate: gate, scope: scope,
            storage: .file(directory.appendingPathComponent("delivery-warnings.json")),
            feedback: { await feedbackCoordinator.recoveryEvents() })
        commandRecovery = CommandRecovery(gate: gate, scope: scope,
            file: directory.appendingPathComponent("command-resolutions-v2.json"), response: { id in
                switch try await operations.latestOutcome(requestID: id.uuidString) {
                case .completed(let status, let body): return CommandResponse(status: status, body: body)
                case .unknown: return nil
                }
            })
    }

    public func deactivate() async {
        active = false
        await deliveryJournal?.stop()
        await attachmentStorage.deactivate()
        feedbackRecoveryTask?.cancel()
        feedbackRecoveryTask = nil
        await commandRecovery.stop()
        await deliveryStatus.stop()
        await operations.deactivate()
        await sync.stop()
        await domainSync.stop()
        await gate.use(CommandScope(server: scope.server, deviceID: scope.deviceID,
                                    credentialRevision: scope.credentialRevision, generation: scope.generation + 1))
    }

    /// Retries now instead of waiting out the backoff: the network came back or the app returned
    /// to the foreground.
    public func retryConnection() async {
        guard active else { return }
        await sync.retryNow()
    }

    public nonisolated func connectionProblem(for error: any Error) -> ConnectionProblem {
        ConnectionClassifier.problem(for: error, local: isLocal, networkOffline: NetworkReachability.shared.isOffline)
    }

    public func startRecovery() {
        guard active, feedbackRecoveryTask == nil else { return }
        feedbackRecoveryTask = Task {
            while active, !Task.isCancelled {
                // A failed status read leaves the durable command pending; the next
                // pass checks status again and never repeats the message.
                _ = try? await reportFeedback.recover()
                do { try await Task.sleep(for: .seconds(5)) } catch { break }
            }
        }
    }

    public func reportFeedbackRecoveryEvents() async -> AsyncStream<[ReportFeedbackRecoveryEvent]> {
        await reportFeedback.recoveryEvents()
    }

    public func retryReportFeedbackRead(_ id: UUID) async {
        guard active else { return }
        _ = try? await reportFeedback.recover(retryingReadFor: id)
    }

    public func deliveryStatuses() async -> AsyncStream<DeliveryStatus> {
        await deliveryStatus.observe()
    }

    public func acknowledgeDeliveryWarnings(_ observed: [UUID: DeliveryStatus.Phase], in expectedScope: CommandScope) async throws {
        guard active else { throw DeliveryStatusService.Failure.obsolete }
        try await deliveryStatus.acknowledge(observed, in: expectedScope)
    }

    public func commandResolutions() async -> AsyncStream<CommandResolution> {
        await commandRecovery.subscribe()
    }

    /// Opt-in domain-v2 data stream. Existing v1 consumers remain attached to `sync`.
    public func domainUpdates() async -> AsyncStream<DomainSyncState> {
        guard active else { return AsyncStream { $0.finish() } }
        return await domainSync.subscribe()
    }

    public func triageService() -> TriageService {
        TriageService(observe: { await self.inboxUpdates() }, mutate: { kind, itemID, identity in
            await self.mutate(kind, itemID: itemID, identity: identity)
        })
    }

    private func inboxUpdates() -> AsyncStream<TriageInboxUpdate> {
        AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            let task = Task {
                guard active else { continuation.finish(); return }
                let updates = await sync.subscribe()
                let decoder = JSONDecoder()
                decoder.keyDecodingStrategy = .convertFromSnakeCase
                for await update in updates {
                    guard active, !Task.isCancelled else { break }
                    let overview = update.snapshot.flatMap { try? decoder.decode(Overview.self, from: $0.body) }
                    let connection: TriageConnection
                    switch update.connection {
                    case .connecting: connection = .connecting
                    case .online: connection = .online
                    case .offline(let problem): connection = .offline(problem)
                    case .invalidConfiguration: connection = .offline(.unexpected)
                    }
                    continuation.yield(TriageInboxUpdate(identity: triageIdentity, overview: overview, connection: connection))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    var triageIdentity: TriageIdentity {
        TriageIdentity(server: scope.server, deviceID: scope.deviceID,
                       credentialRevision: scope.credentialRevision, generation: scope.generation)
    }

    public func submitReportFeedback(_ request: ReportFeedbackRequest, identity: QuestionWorkIdentity) async -> QuestionWorkOutcome {
        guard active else { return .obsolete }
        let result = await reportFeedback.submit(request, identity: identity)
        return active ? result : .obsolete
    }

    public func documentText(_ path: String) async throws -> String {
        try checkActive()
        // Chunked so documents larger than one encrypted relay response still load.
        let bytes = try await api.downloadAttachment(path: path, downloadID: UUID())
        try checkActive()
        return String(decoding: bytes, as: UTF8.self)
    }

    public func checkActive() throws {
        guard active else { throw CancellationError() }
    }

    public func submitCommand(_ kind: CommandKind, itemID: String? = nil,
        operation: @escaping @Sendable (AlondraAPIClient, UUID) async throws -> Void) async -> CommandResult {
        guard active else { return .obsolete }
        let api = api
        let witness = CommandDispatchWitness()
        let local = isLocal
        let result = await gate.submitIdentified(kind, itemID: itemID, in: scope,
            isUnknown: { error in
                if local, LoopbackSession.neverSent(error) { return false }
                if let error = error as? ServiceAPIError { return error == .outcomeUnknown }
                if error is APIError { return false }
                if error is DomainV2.DomainError || error is DomainIntentError { return false }
                if error is DeliveryHandoffUnconfirmed { return false }
                if case RelaySessionError.invalidRequest = error { return false }
                if case RelaySessionError.operationIdentityConflict = error { return false }
                return true
            }) { id in
                await witness.markInvoked()
                try await operation(api, id)
            }
        guard active else { return .obsolete }
        if case .unknown = result, !(await witness.invoked) {
            return .rejected(PreviousCommandStillPending())
        }
        return result
    }

    private func mutate(_ kind: CommandKind, itemID: String, identity: TriageIdentity) async -> TriageMutationOutcome {
        guard active, identity == triageIdentity else { return .obsolete }
        let result = await submitCommand(kind, itemID: itemID) { api, operationID in
                switch kind {
                case .markRead(let id): try await api.markRead(id, operationID: operationID.uuidString)
                case .dismissPullRequest(let id): try await api.dismissPullRequest(id, operationID: operationID.uuidString)
                default: throw ServiceAPIError.invalidEndpoint
                }
            }
        guard active else { return .obsolete }
        switch result {
        case .confirmed: return .confirmed
        case .unknown(let command): return .unknownCommand(command.id)
        case .obsolete: return .obsolete
        case .rejected(let error): return .rejected(error.localizedDescription)
        }
    }
}
