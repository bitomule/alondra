import AlondraAPIClient
import AlondraContracts
import AlondraDomain
import AlondraAttachmentStorage
import AlondraAuth
import AlondraLoopbackSession
import AlondraRecovery
import AlondraRelayOutbox
import AlondraRelaySession
import AlondraServicePairing
import AlondraSync
import AlondraViews
import AlondraViewsData
import CryptoKit
import Foundation

/// One approved service identity. Scene stores own presentation; this actor owns I/O.
public actor CloudAppServices {
    public let api: AlondraAPIClient
    public let domainSync: DomainSyncSession
    public let viewsFeed: ViewsFeedSession
    public let viewActionSender: ViewActionSender
    private let operations: any WorkspaceOperations
    let scope: CommandScope
    let attachmentStorage = AttachmentDownloadStorage()
    /// Envelopes whose outcome is unknown, by command ID. A retry resends the same envelope,
    /// so the server returns the first outcome instead of applying the command twice.
    var unresolved: [String: DomainV2.CommandEnvelope] = [:]
    private var active = true

    public init(approved: ApprovedServicePairing, auth: AuthSession) throws {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: approved.signingPrivateKey)
        let agreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: approved.agreementPrivateKey)
        let revision = SHA256.hash(data: signing.publicKey.rawRepresentation + Data(approved.macSigningKey.utf8)
            + Data(approved.ownerSub.utf8) + Data(String(approved.approvedAtMs).utf8))
            .map { String(format: "%02x", $0) }.joined()
        let relay = try RelaySession(
            scope: RelayOutboxScope(relay: approved.relayURL, macID: approved.macID,
                                   deviceID: approved.deviceID, credentialRevision: revision),
            bearer: { try await auth.bearerToken() }, signing: signing, agreement: agreement,
            macSigning: approved.macSigningPublicKey(),
            macAgreement: approved.macAgreementPublicKey())
        self.init(operations: relay, server: approved.relayURL, deviceID: approved.deviceID,
                  revision: revision)
    }

    /// The daemon on this Mac, reached over loopback. No account, relay or pairing QR involved.
    public static func local(_ session: LoopbackSession, identity: LoopbackIdentityStore) async throws -> CloudAppServices {
        let revision = try await identity.credentialRevision()
        return CloudAppServices(operations: session, server: session.base, deviceID: Self.localDeviceID,
            revision: revision)
    }

    public static let localDeviceID = "mac"
    public nonisolated var isLocal: Bool { operations is LoopbackSession }

    private init(operations: any WorkspaceOperations, server: URL, deviceID: String, revision: String) {
        let scope = CommandScope(server: server, deviceID: deviceID, credentialRevision: revision, generation: 0)
        let identity = SyncIdentity(server: server, deviceID: deviceID, credentialRevision: revision)
        self.scope = scope
        self.operations = operations
        let local = operations is LoopbackSession
        api = AlondraAPIClient(serviceURL: server, deviceID: deviceID, operations: operations)
        #if os(macOS)
        let client: ViewClient = .mac
        #else
        let client: ViewClient = .ios
        #endif
        let preferred = Locale.preferredLanguages.first?.split(separator: "-").first.map(String.init) ?? "en"
        let lang = preferred == "es" ? "es" : "en"
        viewsFeed = ViewsFeedSession(transport: api, client: client, lang: lang)
        viewActionSender = ViewActionSender(transport: api)
        let classify: @Sendable (any Error) -> ConnectionProblem = { error in
            ConnectionClassifier.problem(for: error, local: local, networkOffline: NetworkReachability.shared.isOffline)
        }
        domainSync = DomainSyncSession(transport: api, identity: identity, classify: classify)
    }

    public func deactivate() async {
        active = false
        unresolved = [:]
        await attachmentStorage.deactivate()
        await operations.deactivate()
        await domainSync.stop()
        await viewsFeed.stop()
    }

    /// Retries now instead of waiting out the backoff: the network came back or the app returned
    /// to the foreground.
    public func retryConnection() async {
        guard active else { return }
        await viewsFeed.reconnect()
        await domainSync.retryNow()
    }

    /// Views-only shipping scenes reconnect their feed without waking the legacy poller.
    public func retryViewsConnection() async {
        guard active else { return }
        await viewsFeed.reconnect()
    }

    public nonisolated func connectionProblem(for error: any Error) -> ConnectionProblem {
        ConnectionClassifier.problem(for: error, local: isLocal, networkOffline: NetworkReachability.shared.isOffline)
    }

    var triageIdentity: TriageIdentity {
        TriageIdentity(server: scope.server, deviceID: scope.deviceID,
                       credentialRevision: scope.credentialRevision, generation: scope.generation)
    }

    public func checkActive() throws {
        guard active else { throw CancellationError() }
    }

    /// Only errors that prove nothing reached the Mac are rejections; anything else leaves the
    /// outcome unknown and the caller keeps its draft for a retry.
    nonisolated func neverApplied(_ error: any Error) -> Bool {
        if isLocal, LoopbackSession.neverSent(error) { return true }
        if let error = error as? ServiceAPIError { return error != .outcomeUnknown }
        if error is APIError || error is DomainV2.DomainError || error is DomainIntentError { return true }
        if case RelaySessionError.invalidRequest = error { return true }
        if case RelaySessionError.operationIdentityConflict = error { return true }
        return false
    }
}
