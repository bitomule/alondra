import AlondraContract
import Foundation

public enum DirectRouteError: Error, Equatable, Sendable {
    /// No known address answered as this Mac from the current network.
    case unreachable
    /// The only answers carried this Mac's ID with other keys. Nothing is changed by it: those
    /// addresses are skipped, the relay keeps working, and the next probe starts afresh.
    case identityChanged
}

/// Picks the address one paired Mac answers on: LAN, then the tailnet IP, then the tailnet name.
///
/// `hello` is public, so it only filters: an address that answers with other keys is skipped on
/// its own, and an address becomes trusted only after a sealed answer from it opens with the
/// pinned keys (`proved`). Only trusted addresses are ever handed out for saving.
public actor DirectRouter {
    public enum Source: Sendable { case saved, mac, bonjour }

    static let maxBonjour = 4
    static let maxFromMac = 8
    private static let rejectionTime: Duration = .seconds(600)

    private let pinned: DirectMacIdentity
    private let http: DirectHTTP
    private let freshness: Duration
    private let retryAfterFailure: Duration
    private let clock = ContinuousClock()
    private var saved: [DirectEndpoint]
    private var fromMac: [DirectEndpoint] = []
    private var fromBonjour: [DirectEndpoint] = []
    private var proven: Set<DirectEndpoint>
    private var rejected: [DirectEndpoint: ContinuousClock.Instant] = [:]
    private var current: DirectEndpoint?
    private var validatedAt: ContinuousClock.Instant?
    private var failedAt: ContinuousClock.Instant?
    private var probing: Task<DirectEndpoint, any Error>?

    /// `saved` are the addresses a sealed answer already proved, kept with the pairing.
    public init(pinned: DirectMacIdentity, saved: [DirectEndpoint], http: DirectHTTP,
                freshness: Duration = .seconds(60), retryAfterFailure: Duration = .seconds(15)) {
        self.pinned = pinned
        self.http = http
        self.freshness = freshness
        self.retryAfterFailure = retryAfterFailure
        self.saved = Self.unique(saved.filter(\.isValid))
        proven = Set(self.saved)
    }

    public var known: [DirectEndpoint] { candidates }

    /// The addresses a sealed answer proved, the only ones worth saving.
    public var provenEndpoints: [DirectEndpoint] { candidates.filter(proven.contains) }

    public var hasProvenEndpoint: Bool { !proven.isEmpty }

    public func isProven(_ endpoint: DirectEndpoint) -> Bool { proven.contains(endpoint) }

    /// The route in use, or nil when no address has answered as this Mac yet.
    public var route: ConnectionRoute? { current?.route }

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
        if let current, candidates.first.map({ $0.rank < current.rank }) == true { validatedAt = nil }
    }

    public func endpoint() async throws -> DirectEndpoint {
        if let current, let validatedAt, validatedAt.duration(to: clock.now) < freshness { return current }
        // Away from home every request would otherwise wait for the probes.
        if let failedAt, failedAt.duration(to: clock.now) < retryAfterFailure { throw DirectRouteError.unreachable }
        if let probing { return try await probing.value }
        let task = Task { try await self.probe() }
        probing = task
        defer { probing = nil }
        return try await task.value
    }

    /// A sealed answer from this address opened with the pinned keys.
    public func proved(_ endpoint: DirectEndpoint) {
        proven.insert(endpoint)
        rejected[endpoint] = nil
    }

    /// This address answered with something that is not this Mac: skip it, and only it.
    public func reject(_ endpoint: DirectEndpoint) {
        rejected[endpoint] = clock.now
        proven.remove(endpoint)
        if current == endpoint {
            current = nil
            validatedAt = nil
        }
    }

    /// The last address stopped answering, or the network changed: probe again on next use.
    public func invalidate() {
        validatedAt = nil
        failedAt = nil
    }

    private var candidates: [DirectEndpoint] {
        Self.unique(saved + fromMac + fromBonjour).enumerated()
            .sorted { ($0.element.rank, $0.offset) < ($1.element.rank, $1.offset) }.map(\.element)
    }

    private func probe() async throws -> DirectEndpoint {
        let now = clock.now
        rejected = rejected.filter { $0.value.duration(to: now) < Self.rejectionTime }
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
        var impostor = false
        for (index, answer) in answers.enumerated() where answer?.macID == pinned.macID && answer?.sameKeys(as: pinned) == false {
            rejected[candidates[index]] = clock.now
            impostor = true
        }
        guard let index = answers.firstIndex(where: { $0?.macID == pinned.macID && $0?.sameKeys(as: pinned) == true }) else {
            current = nil
            validatedAt = nil
            failedAt = clock.now
            throw impostor ? DirectRouteError.identityChanged : DirectRouteError.unreachable
        }
        current = candidates[index]
        validatedAt = clock.now
        failedAt = nil
        return candidates[index]
    }

    private static func unique(_ endpoints: [DirectEndpoint]) -> [DirectEndpoint] {
        var seen = Set<DirectEndpoint>()
        return endpoints.filter { seen.insert($0).inserted }
    }
}
