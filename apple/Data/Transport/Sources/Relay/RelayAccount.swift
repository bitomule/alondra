import CryptoKit
import Foundation

/// What a Mac paired with an account needs from the relay it was paired through: the iroh relay
/// to dial through, admission of this device's iroh endpoint, push registration and the iroh
/// endpoint the Mac bound. Requests to the Mac never go through it.
public actor RelayAccount: PushRegistrationAPI {
    private let client: any Transport
    private let macID: String
    private let deviceID: String
    private var active = true

    public init(relay: URL, macID: String, deviceID: String, signing: P256.Signing.PrivateKey,
                bearer: @escaping @Sendable () async throws -> String,
                bearerRefusedForToken: (@Sendable (String) async -> Void)? = nil,
                transport: (any HTTPTransport)? = nil) throws {
        let client = try AlondraAPIClient.makeTransport(base: relay, deviceID: deviceID, signing: signing,
            bearer: bearer, bearerRefusedForToken: bearerRefusedForToken, transport: transport)
        self.init(client: client, macID: macID, deviceID: deviceID)
    }

    init(client: any Transport, macID: String, deviceID: String) {
        self.client = client
        self.macID = macID
        self.deviceID = deviceID
    }

    /// The iroh relay this relay admits endpoints to, nil while it has none.
    public func irohRelay() async throws -> URL? {
        try checkActive()
        return try await client.bootstrapIfNeeded().irohRelay
    }

    /// Lets this device's iroh endpoint for this pairing through the iroh relay.
    public func bindIrohNode(_ identity: IrohIdentity) async throws {
        try checkActive()
        try await client.bindIrohNode(identity.nodeBinding(deviceID: deviceID))
    }

    /// Where to dial the Mac: the endpoint the pairing knows, else the one the Mac bound at the
    /// relay, so a pairing made before the Mac had one learns it; through this relay's iroh relay.
    public func macAddress(known: String?) async -> IrohMacAddress? {
        var endpoint = known
        if endpoint == nil { endpoint = try? await macIrohEndpoint() }
        guard let endpoint, let relay = try? await irohRelay() else { return nil }
        return IrohMacAddress(endpointID: endpoint, relay: relay)
    }

    /// The iroh endpoint the Mac bound at the relay, nil while it has bound none.
    func macIrohEndpoint() async throws -> String? {
        try checkActive()
        let macs = try await client.macs()
        try checkActive()
        return macs.first { $0.macID == macID }?.irohEndpoint
    }

    public func registerServicePush(token: String, environment: String, binding: String) async throws {
        try checkActive()
        let registration = try RelayPushRegistration(token: token, environment: environment, pushBinding: binding)
        _ = try await client.bootstrap()
        try checkActive()
        try await client.registerPush(registration)
        try checkActive()
    }

    public func registerActivity(_ registration: RelayActivityRegistration) async throws {
        try checkActive()
        _ = try await client.bootstrapIfNeeded()
        try checkActive()
        try await client.registerActivity(registration)
    }

    public func unregisterActivity(phoneID: String) async throws {
        try checkActive()
        _ = try await client.bootstrapIfNeeded()
        try checkActive()
        try await client.unregisterActivity(phoneID: phoneID)
    }

    public func deactivate() async {
        active = false
        if let concrete = client as? RelayTransport { await concrete.cancelBootstrap() }
    }

    private func checkActive() throws {
        guard active, !Task.isCancelled else { throw CancellationError() }
    }
}
