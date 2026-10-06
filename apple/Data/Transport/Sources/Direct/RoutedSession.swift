import AlondraContract
import Foundation

/// One paired Mac over the best path it has: directly on the LAN or tailnet, and through the
/// relay only for a pairing made with an account. Each request goes whole over one carrier.
public actor RoutedSession: OperationAPI, PushRegistrationAPI {
    private let direct: DirectSession?
    private let router: DirectRouter?
    private let relay: RelaySession?
    private var lastRoute: ConnectionRoute?

    public init(direct: DirectSession?, router: DirectRouter?, relay: RelaySession?) {
        self.direct = direct
        self.router = router
        self.relay = relay
    }

    public nonisolated var hasRelay: Bool { relay != nil }

    /// The path the last answered request took.
    public var route: ConnectionRoute? { lastRoute }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        if let direct {
            do {
                let response = try await direct.perform(request)
                lastRoute = await router?.route
                return response
            } catch DirectRouteError.unreachable where relay != nil {}
        }
        guard let relay else { throw DirectRouteError.unreachable }
        let response = try await relay.perform(request)
        lastRoute = .relay
        return response
    }

    public func registerServicePush(token: String, environment: String, binding: String) async throws {
        guard let relay else { throw OperationError.invalidConfiguration }
        try await relay.registerServicePush(token: token, environment: environment, binding: binding)
    }

    /// Addresses learned from Bonjour or from the Mac's `/routes`.
    public func offer(_ endpoints: [DirectEndpoint]) async {
        await router?.offer(endpoints)
    }

    public func knownEndpoints() async -> [DirectEndpoint] {
        await router?.known ?? []
    }

    /// The network changed or the app came back: the next request probes again.
    public func networkChanged() async {
        await router?.invalidate()
    }

    public func deactivate() async {
        await direct?.deactivate()
        await relay?.deactivate()
    }
}
