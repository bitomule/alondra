import AlondraContract
import Foundation

public enum DirectRouteError: Error, Equatable, Sendable {
    /// No known address answers as this Mac from the current network.
    case unreachable
    /// An address answers with this Mac's ID but other keys. Never trusted silently.
    case identityChanged
}

/// Picks the address one paired Mac answers on: LAN, then the tailnet IP, then the tailnet name.
/// An address counts only after `hello` there returns the pinned Mac ID and both pinned keys.
public actor DirectRouter {
    private let pinned: DirectMacIdentity
    private let http: DirectHTTP
    private let freshness: Duration
    private let retryAfterFailure: Duration
    private let clock = ContinuousClock()
    private var candidates: [DirectEndpoint]
    private var current: DirectEndpoint?
    private var validatedAt: ContinuousClock.Instant?
    private var failedAt: ContinuousClock.Instant?
    private var identityChanged = false
    private var probing: Task<DirectEndpoint, any Error>?

    public init(pinned: DirectMacIdentity, candidates: [DirectEndpoint], http: DirectHTTP,
                freshness: Duration = .seconds(60), retryAfterFailure: Duration = .seconds(15)) {
        self.pinned = pinned
        self.http = http
        self.freshness = freshness
        self.retryAfterFailure = retryAfterFailure
        self.candidates = Self.ordered(candidates)
    }

    public var known: [DirectEndpoint] { candidates }

    /// The route in use, or nil when no address has answered as this Mac yet.
    public var route: ConnectionRoute? { current?.route }

    /// Adds addresses learned from Bonjour or `/routes`. They are tried on the next probe.
    public func offer(_ endpoints: [DirectEndpoint]) {
        let merged = Self.ordered(candidates + endpoints.filter(\.isValid))
        guard merged != candidates else { return }
        candidates = merged
        failedAt = nil
        if let current, merged.first.map({ $0.rank < current.rank }) == true { validatedAt = nil }
    }

    public func endpoint() async throws -> DirectEndpoint {
        if identityChanged { throw DirectRouteError.identityChanged }
        if let current, let validatedAt, validatedAt.duration(to: clock.now) < freshness { return current }
        // Away from home every request would otherwise wait for the probes before the relay.
        if let failedAt, failedAt.duration(to: clock.now) < retryAfterFailure { throw DirectRouteError.unreachable }
        if let probing { return try await probing.value }
        let task = Task { try await self.probe() }
        probing = task
        defer { probing = nil }
        return try await task.value
    }

    /// The last address stopped answering, or the network changed: probe again on next use.
    public func invalidate() {
        validatedAt = nil
        failedAt = nil
    }

    private func probe() async throws -> DirectEndpoint {
        let candidates = candidates
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
        if answers.contains(where: { $0?.macID == pinned.macID && $0?.sameKeys(as: pinned) == false }) {
            identityChanged = true
            current = nil
            throw DirectRouteError.identityChanged
        }
        guard let index = answers.firstIndex(where: { $0?.macID == pinned.macID }) else {
            current = nil
            validatedAt = nil
            failedAt = clock.now
            throw DirectRouteError.unreachable
        }
        current = candidates[index]
        validatedAt = clock.now
        failedAt = nil
        return candidates[index]
    }

    private static func ordered(_ endpoints: [DirectEndpoint]) -> [DirectEndpoint] {
        var seen = Set<DirectEndpoint>()
        return endpoints.filter { seen.insert($0).inserted }.enumerated()
            .sorted { ($0.element.rank, $0.offset) < ($1.element.rank, $1.offset) }.map(\.element)
    }
}
