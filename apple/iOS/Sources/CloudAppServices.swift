import AlondraAPIClient
import AlondraAuth
import AlondraCommands
import AlondraRelayOutbox
import AlondraRelaySession
import AlondraServicePairing
import AlondraShared
import AlondraSync
import AlondraTriageFeature
import CryptoKit
import Foundation

/// One approved service identity. Scene stores own presentation; this actor owns I/O.
actor CloudAppServices {
    let api: AlondraAPIClient
    let sync: SyncSession
    private let operations: RelaySession
    private let gate: CommandGate
    private let scope: CommandScope
    private var active = true

    init(approved: ApprovedServicePairing, auth: AuthSession) throws {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: approved.signingPrivateKey)
        let agreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: approved.agreementPrivateKey)
        let revision = SHA256.hash(data: signing.publicKey.rawRepresentation + Data(approved.macSigningKey.utf8)
            + Data(approved.ownerSub.utf8) + Data(String(approved.approvedAtMs).utf8))
            .map { String(format: "%02x", $0) }.joined()
        let directory = CommandJournalLocation.applicationSupport(bundleID: "com.bitomule.alondra")
            .appendingPathComponent("service-" + revision, isDirectory: true)
        let scope = CommandScope(server: approved.relayURL, deviceID: approved.deviceID,
                                 credentialRevision: revision, generation: 0)
        self.scope = scope
        operations = try RelaySession(journalFile: directory.appendingPathComponent("outbox.json"),
            scope: RelayOutboxScope(relay: approved.relayURL, macID: approved.macID,
                                   deviceID: approved.deviceID, credentialRevision: revision),
            bearer: { try await auth.bearerToken() }, signing: signing, agreement: agreement,
            macSigning: approved.macSigningPublicKey(),
            macAgreement: approved.macAgreementPublicKey())
        api = AlondraAPIClient(serviceURL: approved.relayURL, deviceID: approved.deviceID, operations: operations)
        sync = SyncSession(transport: api,
            identity: SyncIdentity(server: approved.relayURL, deviceID: approved.deviceID, credentialRevision: revision))
        gate = CommandGate(scope: scope, journal: FileCommandJournal(directory: directory.appendingPathComponent("commands")))
    }

    func deactivate() async {
        active = false
        await operations.deactivate()
        await sync.stop()
        await gate.use(CommandScope(server: scope.server, deviceID: scope.deviceID,
                                    credentialRevision: scope.credentialRevision, generation: scope.generation + 1))
    }

    func triageService() -> TriageService {
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
                    continuation.yield(TriageInboxUpdate(identity: triageIdentity, overview: overview))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private var triageIdentity: TriageIdentity {
        TriageIdentity(server: scope.server, deviceID: scope.deviceID,
                       credentialRevision: scope.credentialRevision, generation: scope.generation)
    }

    private func mutate(_ kind: CommandKind, itemID: String, identity: TriageIdentity) async -> TriageMutationOutcome {
        guard active, identity == triageIdentity else { return .obsolete }
        let api = api
        let result = await gate.submitIdentified(kind, itemID: itemID, in: scope,
            isUnknown: { error in
                if case ServiceAPIError.outcomeUnknown = error { return true }
                return error is CancellationError || (error as? URLError) != nil
            }) { operationID in
                switch kind {
                case .markRead(let id): try await api.markRead(id, operationID: operationID.uuidString)
                case .dismissPullRequest(let id): try await api.dismissPullRequest(id, operationID: operationID.uuidString)
                default: throw ServiceAPIError.invalidEndpoint
                }
            }
        guard active else { return .obsolete }
        switch result {
        case .confirmed: return .confirmed
        case .unknown: return .unknown
        case .obsolete: return .obsolete
        case .rejected(let error): return .rejected(error.localizedDescription)
        }
    }
}
