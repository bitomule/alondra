import AlondraContract
import Foundation

public enum DirectRouteError: Error, Equatable, Sendable {
    /// No address proved to be this Mac from the current network.
    case unreachable
    /// Like `unreachable`, and an address already proved on this network now answers as the same
    /// Mac ID with other keys. Only a hint: nothing the app trusts changes until the person pairs again.
    case identityChanged
}

public enum DirectSessionError: Error, Equatable, Sendable {
    /// The Mac itself, in an answer sealed with the pinned keys, said it revoked this device.
    case deviceRevoked
}

/// Picks the address one paired Mac answers on: LAN, then the tailnet IP, then the tailnet name.
///
/// Nothing unsigned changes what the app believes about the Mac. `hello` only filters: an
/// address that answers with other keys is skipped on its own and the Mac stays as not
/// answering. An address is trusted only on the network where a sealed answer through it opened
/// with the pinned keys; on a new network even a saved address is proved again first.
public actor DirectRouter {
    public enum Source: Sendable { case saved, mac, bonjour }

    struct RequestContext: Sendable {
        fileprivate let network: String
        fileprivate let generation: UInt64
    }

    func requestContext() -> RequestContext {
        RequestContext(network: network(), generation: generation)
    }

    func proved(_ endpoint: DirectEndpoint, in context: RequestContext) {
        guard context.generation == generation, context.network == network() else { return }
        proven[endpoint] = context.network
        rejected[endpoint] = nil
    }

    func isProven(_ endpoint: DirectEndpoint, in context: RequestContext) -> Bool {
        context.generation == generation && context.network == network() && proven[endpoint] == context.network
    }

    func reject(_ endpoint: DirectEndpoint, in context: RequestContext) {
        guard context.generation == generation, context.network == network() else { return }
        reject(endpoint, on: context.network)
    }

    static let maxBonjour = 4
    static let maxFromMac = 8
    private static let rejectionTime: Duration = .seconds(600)

    private let pinned: DirectMacIdentity
    private let http: DirectHTTP
    private let network: @Sendable () -> String
    private let freshness: Duration
    private let retryAfterFailure: Duration
    private let clock = ContinuousClock()
    private var saved: [DirectEndpoint]
    private var fromMac: [DirectEndpoint] = []
    private var fromBonjour: [DirectEndpoint] = []
    /// The network each address was last proved on.
    private var proven: [DirectEndpoint: String] = [:]
    private var rejected: [DirectEndpoint: (at: ContinuousClock.Instant, network: String)] = [:]
    private var current: DirectEndpoint?
    private var validated: (at: ContinuousClock.Instant, network: String)?
    private var failedAt: (at: ContinuousClock.Instant, network: String)?
    private var probing: Task<DirectEndpoint, any Error>?
    private var generation: UInt64 = 0
    private var identityChangedOn: String?

    /// `saved` are addresses a sealed answer proved before, on some network; `network` names the
    /// current one.
    public init(pinned: DirectMacIdentity, saved: [DirectEndpoint], http: DirectHTTP,
                network: @escaping @Sendable () -> String = { "" },
                freshness: Duration = .seconds(60), retryAfterFailure: Duration = .seconds(15)) {
        self.pinned = pinned
        self.http = http
        self.network = network
        self.freshness = freshness
        self.retryAfterFailure = retryAfterFailure
        self.saved = Self.unique(saved.filter(\.isValid))
    }

    public var known: [DirectEndpoint] { candidates }

    /// The addresses worth keeping with the pairing: the saved ones and every address a sealed
    /// answer proved, on any network.
    public var provenEndpoints: [DirectEndpoint] { candidates.filter { saved.contains($0) || proven[$0] != nil } }

    /// The route in use, or nil when no address has answered as this Mac yet.
    public var route: ConnectionRoute? { current?.route }

    /// Proved by a sealed answer on the network this device is on now.
    public func isProven(_ endpoint: DirectEndpoint) -> Bool { proven[endpoint] == network() }

    /// Adds addresses learned from the Mac's sealed `/routes` or from Bonjour. Bonjour can be
    /// flooded by anyone on the network, so it only ever holds a few of the newest addresses and
    /// never pushes out a saved or Mac-given one.
    public func offer(_ endpoints: [DirectEndpoint], from source: Source) {
        let fresh = endpoints.filter { $0.isValid && !saved.contains($0) }
        switch source {
        case .saved: saved = Self.unique(saved + fresh)
        case .mac: fromMac = Array(Self.unique(fresh + fromMac).prefix(Self.maxFromMac))
        case .bonjour:
            let others = Set(fromMac)
            fromBonjour = Array(Self.unique(fresh.filter { !others.contains($0) } + fromBonjour).prefix(Self.maxBonjour))
        }
        failedAt = nil
        if let current, candidates.first.map({ $0.rank < current.rank }) == true { validated = nil }
    }

    public func endpoint() async throws -> DirectEndpoint {
        if let current, let validated, validated.network == network(),
           validated.at.duration(to: clock.now) < freshness { return current }
        // Away from home every request would otherwise wait for the probes.
        if let failedAt, failedAt.network == network(), failedAt.at.duration(to: clock.now) < retryAfterFailure { throw unreachable }
        if let probing { return try await probing.value }
        let task = MutationScope.$current.withValue(nil) { Task { try await self.probe() } }
        probing = task
        defer { probing = nil }
        return try await task.value
    }

    /// A sealed answer from this address opened with the pinned keys.
    public func proved(_ endpoint: DirectEndpoint) {
        proven[endpoint] = network()
        rejected[endpoint] = nil
    }

    /// This address answered with something that is not this Mac, or with an unsigned refusal:
    /// skip it, and only it.
    public func reject(_ endpoint: DirectEndpoint) {
        reject(endpoint, on: network())
    }

    private func reject(_ endpoint: DirectEndpoint, on rejectedOn: String) {
        rejected[endpoint] = (clock.now, rejectedOn)
        if proven[endpoint] == rejectedOn { proven[endpoint] = nil }
        if current == endpoint {
            current = nil
            validated = nil
        }
    }

    /// The last address stopped answering, or the network changed: probe again on next use.
    public func invalidate() {
        generation &+= 1
        validated = nil
        failedAt = nil
    }

    private var candidates: [DirectEndpoint] {
        Self.unique(saved + fromMac + fromBonjour).enumerated()
            .sorted { ($0.element.rank, $0.offset) < ($1.element.rank, $1.offset) }.map(\.element)
    }

    private func probe() async throws -> DirectEndpoint {
        let startedOn = network()
        let startedGeneration = generation
        let now = clock.now
        rejected = rejected.filter { $0.value.network == startedOn && $0.value.at.duration(to: now) < Self.rejectionTime }
        let candidates = candidates.filter { rejected[$0] == nil }
        let http = http
        let answers = await withTaskGroup(of: (Int, DirectMacIdentity?).self) { group in
            for (index, endpoint) in candidates.enumerated() {
                group.addTask { (index, try? await http.hello(at: endpoint)) }
            }
            var answers = [DirectMacIdentity?](repeating: nil, count: candidates.count)
            for await (index, identity) in group { answers[index] = identity }
            return answers
        }
        try Task.checkCancellation()
        guard network() == startedOn, generation == startedGeneration else {
            current = nil; validated = nil; failedAt = nil
            throw DirectRouteError.unreachable
        }
        var changedOn: String?
        for (index, answer) in answers.enumerated() where answer?.macID == pinned.macID && answer?.sameKeys(as: pinned) == false {
            if proven[candidates[index]] == startedOn { changedOn = startedOn }
            rejected[candidates[index]] = (clock.now, startedOn)
        }
        guard let index = answers.firstIndex(where: { $0?.macID == pinned.macID && $0?.sameKeys(as: pinned) == true }) else {
            current = nil
            validated = nil
            failedAt = (clock.now, startedOn)
            if changedOn != nil { identityChangedOn = changedOn }
            throw unreachable
        }
        identityChangedOn = nil
        current = candidates[index]
        validated = (clock.now, startedOn)
        failedAt = nil
        return candidates[index]
    }

    private var unreachable: DirectRouteError {
        identityChangedOn != nil && identityChangedOn == network() ? .identityChanged : .unreachable
    }

    private static func unique(_ endpoints: [DirectEndpoint]) -> [DirectEndpoint] {
        var seen = Set<DirectEndpoint>()
        return endpoints.filter { seen.insert($0).inserted }
    }
}
