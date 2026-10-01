import AlondraTransport
import AlondraContract
import AlondraNetworkReachability
import AlondraNotifications
import AlondraPairing
import AlondraFeed
import CryptoKit
import Foundation

private protocol FeedOperations: OperationAPI {
    func deactivate() async
}

extension RelaySession: FeedOperations {}
extension LoopbackSession: FeedOperations {}

/// One approved service identity. Shipping presentation reads only server Views.
public actor Connection {
    public let api: AlondraAPIClient
    public let viewsFeed: FeedSession
    public let viewActionSender: ViewActionSender
    private let operations: any FeedOperations
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
            macSigning: approved.macSigningPublicKey(), macAgreement: approved.macAgreementPublicKey())
        self.init(operations: relay, server: approved.relayURL, deviceID: approved.deviceID)
    }

    public static func local(_ session: LoopbackSession, identity: LoopbackIdentityStore) async throws -> Connection {
        _ = try await identity.credentialRevision()
        return Connection(operations: session, server: session.base, deviceID: "mac")
    }

    public nonisolated var isLocal: Bool { operations is LoopbackSession }

    private init(operations: any FeedOperations, server: URL, deviceID: String) {
        self.operations = operations
        api = AlondraAPIClient(serviceURL: server, deviceID: deviceID, operations: operations)
        #if os(macOS)
        let client: ViewClient = .mac
        #else
        let client: ViewClient = .ios
        #endif
        let preferred = Locale.preferredLanguages.first?.split(separator: "-").first.map(String.init) ?? "en"
        viewsFeed = FeedSession(transport: api, client: client, lang: preferred == "es" ? "es" : "en")
        viewActionSender = ViewActionSender(transport: api)
    }

    public func deactivate() async {
        active = false
        await viewsFeed.stop()
        await operations.deactivate()
    }

    public func retryViewsConnection() async {
        guard active else { return }
        await viewsFeed.reconnect()
    }

    public func checkActive() throws {
        guard active else { throw CancellationError() }
    }

    public func checkSettingsConnection() async throws {
        try checkActive()
        _ = try await api.send(.data(.get, path: ["capabilities"]))
        try checkActive()
    }

    public nonisolated func connectionProblem(for error: any Error) -> ConnectionProblem {
        ConnectionClassifier.problem(for: error, local: isLocal, networkOffline: NetworkReachability.shared.isOffline)
    }

    public func uploadAttachment(_ data: Data, name: String, transferID: UUID,
                                 progress: @escaping @Sendable (Double) -> Void = { _ in }) async throws -> ViewUploadedAttachment {
        try checkActive()
        let stored = try await api.uploadViewAttachment(data, name: name, transferID: transferID, progress: progress)
        try checkActive()
        return stored
    }

    public func notificationClient(identity: String, binding: NotificationPayload.Binding) -> NotificationClient {
        NotificationClient(identity: identity, binding: binding, register: { token, environment, quietStart, quietEnd, revision in
            try await self.checkActive()
            try await self.api.registerServicePush(token: token, environment: environment, binding: binding.value,
                                                   preferencesRevision: revision, quietStart: quietStart, quietEnd: quietEnd)
            try await self.checkActive()
        }, submit: { request in
            do {
                try await self.checkActive()
                let response = try await self.viewActionSender.send(request)
                try await self.checkActive()
                if response.outcome == .noLongerAvailable { return .noLongerPending }
                return .sent(response)
            } catch {
                guard (try? await self.checkActive()) != nil else { return .failed(String(localized: "Service is no longer active")) }
                return .uncertain
            }
        })
    }
}
