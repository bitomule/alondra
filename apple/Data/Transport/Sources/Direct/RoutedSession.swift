import AlondraContract
import Foundation

/// What `RoutedSession` needs from the relay; `RelaySession` in the app, a fake in tests.
public protocol RelayOperations: OperationAPI, PushRegistrationAPI {
    func deactivate() async
    /// The envelope sent for an operation, if one left this device.
    func sentEnvelope(for requestID: String) async -> Envelope?
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
    private let requestClock: RequestClock
    let mutations: MutationRunner
    private var lastRoute: ConnectionRoute?
    private var relayFirst: Bool
    private var probe: Task<Void, Never>?
    private var probedAt: ContinuousClock.Instant?

    /// `directAllowed` keeps a relay pairing off the local network until the person has allowed
    /// it, so the system never asks for Local Network access on its own.
    public init(direct: DirectSession?, router: DirectRouter?, relay: (any RelayOperations)?,
                directAllowed: @escaping @Sendable () -> Bool = { true }, probeInterval: Duration = .seconds(60)) {
        self.init(direct: direct, router: router, relay: relay, directAllowed: directAllowed,
            probeInterval: probeInterval, requestClock: .continuous)
    }

    init(direct: DirectSession?, router: DirectRouter?, relay: (any RelayOperations)?,
         directAllowed: @escaping @Sendable () -> Bool = { true }, probeInterval: Duration = .seconds(60),
         requestClock: RequestClock) {
        self.requestClock = requestClock
        mutations = MutationRunner(clock: requestClock)
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

    public func recover(_ request: APIRequest) async throws -> APIResponse {
        if let relay, let result = try? await relay.recover(request), result != .unknown { return result }
        if let direct, directAllowed() { return try await direct.recover(request) }
        return .unknown
    }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        guard request.method != "GET" else { return try await performPrepared(request) }
        return try await mutations.run(conservativeUnknown: true, startedAt: requestClock.now(), unknown: { APIResponse.unknown }) {
            try await self.performPrepared(request)
        }
    }

    private func performPrepared(_ request: APIRequest) async throws -> APIResponse {
        try MutationScope.check()
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
            // The relay could not say whether the Mac ran it: ask the Mac directly with the same
            // envelope, which it answers from its journal if it already did.
            if response == .unknown, request.method != "GET", let direct, directAllowed(),
               let answered = try? await resend(request, from: relay, over: direct), answered != .unknown {
                return answered
            }
            return response
        } catch where !(error is CancellationError) {
            guard let direct, directAllowed() else { throw error }
            let relayError = error
            do { return try await resend(request, from: relay, over: direct) }
            catch { throw Self.directFailed(error) ? relayError : error }
        }
    }

    /// A mutation the relay already sent goes out again as those same bytes.
    private func resend(_ request: APIRequest, from relay: any RelayOperations,
                        over direct: DirectSession) async throws -> APIResponse {
        try MutationScope.check()
        if request.method != "GET", let envelope = await relay.sentEnvelope(for: request.operationID) {
            try MutationScope.check()
            try await direct.adopt(envelope, for: request)
        }
        try MutationScope.check()
        let response = try await viaDirect(direct, request)
        relayFirst = false
        return response
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
        await mutations.cancelAll()
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
        probe = MutationScope.$current.withValue(nil) {
            Task {
                let answered = (try? await direct.perform(request)) != nil
                self.probeFinished(answered)
            }
        }
    }

    private func probeFinished(_ answered: Bool) {
        probe = nil
        if answered { relayFirst = false }
    }

    /// No address proved to be this Mac: the relay's own answer is the one to believe.
    private static func directFailed(_ error: any Error) -> Bool {
        error is DirectRouteError
    }
}
