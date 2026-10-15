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
    /// Whether this Mac is answering, decided from every request to it.
    public nonisolated let monitor: ConnectionMonitor
    private let operations: any FeedOperations
    private let macName: String?
    private let irohEndpoint: IrohEndpointHolder?
    private var movesTask: Task<Void, Never>?
    /// The paired Mac's ID; nil for the Mac this app runs on.
    public nonisolated let macID: String?
    private var active = true

    /// A Mac paired on the local network talks only directly; one paired with an account also
    /// keeps the relay, and reaches it directly only once Local Network access is allowed. With
    /// `irohDialer`, a Mac paired with an account is reached first over its iroh endpoint.
    public init(approved: ApprovedServicePairing, auth: AuthSession, storageDirectory: URL? = nil,
                measurements: ConnectionMeasurements? = nil,
                localNetworkAllowed: @escaping @Sendable () -> Bool = { LocalNetworkAccess.allowed },
                irohDialer: (@Sendable (IrohIdentity) -> any IrohDialer)? = nil) async throws {
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
        var iroh: RoutedSession.Iroh?
        var irohEndpoint: IrohEndpointHolder?
        if let relay, let irohDialer {
            let identity = IrohIdentity(pairingSigning: signing, macID: approved.macID)
            let endpoint = IrohEndpointHolder(approved.irohEndpoint)
            let carrier = IrohCarrier(dialer: irohDialer(identity), macID: approved.macID, deviceID: approved.deviceID,
                signing: signing, address: {
                    guard let id = endpoint.value, let url = try? await relay.irohRelay() else { return nil }
                    return IrohMacAddress(endpointID: id, relay: url)
                }, prepare: { _ in try await relay.bindIrohNode(identity) })
            iroh = RoutedSession.Iroh(session: DirectSession(macID: approved.macID, deviceID: approved.deviceID,
                signing: signing, agreement: agreement, macSigning: macSigning, macAgreement: macAgreement,
                iroh: carrier, journal: journal), carrier: carrier)
            irohEndpoint = endpoint
        }
        let hasRelay = relay != nil
        let directAllowed: @Sendable () -> Bool = { !hasRelay || localNetworkAllowed() }
        let routed = RoutedSession(iroh: iroh, direct: direct, router: router, relay: relay, directAllowed: directAllowed)
        self.init(operations: routed, server: server, deviceID: approved.deviceID, macName: approved.name,
                  macID: approved.macID, intents: intents, measurements: measurements, irohEndpoint: irohEndpoint)
        if let carrier = iroh?.carrier {
            await follow(carrier, measurements: measurements)
            // Open the live connection before the first request; the background closes it.
            await carrier.start()
        }
    }

    public static func local(_ session: LoopbackSession, identity: LoopbackIdentityStore,
                             storageDirectory: URL? = nil, measurements: ConnectionMeasurements? = nil) async throws -> Connection {
        let revision = try await identity.credentialRevision()
        let material = try await identity.bindingSecret()
        let intents = await openIntents(in: storageDirectory, keyMaterial: material,
            scope: ActionIntentScope(service: session.base.absoluteString, device: "this-mac", credentialRevision: revision))
        return Connection(operations: session, server: session.base, deviceID: "mac", macName: nil,
                          macID: nil, intents: intents, measurements: measurements, irohEndpoint: nil)
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
        let reply = try JSONDecoder().decode(DirectRoutesReply.self, from: try await api.send(.data(.get, path: ["routes"])))
        if let endpoint = reply.irohEndpoint, let irohEndpoint, irohEndpoint.value != endpoint {
            irohEndpoint.learn(endpoint)
            await routed.networkChanged()
        }
        await routed.offer(reply.endpoints, from: .mac)
    }

    /// The Mac's iroh endpoint ID, once known: the pairing keeps it.
    public nonisolated var learnedIrohEndpoint: String? { irohEndpoint?.value }

    /// The app came to the foreground: open the live connection to this Mac.
    public func goLive() async {
        guard active else { return }
        await (operations as? RoutedSession)?.goLive()
    }

    /// The app went to the background: close the live connection; push still reaches this device.
    public func goIdle() async {
        await (operations as? RoutedSession)?.goIdle()
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
        await monitor.handle(.retrying)
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
                 intents: OpenedActionIntents?, measurements: ConnectionMeasurements?, irohEndpoint: IrohEndpointHolder?) {
        self.operations = operations
        self.macName = macName
        self.irohEndpoint = irohEndpoint
        self.macID = macID
        let local = operations is LoopbackSession
        let monitor = ConnectionMonitor { error in
            ConnectionClassifier.problem(for: error, local: local, networkOffline: NetworkReachability.shared.isOffline,
                                         macName: macName)
        }
        self.monitor = monitor
        let mac = ConnectionMeasurement.macReference(macID)
        let monitored = MonitoredOperations(operations, monitor: monitor, route: {
            local ? .thisMac : await (operations as? RoutedSession)?.route ?? .relay
        }, record: { observation in
            let network = ConnectionMeasurement.NetworkSnapshot(interface: NetworkReachability.shared.interfaceKind,
                                                                radio: RadioAccess.current)
            await measurements?.record(ConnectionMeasurement(observation, at: Date(), mac: mac, network: network))
        })
        api = AlondraAPIClient(serviceURL: server, deviceID: deviceID, operations: monitored)
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
        Task { await monitor.follow(network: NetworkReachability.shared.changes()) }
    }

    /// The live connection's path shows as soon as iroh changes it, and how long it took to
    /// open is measured once per return to the foreground.
    private func follow(_ carrier: IrohCarrier, measurements: ConnectionMeasurements?) async {
        let moves = NetworkReachability.shared.moves()
        movesTask = Task { [weak self] in
            for await _ in moves {
                guard let routed = await self?.operations as? RoutedSession else { return }
                await routed.networkMoved()
            }
        }
        let monitor = monitor
        let mac = ConnectionMeasurement.macReference(macID)
        await carrier.observe { event in
            switch event {
            case let .connected(route, after):
                guard let measurements else { return }
                let network = ConnectionMeasurement.NetworkSnapshot(interface: NetworkReachability.shared.interfaceKind,
                                                                    radio: RadioAccess.current)
                Task { await measurements.record(.connected(after: after, route: route, at: Date(), mac: mac, network: network)) }
            case let .routeChanged(route):
                Task { await monitor.handle(.routeChanged(route)) }
            case .lost:
                break
            }
        }
    }

    public func deactivate() async {
        active = false
        movesTask?.cancel()
        await monitor.stop()
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
