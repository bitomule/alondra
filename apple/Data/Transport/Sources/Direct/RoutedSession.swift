import AlondraContract
import Foundation

/// What `RoutedSession` needs from the relay; `RelaySession` in the app, a fake in tests.
public protocol RelayOperations: OperationAPI, PushRegistrationAPI {
    func deactivate() async
}

extension RelaySession: RelayOperations {}

/// One paired Mac over the best path it has: directly on the LAN or tailnet, and through the
/// relay only for a pairing made with an account. Each request goes whole over one carrier.
///
/// With a relay, no request waits on the direct path: the relay goes first and the direct path
/// is probed in the background, at most once a minute or when the network changes. Once a
/// sealed answer comes back directly, requests go direct until that path fails. When the relay
/// itself is out of reach (no internet, relay down) the direct path is tried inline.
public actor RoutedSession: OperationAPI, PushRegistrationAPI {
    private let direct: DirectSession?
    private let router: DirectRouter?
    private let relay: (any RelayOperations)?
    private let directAllowed: @Sendable () -> Bool
    private let probeInterval: Duration
    private let clock = ContinuousClock()
    private var lastRoute: ConnectionRoute?
    private var relayFirst: Bool
    private var probe: Task<Void, Never>?
    private var probedAt: ContinuousClock.Instant?

    /// `directAllowed` keeps a relay pairing off the local network until the person has allowed
    /// it, so the system never asks for Local Network access on its own.
    public init(direct: DirectSession?, router: DirectRouter?, relay: (any RelayOperations)?,
                directAllowed: @escaping @Sendable () -> Bool = { true }, probeInterval: Duration = .seconds(60)) {
        self.direct = direct
        self.router = router
        self.relay = relay
        self.directAllowed = directAllowed
        self.probeInterval = probeInterval
        relayFirst = relay != nil
    }

    public nonisolated var hasRelay: Bool { relay != nil }

    /// The path the last answered request took.
    public var route: ConnectionRoute? { lastRoute }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        guard let relay else {
            guard let direct else { throw DirectRouteError.unreachable }
            return try await viaDirect(direct, request)
        }
        if !relayFirst, let direct, directAllowed() {
            do { return try await viaDirect(direct, request) }
            catch where Self.directFailed(error) { relayFirst = true }
        }
        startProbe()
        do {
            let response = try await relay.perform(request)
            lastRoute = .relay
            return response
        } catch where !(error is CancellationError) {
            guard let direct, directAllowed() else { throw error }
            let relayError = error
            do {
                let response = try await viaDirect(direct, request)
                relayFirst = false
                return response
            } catch { throw Self.directFailed(error) ? relayError : error }
        }
    }

    public func registerServicePush(token: String, environment: String, binding: String) async throws {
        guard let relay else { throw OperationError.invalidConfiguration }
        try await relay.registerServicePush(token: token, environment: environment, binding: binding)
    }

    /// Addresses learned from the Mac's sealed `/routes` or from Bonjour.
    public func offer(_ endpoints: [DirectEndpoint], from source: DirectRouter.Source) async {
        await router?.offer(endpoints, from: source)
    }

    /// The addresses a sealed answer proved, the only ones worth saving.
    public func provenEndpoints() async -> [DirectEndpoint] {
        await router?.provenEndpoints ?? []
    }

    /// The network changed or the app came back: start over from the relay, probe direct again.
    public func networkChanged() async {
        await router?.invalidate()
        probedAt = nil
        relayFirst = relay != nil
        if relay != nil { startProbe() }
    }

    public func deactivate() async {
        probe?.cancel()
        await direct?.deactivate()
        await relay?.deactivate()
    }

    private func viaDirect(_ direct: DirectSession, _ request: APIRequest) async throws -> APIResponse {
        let response = try await direct.perform(request)
        lastRoute = await router?.route
        return response
    }

    private func startProbe() {
        guard probe == nil, let direct, directAllowed(),
              probedAt.map({ $0.duration(to: clock.now) >= probeInterval }) ?? true,
              let request = try? APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "probe")
        else { return }
        probedAt = clock.now
        probe = Task {
            let answered = (try? await direct.perform(request)) != nil
            self.probeFinished(answered)
        }
    }

    private func probeFinished(_ answered: Bool) {
        probe = nil
        if answered { relayFirst = false }
    }

    /// Failures that say nothing about the Mac itself: an unreachable or impostor address, or an
    /// unsigned refusal. The relay's own answer is the one to believe.
    private static func directFailed(_ error: any Error) -> Bool {
        error is DirectRouteError || error as? DirectSessionError == .unauthorized
    }
}
