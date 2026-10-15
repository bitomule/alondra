import AlondraContract
import Foundation

/// What `RoutedSession` needs from the relay; `RelaySession` in the app, a fake in tests.
public protocol RelayOperations: OperationAPI, PushRegistrationAPI {
    func deactivate() async
    /// The envelope sent for an operation, if one left this device.
    func sentEnvelope(for requestID: String) async -> Envelope?
}

extension RelaySession: RelayOperations {}

/// One paired Mac over the best path it has: its iroh endpoint while that connection is up,
/// directly on the LAN or tailnet, and through the relay only for a pairing made with an account.
/// Each request goes whole over one carrier.
///
/// iroh goes first when it has a live connection (or one ready within a moment). A request it
/// could not send, and a read it got no answer to, go on as before; a change that may have
/// reached the Mac stays unknown and is resent later as the same bytes.
///
/// With a relay, no request waits on the direct path: the relay goes first and the direct path
/// is probed in the background, at most once a minute or when the network changes. Once a
/// sealed answer comes back directly, requests go direct until that path fails; a read gets one
/// short try there, so a slow Mac or a refused local network hands it to the relay in seconds.
/// When the relay itself is out of reach (no internet, relay down) or cannot get the Mac's
/// answer, the direct path is tried inline.
public actor RoutedSession: OperationAPI, PushRegistrationAPI {
    public struct Iroh: Sendable {
        let session: DirectSession
        public let carrier: IrohCarrier

        public init(session: DirectSession, carrier: IrohCarrier) {
            self.session = session
            self.carrier = carrier
        }
    }

    private let iroh: Iroh?
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
    public init(iroh: Iroh? = nil, direct: DirectSession?, router: DirectRouter?, relay: (any RelayOperations)?,
                directAllowed: @escaping @Sendable () -> Bool = { true }, probeInterval: Duration = .seconds(60)) {
        self.init(iroh: iroh, direct: direct, router: router, relay: relay, directAllowed: directAllowed,
            probeInterval: probeInterval, requestClock: .continuous)
    }

    init(iroh: Iroh? = nil, direct: DirectSession?, router: DirectRouter?, relay: (any RelayOperations)?,
         directAllowed: @escaping @Sendable () -> Bool = { true }, probeInterval: Duration = .seconds(60),
         requestClock: RequestClock) {
        self.iroh = iroh
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

    public func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        try await iroh?.session.retireActions(acknowledgment)
        try await direct?.retireActions(acknowledgment)
        try await relay?.retireActions(acknowledgment)
    }

    public func recover(_ request: APIRequest) async throws -> APIResponse {
        if let iroh, let result = try? await iroh.session.recover(request), result != .unknown { return result }
        if let relay, let result = try? await relay.recover(request), result != .unknown { return result }
        if let direct, directAllowed() { return try await direct.recover(request) }
        return .unknown
    }

    public func lookup(_ request: APIRequest) async throws -> OperationLookup {
        if let iroh, let result = try? await iroh.session.lookup(request), result != .unknown { return result }
        if let relay, let result = try? await relay.lookup(request), result != .unknown { return result }
        if let direct, directAllowed() { return try await direct.lookup(request) }
        return .unknown
    }

    public func resend(_ request: APIRequest) async throws -> APIResponse {
        guard request.method != "GET" else { return .unknown }
        return try await mutations.run(knownDispatched: true, conservativeUnknown: true, startedAt: requestClock.now(), unknown: { APIResponse.unknown }) {
            try await self.resendPrepared(request)
        }
    }

    /// The relay first when it holds the sealed bytes; the direct path with those same bytes when
    /// the relay does not answer or never had them.
    private func resendPrepared(_ request: APIRequest) async throws -> APIResponse {
        try MutationScope.check()
        if let iroh {
            do {
                let response = try await iroh.session.resend(request)
                if response != .unknown {
                    lastRoute = await iroh.carrier.route ?? .irohRelay
                    return response
                }
            } catch OperationError.sealExpired {
                throw OperationError.sealExpired
            } catch where !(error is CancellationError) {}
        }
        var relayFailure: (any Error)?
        if let relay {
            do {
                let response = try await relay.resend(request)
                if response != .unknown {
                    lastRoute = .relay
                    return response
                }
            } catch OperationError.sealExpired {
                throw OperationError.sealExpired
            } catch where !(error is CancellationError) {
                relayFailure = error
            }
        }
        guard let direct, directAllowed() else {
            if let relayFailure { throw relayFailure }
            return .unknown
        }
        do {
            try MutationScope.check()
            let envelope = await relay?.sentEnvelope(for: request.operationID)
            let response = try await direct.resend(request, adopting: envelope)
            if response != .unknown {
                lastRoute = await router?.route
                relayFirst = false
            }
            return response
        } catch where Self.directFailed(error) {
            if let relayFailure { throw relayFailure }
            return .unknown
        }
    }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        guard request.method != "GET", !request.isActionJournalControl else { return try await performPrepared(request) }
        return try await mutations.run(conservativeUnknown: true, startedAt: requestClock.now(), unknown: { APIResponse.unknown }) {
            try await self.performPrepared(request)
        }
    }

    private func performPrepared(_ request: APIRequest) async throws -> APIResponse {
        try MutationScope.check()
        if let iroh, await iroh.carrier.ready() {
            let read = request.method == "GET"
            do {
                let response = read ? try await iroh.session.liveRead(request) : try await iroh.session.perform(request)
                lastRoute = await iroh.carrier.route ?? .irohRelay
                return response
            } catch where Self.irohCanHandOver(error, read: read) {
                // An action iroh sealed and kept but never sent goes through the relay as those
                // same bytes, never sealed again.
                if !read, let relay, await relay.sentEnvelope(for: request.operationID) != nil {
                    let response = try await relay.resend(request)
                    if response != .unknown { lastRoute = .relay }
                    return response
                }
            }
        }
        guard let relay else {
            guard let direct else { throw DirectRouteError.unreachable }
            return try await viaDirect(direct, request)
        }
        let read = request.method == "GET"
        if !relayFirst, let direct, directAllowed() {
            do { return try await viaDirect(direct, request, quick: read) }
            catch where Self.relayCanTakeOver(error, read: read) { relayFirst = true }
        }
        startProbe()
        do {
            let response = try await relay.perform(request)
            lastRoute = .relay
            // The relay could not get the Mac's answer: ask the Mac directly. A change goes with
            // the same envelope, which the Mac answers from its journal if it already ran it.
            if response == .unknown, let direct, directAllowed(),
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

    /// The app came to the foreground: open the iroh connection.
    public func goLive() async {
        await iroh?.carrier.start()
    }

    /// The app went to the background: close the iroh connection; push is the only path left.
    public func goIdle() async {
        await iroh?.carrier.stop()
    }

    /// The device moved to another network: the live connection is made again on it.
    public func networkMoved() async {
        await iroh?.carrier.networkMoved()
    }

    /// The network changed or the app came back: start over from the relay, probe direct again.
    public func networkChanged() async {
        await iroh?.carrier.networkChanged()
        await router?.invalidate()
        probedAt = nil
        relayFirst = relay != nil
        if relay != nil { startProbe() }
    }

    public func deactivate() async {
        await mutations.cancelAll()
        await iroh?.session.deactivate()
        await iroh?.carrier.shutdown()
        probe?.cancel()
        await direct?.deactivate()
        await relay?.deactivate()
    }

    func drainProbe() async {
        await probe?.value
    }

    private func viaDirect(_ direct: DirectSession, _ request: APIRequest, quick: Bool = false) async throws -> APIResponse {
        let response: APIResponse
        if quick { response = try await direct.quickRead(request) } else { response = try await direct.perform(request) }
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

    /// The request never ran on the Mac, or it is a read: the other carriers may take it.
    private static func irohCanHandOver(_ error: any Error, read: Bool) -> Bool {
        switch error {
        case IrohCarrierError.unavailable, IrohCarrierError.refused: return true
        case IrohCarrierError.noAnswer, is DeadlineExceeded: return read && !Task.isCancelled
        default: return false
        }
    }

    /// No address proved to be this Mac: the relay's own answer is the one to believe.
    private static func directFailed(_ error: any Error) -> Bool {
        error is DirectRouteError
    }

    /// A change moves to the relay only when it never left this device; a read moves on any
    /// failure except the Mac's own sealed refusal.
    private static func relayCanTakeOver(_ error: any Error, read: Bool) -> Bool {
        if directFailed(error) { return true }
        guard read, !Task.isCancelled else { return false }
        switch error {
        case is CancellationError, is DirectSessionError, RelaySessionError.operationIdentityConflict: return false
        default: return true
        }
    }
}
