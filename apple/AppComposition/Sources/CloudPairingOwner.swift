import AlondraAuth
import AlondraServicePairing
import AlondraCloudPairingFeature
import ComposableArchitecture
import Observation

/// Process-owned credential and pairing use cases. Secrets never enter presentation state.
@MainActor
@Observable
public final class CloudPairingOwner {
    public let auth: AuthSession
    private let displayName: String
    private let prepareCamera: @Sendable () async -> Bool
    private let pairing = ServicePairingClient()
    private var attempt: UUID?
    private var generation = 0
    public private(set) var services: CloudAppServices?
    @ObservationIgnored public lazy var store = Store(initialState: CloudPairingFeature.State()) { CloudPairingFeature() } withDependencies: {
        $0.cloudPairing = CloudPairingService(
            restore: { [self] in try await restore() }, login: { [self] in try await login() },
            begin: { [self] payload in try await begin(payload) }, approve: { [self] in try await approve() },
            cancel: { [self] in await cancel() }, logout: { [self] in try await logout() })
        $0.cloudPairing.prepareCamera = prepareCamera
    }
    public init(auth: AuthSession, displayName: String,
                prepareCamera: @escaping @Sendable () async -> Bool = { false }) {
        self.auth = auth
        self.displayName = displayName
        self.prepareCamera = prepareCamera
    }
    /// A QR may name the relay but cannot authorize a destination for the account bearer.
    private static func validateRelay(_ url: URL) throws {
        guard url.scheme == "https", url.host()?.lowercased() == "alondra-api.davidcollado.dev",
              url.port == nil || url.port == 443, url.user == nil, url.password == nil,
              url.query == nil, url.fragment == nil, url.path.isEmpty || url.path == "/" else {
            throw ServicePairingError.invalidInvitation
        }
    }
    private func restore() async throws -> Bool {
        guard let approved = try await pairing.approvedPairing() else { return false }
        try Self.validateRelay(approved.relayURL)
        let current = generation
        let subject = try await auth.verifiedSubject()
        guard subject == approved.ownerSub else { throw ServicePairingError.invalidIdentity }
        try Task.checkCancellation()
        guard current == generation else { throw CancellationError() }
        try await activate(approved, auth)
        return true
    }
    private func login() async throws { try await auth.login(); _ = try await auth.verifiedSubject() }
    private func begin(_ payload: String) async throws -> String {
        let invitation = try ServicePairingInvitation(qrPayload: payload)
        try Self.validateRelay(invitation.url)
        let current = generation
        let subject = try await auth.verifiedSubject()
        let token = try await auth.bearerToken()
        let pending = try await pairing.begin(qrPayload: payload,
            authorization: PairingAuthorization(bearerToken: token, ownerSub: subject), displayName: displayName)
        guard current == generation, !Task.isCancelled else {
            await pairing.discard(pending.attemptID)
            throw CancellationError()
        }
        attempt = pending.attemptID
        return pending.comparisonCode
    }
    private func approve() async throws {
        guard let attempt else { throw ServicePairingError.missingAttempt }
        let current = generation
        let approved: ApprovedServicePairing
        do { approved = try await pairing.waitForApproval(attempt) }
        catch {
            await pairing.discard(attempt)
            if self.attempt == attempt { self.attempt = nil }
            throw error
        }
        guard current == generation, !Task.isCancelled else { throw CancellationError() }
        self.attempt = nil
        try await activate(approved, auth)
    }
    private func cancel() async {
        generation += 1
        if let attempt { self.attempt = nil; await pairing.discard(attempt) }
    }
    private func activate(_ approved: ApprovedServicePairing, _ auth: AuthSession) async throws {
        let current = generation
        let next = try CloudAppServices(approved: approved, auth: auth)
        let previous = services
        services = nil
        await previous?.deactivate()
        guard current == generation, !Task.isCancelled else {
            await next.deactivate()
            throw CancellationError()
        }
        services = next
    }
    private func deactivate() async {
        generation += 1
        let previous = services
        services = nil
        await previous?.deactivate()
    }
    private func logout() async throws {
        await deactivate()
        try await pairing.removeApprovedPairing()
        try await auth.logout()
    }
}
