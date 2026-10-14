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

extension RoutedSession: FeedOperations {}
extension LoopbackSession: FeedOperations {}

/// One approved service identity. Shipping presentation reads only server Views.
public actor Connection {
    public let api: AlondraAPIClient
    public let viewsFeed: FeedSession
    public let viewActionSender: ViewActionSender
    private let operations: any FeedOperations
    private let macName: String?
    /// The paired Mac's ID; nil for the Mac this app runs on.
    public nonisolated let macID: String?
    private var active = true

    /// A Mac paired on the local network talks only directly; one paired with an account also
    /// keeps the relay, and reaches it directly only once Local Network access is allowed.
    public init(approved: ApprovedServicePairing, auth: AuthSession, storageDirectory: URL? = nil,
                localNetworkAllowed: @escaping @Sendable () -> Bool = { LocalNetworkAccess.allowed }) async throws {
        let signing = try P256.Signing.PrivateKey(rawRepresentation: approved.signingPrivateKey)
        let agreement = try P256.KeyAgreement.PrivateKey(rawRepresentation: approved.agreementPrivateKey)
        let macSigning = try approved.macSigningPublicKey()
        let macAgreement = try approved.macAgreementPublicKey()
        var name = URLComponents()
        name.scheme = "alondra-direct"
        name.host = approved.macID
        guard let server = approved.relayURL ?? name.url else { throw PairingError.invalidIdentity }
        let binding = try JSONEncoder().encode([approved.macID, approved.deviceID, approved.ownerSub ?? "",
            signing.publicKey.rawRepresentation.base64EncodedString(), agreement.publicKey.rawRepresentation.base64EncodedString(),
            approved.macSigningKey, approved.macAgreementKey, String(approved.approvedAtMs)])
        let revision = SHA256.hash(data: binding).map { String(format: "%02x", $0) }.joined()
        let scope = RelayOutboxScope(relay: server, macID: approved.macID,
            deviceID: approved.deviceID, credentialRevision: revision)
        let journal = try storageDirectory.map { try EnvelopeJournal(scope: scope, directory: $0.appending(path: "operations")) }
        let intents = await Self.openIntents(in: storageDirectory, keyMaterial: approved.agreementPrivateKey,
            scope: ActionIntentScope(service: server.absoluteString + "/" + approved.macID,
                device: approved.deviceID, credentialRevision: revision))
        let http = DirectHTTP(transport: AlondraAPIClient.makeHTTPTransport())
        let router = DirectRouter(pinned: approved.pinnedIdentity, saved: approved.routes ?? [], http: http,
                                  network: { NetworkReachability.shared.signature })
        let direct = DirectSession(macID: approved.macID, deviceID: approved.deviceID, signing: signing,
            agreement: agreement, macSigning: macSigning, macAgreement: macAgreement, router: router, http: http, journal: journal)
        var relay: RelaySession?
        if approved.relayURL != nil, approved.ownerSub != nil {
            relay = try RelaySession(
                scope: scope,
                bearer: { try await auth.bearerToken() }, bearerRefusedForToken: { await auth.accessTokenRefused(ifMatches: $0) }, journal: journal,
                signing: signing, agreement: agreement,
                macSigning: macSigning, macAgreement: macAgreement)
        }
        let hasRelay = relay != nil
        let directAllowed: @Sendable () -> Bool = { !hasRelay || localNetworkAllowed() }
        let routed = RoutedSession(direct: direct, router: router, relay: relay, directAllowed: directAllowed)
        self.init(operations: routed, server: server, deviceID: approved.deviceID, macName: approved.name,
                  macID: approved.macID, intents: intents)
    }

    public static func local(_ session: LoopbackSession, identity: LoopbackIdentityStore,
                             storageDirectory: URL? = nil) async throws -> Connection {
        let revision = try await identity.credentialRevision()
        let material = try await identity.bindingSecret()
        let intents = await openIntents(in: storageDirectory, keyMaterial: material,
            scope: ActionIntentScope(service: session.base.absoluteString, device: "this-mac", credentialRevision: revision))
        return Connection(operations: session, server: session.base, deviceID: "mac", macName: nil,
                          macID: nil, intents: intents)
    }

    /// Saved answers are this device's own business: whatever their store does, the Mac is still reached.
    private static func openIntents(in storageDirectory: URL?, keyMaterial: Data, scope: ActionIntentScope) async -> OpenedActionIntents? {
        guard let storageDirectory else { return nil }
        return await ViewActionIntentStore.openSettingAsideUnreadable(directory: storageDirectory.appending(path: "action-intents"),
            keyMaterial: keyMaterial, scope: scope)
    }

    public nonisolated var isLocal: Bool { operations is LoopbackSession }

    /// A route only the daemon on this Mac answers. Any other connection refuses it.
    public func local(_ route: LoopbackLocalRoute) async throws -> Data {
        guard let loopback = operations as? LoopbackSession else { throw OperationError.invalidConfiguration }
        try checkActive()
        return try await loopback.local(route)
    }

    /// No relay behind it: a Mac paired on the local network.
    public nonisolated var isDirectOnly: Bool { (operations as? RoutedSession)?.hasRelay == false }

    /// The path the last answered request took: this Mac, the LAN, the tailnet or the relay.
    public func route() async -> ConnectionRoute? {
        if isLocal { return .thisMac }
        return await (operations as? RoutedSession)?.route
    }

    /// Asks the Mac where it answers directly and starts trying what it says. Nothing is saved
    /// from it: an address is kept only after a sealed answer through it.
    public func learnRoutes() async throws {
        guard let routed = operations as? RoutedSession else { return }
        try checkActive()
        let reply = try await api.send(.data(.get, path: ["routes"]))
        await routed.offer(try JSONDecoder().decode(DirectRoutesReply.self, from: reply).endpoints, from: .mac)
    }

    /// An address Bonjour found for this Mac on the current network: a hint, never saved as is.
    public func offerDiscovered(_ endpoint: DirectEndpoint) async {
        await (operations as? RoutedSession)?.offer([endpoint], from: .bonjour)
    }

    /// The addresses a sealed answer proved, the only ones the pairing keeps.
    public func provenEndpoints() async -> [DirectEndpoint] {
        await (operations as? RoutedSession)?.provenEndpoints() ?? []
    }

    /// The network changed or the app came back to the foreground.
    public func networkChanged() async {
        guard active else { return }
        await (operations as? RoutedSession)?.networkChanged()
        await viewActionSender.retryNow()
        await viewsFeed.reconnect()
    }

    /// Something arrived from the Mac: whatever this device still has to tell it goes now.
    public func macReached() async {
        guard active else { return }
        await viewActionSender.retryNow()
    }

    private init(operations: any FeedOperations, server: URL, deviceID: String, macName: String?, macID: String?,
                 intents: OpenedActionIntents?) {
        self.operations = operations
        self.macName = macName
        self.macID = macID
        api = AlondraAPIClient(serviceURL: server, deviceID: deviceID, operations: operations)
        #if os(macOS)
        let client: ViewClient = .mac
        #else
        let client: ViewClient = .ios
        #endif
        let preferred = Locale.preferredLanguages.first?.split(separator: "-").first.map(String.init) ?? "en"
        let sender = ViewActionSender(transport: api, intents: intents?.store, setAside: intents?.setAside ?? SetAsideNotice())
        viewActionSender = sender
        viewsFeed = FeedSession(transport: api, client: client, lang: preferred == "es" ? "es" : "en",
            reconcileActions: { epoch, isCurrent in
                await sender.reconcilePending(dataEpoch: epoch, isCurrent: isCurrent)
                if await isCurrent() { await sender.resumeDeliveries(dataEpoch: epoch) }
            })
    }

    public func deactivate() async {
        active = false
        await viewActionSender.stop()
        await viewsFeed.stop()
        await operations.deactivate()
    }

    public func retryViewsConnection() async {
        guard active else { return }
        await networkChanged()
    }

    public func checkActive() throws {
        guard active else { throw CancellationError() }
    }

    public func checkSettingsConnection() async throws {
        try checkActive()
        _ = try await api.send(.data(.get, path: ["capabilities"]))
        try checkActive()
    }

    /// Why this Mac cannot be loaded right now, from one signed round trip.
    public func diagnose() async -> ConnectionProblem {
        do {
            try await checkSettingsConnection()
            return .unexpected
        } catch { return connectionProblem(for: error) }
    }

    public nonisolated func connectionProblem(for error: any Error) -> ConnectionProblem {
        ConnectionClassifier.problem(for: error, local: isLocal, networkOffline: NetworkReachability.shared.isOffline,
                                     macName: macName)
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
