import AlondraContract
import Foundation

/// One paired Mac over the best path it has: its iroh endpoint first, for a Mac paired with an
/// account, then directly on the LAN or tailnet. Each request goes whole over one carrier.
///
/// A request iroh could not send, and a read it got no answer to, go direct; a change that may
/// have reached the Mac stays unknown and is resent later as the same bytes, over iroh first.
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
    private let account: RelayAccount?
    private let directAllowed: @Sendable () -> Bool
    private let requestClock: RequestClock
    let mutations: MutationRunner
    private var lastRoute: ConnectionRoute?

    /// `directAllowed` keeps a pairing made with an account off the local network until the
    /// person has allowed it, so the system never asks for Local Network access on its own.
    public init(iroh: Iroh? = nil, direct: DirectSession?, router: DirectRouter?, account: RelayAccount? = nil,
                directAllowed: @escaping @Sendable () -> Bool = { true }) {
        self.init(iroh: iroh, direct: direct, router: router, account: account, directAllowed: directAllowed,
                  requestClock: .continuous)
    }

    init(iroh: Iroh? = nil, direct: DirectSession?, router: DirectRouter?, account: RelayAccount? = nil,
         directAllowed: @escaping @Sendable () -> Bool = { true }, requestClock: RequestClock) {
        self.iroh = iroh
        self.requestClock = requestClock
        mutations = MutationRunner(clock: requestClock)
        self.direct = direct
        self.router = router
        self.account = account
        self.directAllowed = directAllowed
    }

    /// The path the last answered request took.
    public var route: ConnectionRoute? { lastRoute }

    public func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        try await iroh?.session.retireActions(acknowledgment)
        try await direct?.retireActions(acknowledgment)
    }

    public func recover(_ request: APIRequest) async throws -> APIResponse {
        if let iroh, let result = try? await iroh.session.recover(request), result != .unknown { return result }
        if let direct, directAllowed() { return try await direct.recover(request) }
        return .unknown
    }

    public func lookup(_ request: APIRequest) async throws -> OperationLookup {
        if let iroh, let result = try? await iroh.session.lookup(request), result != .unknown { return result }
        if let direct, directAllowed() { return try await direct.lookup(request) }
        return .unknown
    }

    public func resend(_ request: APIRequest) async throws -> APIResponse {
        guard request.method != "GET" else { return .unknown }
        return try await mutations.run(knownDispatched: true, conservativeUnknown: true, startedAt: requestClock.now(), unknown: { APIResponse.unknown }) {
            try await self.resendPrepared(request)
        }
    }

    /// iroh first with the bytes it holds; the direct path with those same bytes when iroh cannot
    /// reach the Mac or got no answer.
    private func resendPrepared(_ request: APIRequest) async throws -> APIResponse {
        try MutationScope.check()
        var irohFailure: (any Error)?
        if let iroh {
            do {
                let response = try await iroh.session.resend(request)
                if response != .unknown {
                    lastRoute = await iroh.carrier.route ?? .irohRelay
                    return response
                }
            } catch OperationError.sealExpired {
                throw OperationError.sealExpired
            } catch where !(error is CancellationError) {
                irohFailure = error
            }
        }
        guard let direct, directAllowed() else {
            if let irohFailure { throw irohFailure }
            return .unknown
        }
        do {
            try MutationScope.check()
            let envelope = await iroh?.session.sealedEnvelope(for: request.operationID)
            let response = try await direct.resend(request, adopting: envelope)
            if response != .unknown { lastRoute = await router?.route }
            return response
        } catch where Self.directFailed(error) {
            if let irohFailure { throw irohFailure }
            return .unknown
        }
    }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        do {
            guard request.method != "GET", !request.isActionJournalControl else { return try await performPrepared(request) }
            return try await mutations.run(conservativeUnknown: true, startedAt: requestClock.now(), unknown: { APIResponse.unknown }) {
                try await self.performPrepared(request)
            }
        } catch {
            guard !(error is CancellationError) else { throw error }
            if await account?.removedFromAccount() == true { throw AccountDeviceError.removedFromWeb }
            throw error
        }
    }

    private func performPrepared(_ request: APIRequest) async throws -> APIResponse {
        try MutationScope.check()
        let read = request.method == "GET"
        var irohFailure: (any Error)?
        if let iroh {
            do {
                let response = read ? try await iroh.session.liveRead(request) : try await iroh.session.perform(request)
                lastRoute = await iroh.carrier.route ?? .irohRelay
                return response
            } catch where Self.irohCanHandOver(error, read: read) {
                irohFailure = error
            }
        }
        guard let direct, directAllowed() else { throw irohFailure ?? DirectRouteError.unreachable }
        do {
            // An action iroh sealed but never sent goes direct as those same bytes, never sealed again.
            if !read, let envelope = await iroh?.session.sealedEnvelope(for: request.operationID) {
                try MutationScope.check()
                try await direct.adopt(envelope, for: request)
            }
            return try await viaDirect(direct, request)
        } catch where Self.directFailed(error) {
            throw irohFailure ?? error
        }
    }

    public func registerServicePush(token: String, environment: String, binding: String) async throws {
        guard let account else { throw OperationError.invalidConfiguration }
        try await account.registerServicePush(token: token, environment: environment, binding: binding)
    }

    public func registerActivity(_ registration: RelayActivityRegistration) async throws {
        guard let account else { throw OperationError.invalidConfiguration }
        try await account.registerActivity(registration)
    }

    public func unregisterActivity(phoneID: String) async throws {
        guard let account else { throw OperationError.invalidConfiguration }
        try await account.unregisterActivity(phoneID: phoneID)
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

    /// The app went to the background: close the iroh connection. A request made from the
    /// background opens one just for itself.
    public func goIdle() async {
        await iroh?.carrier.stop()
    }

    /// The device moved to another network: the live connection is made again on it.
    public func networkMoved() async {
        await iroh?.carrier.networkMoved()
    }

    /// The network changed or the app came back: iroh reconnects now and direct addresses are
    /// proved again.
    public func networkChanged() async {
        await iroh?.carrier.networkChanged()
        await router?.invalidate()
    }

    public func deactivate() async {
        await mutations.cancelAll()
        await iroh?.session.deactivate()
        await iroh?.carrier.shutdown()
        await direct?.deactivate()
        await account?.deactivate()
    }

    private func viaDirect(_ direct: DirectSession, _ request: APIRequest) async throws -> APIResponse {
        let response = try await direct.perform(request)
        lastRoute = await router?.route
        return response
    }

    /// The request never ran on the Mac, or it is a read: the other carriers may take it.
    private static func irohCanHandOver(_ error: any Error, read: Bool) -> Bool {
        switch error {
        case IrohCarrierError.unavailable, IrohCarrierError.refused: return true
        case IrohCarrierError.noAnswer, is DeadlineExceeded: return read && !Task.isCancelled
        default: return false
        }
    }

    /// No address proved to be this Mac.
    private static func directFailed(_ error: any Error) -> Bool {
        error is DirectRouteError
    }
}
