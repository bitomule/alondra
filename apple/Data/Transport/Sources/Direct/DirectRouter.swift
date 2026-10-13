import AlondraContract
import Foundation
import Synchronization

public enum DirectRouteError: Error, Equatable, Sendable {
    /// No address proved to be this Mac from the current network.
    case unreachable
    /// Like `unreachable`, and an address already proved on this network now answers as the same
    /// Mac ID with other keys. Only a hint: nothing the app trusts changes until the person pairs again.
    case identityChanged
    /// Like `unreachable`, and the system refused a local address because Local Network access is
    /// off for this app.
    case localNetworkDenied
}

extension URLError {
    /// iOS refuses a local address with -1009 and a path that says why when Local Network access
    /// is off; the code alone reads as "no internet".
    var isLocalNetworkDenied: Bool {
        code == .notConnectedToInternet
            && userInfo["_NSURLErrorNWPathKey"].map { String(describing: $0).localizedCaseInsensitiveContains("local network prohibited") } == true
    }
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
enum DirectProbeObservation: Sendable, Equatable {
    case answered(DirectEndpoint), waitingForProbe, waitingForDrain, joiningProbe
}

private struct DirectProbeSelection: Sendable {
    let endpoint: DirectEndpoint
    let network: String
    let generation: UInt64
    let validationGeneration: UInt64
}

public actor DirectRouter {
    public enum Source: Sendable { case saved, mac, bonjour }

    struct RequestContext: Sendable {
        fileprivate let network: String
        fileprivate let generation: UInt64
        fileprivate let validationGeneration: UInt64
    }

    func requestContext() -> RequestContext {
        RequestContext(network: network(), generation: generation, validationGeneration: validationGeneration)
    }

    func proved(_ endpoint: DirectEndpoint, in context: RequestContext, response: APIResponse, lifetime: DirectSessionLifetime) {
        guard context.generation == generation, context.network == network() else { return }
        proven[endpoint] = context.network
        rejected[endpoint] = nil
        guard case .completed(status: 200, body: _) = response else { return }
        lifetime.whileActive {
            guard !Task.isCancelled, context.validationGeneration == validationGeneration,
                  current == endpoint, let validated, validated.network == context.network else { return }
            self.validated = (now(), validated.network)
        }
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

    private let probeObserved: (@Sendable (DirectProbeObservation) -> Void)?
    private let pinned: DirectMacIdentity
    private let http: DirectHTTP
    private let network: @Sendable () -> String
    private let freshness: Duration
    private let retryAfterFailure: Duration
    private let now: @Sendable () -> ContinuousClock.Instant
    private var saved: [DirectEndpoint]
    private var fromMac: [DirectEndpoint] = []
    private var fromBonjour: [DirectEndpoint] = []
    /// The network each address was last proved on.
    private var proven: [DirectEndpoint: String] = [:]
    private var rejected: [DirectEndpoint: (at: ContinuousClock.Instant, network: String)] = [:]
    private var current: DirectEndpoint?
    private var validated: (at: ContinuousClock.Instant, network: String)?
    private var failedAt: (at: ContinuousClock.Instant, network: String)?
    private var probing: Task<Void, Never>?
    private var probeResult: DirectProbeResult?
    private var probeDrained: DirectProbeResult?
    private var probeResolved = false
    private var probePreference: Int?
    private var generation: UInt64 = 0
    private var validationGeneration: UInt64 = 0
    private var identityChangedOn: String?
    private var localNetworkDeniedOn: String?

    /// `saved` are addresses a sealed answer proved before, on some network; `network` names the
    /// current one.
    public init(pinned: DirectMacIdentity, saved: [DirectEndpoint], http: DirectHTTP,
                network: @escaping @Sendable () -> String = { "" },
                freshness: Duration = .seconds(60), retryAfterFailure: Duration = .seconds(15),
                now: @escaping @Sendable () -> ContinuousClock.Instant = { ContinuousClock().now }) {
        self.init(pinned: pinned, saved: saved, http: http, network: network, freshness: freshness,
                  retryAfterFailure: retryAfterFailure, now: now, probeObserved: nil)
    }

    init(pinned: DirectMacIdentity, saved: [DirectEndpoint], http: DirectHTTP,
         network: @escaping @Sendable () -> String, freshness: Duration = .seconds(60),
         retryAfterFailure: Duration = .seconds(15),
         now: @escaping @Sendable () -> ContinuousClock.Instant,
         probeObserved: (@Sendable (DirectProbeObservation) -> Void)?) {
        self.probeObserved = probeObserved
        self.pinned = pinned
        self.http = http
        self.network = network
        self.freshness = freshness
        self.retryAfterFailure = retryAfterFailure
        self.now = now
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
        if let preference = current?.rank ?? probePreference, candidates.first.map({ $0.rank < preference }) == true { discardValidation() }
    }

    public func endpoint() async throws -> DirectEndpoint {
        if let current, let validated, validated.network == network(),
           validated.at.duration(to: now()) < freshness { return current }
        // Away from home every request would otherwise wait for the probes.
        if let failedAt, failedAt.network == network(), failedAt.at.duration(to: now()) < retryAfterFailure { throw unreachable }
        try Task.checkCancellation()
        if probing != nil {
            if probeResolved, let probeDrained {
                probeObserved?(.waitingForDrain)
                do { _ = try await probeDrained.value() }
                catch { try Task.checkCancellation() }
                return try await endpoint()
            }
            guard let probeResult else { throw unreachable }
            probeObserved?(.joiningProbe)
            return try await use(try await probeResult.value())
        }
        let observer = probeObserved
        let result = DirectProbeResult(registered: { observer?(.waitingForProbe) })
        let drained = DirectProbeResult()
        probeResult = result
        probeDrained = drained
        probePreference = candidates.first?.rank
        let task = MutationScope.$current.withValue(nil) { Task {
            let outcome: Result<DirectProbeSelection, any Error>
            do { outcome = .success(try await self.probe(result)) }
            catch { outcome = .failure(error) }
            result.complete(outcome)
            self.probing = nil
            self.probeResult = nil
            self.probeDrained = nil
            self.probeResolved = false
            self.probePreference = nil
            drained.complete(outcome)
        } }
        probing = task
        return try await use(try await result.value())
    }

    private func use(_ selection: DirectProbeSelection) async throws -> DirectEndpoint {
        try Task.checkCancellation()
        guard selection.network == network(), selection.generation == generation,
              selection.validationGeneration == validationGeneration, current == selection.endpoint else {
            return try await endpoint()
        }
        return selection.endpoint
    }

    var probeWaiterCount: Int { probeResult?.waiterCount ?? 0 }

    func drainProbes() async {
        if let probing { await probing.value }
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
        rejected[endpoint] = (now(), rejectedOn)
        if proven[endpoint] == rejectedOn { proven[endpoint] = nil }
        if current == endpoint {
            current = nil
            discardValidation()
        }
    }

    /// The last address stopped answering, or the network changed: probe again on next use.
    public func invalidate() {
        generation &+= 1
        discardValidation()
        failedAt = nil
    }

    private func discardValidation() {
        validationGeneration &+= 1
        validated = nil
    }

    private var candidates: [DirectEndpoint] {
        Self.unique(saved + fromMac + fromBonjour).enumerated()
            .sorted { ($0.element.rank, $0.offset) < ($1.element.rank, $1.offset) }.map(\.element)
    }

    private func probe(_ result: DirectProbeResult) async throws -> DirectProbeSelection {
        let startedOn = network()
        let startedGeneration = generation
        let startedValidationGeneration = validationGeneration
        let observedAt = now()
        rejected = rejected.filter { $0.value.network == startedOn && $0.value.at.duration(to: observedAt) < Self.rejectionTime }
        let candidates = candidates.filter { rejected[$0] == nil }
        probePreference = candidates.first?.rank
        let http = http
        let selected = await withTaskGroup(of: (Int, DirectMacIdentity?, Bool).self) { group in
            for (index, endpoint) in candidates.enumerated() {
                group.addTask {
                    let identity: DirectMacIdentity
                    do { identity = try await http.hello(at: endpoint) }
                    catch { return (index, nil, (error as? URLError)?.isLocalNetworkDenied == true) }
                    return (index, identity, false)
                }
            }
            var answers = [DirectMacIdentity?](repeating: nil, count: candidates.count)
            var finished = [Bool](repeating: false, count: candidates.count)
            var denied = false
            for await (index, identity, refused) in group {
                answers[index] = identity
                denied = denied || refused
                finished[index] = true
                if let winner = answers.firstIndex(where: { $0?.macID == pinned.macID && $0?.sameKeys(as: pinned) == true }),
                   finished[..<winner].allSatisfy({ $0 }) {
                    let selection = Result { try select(candidates: candidates, answers: answers, denied: denied,
                        startedOn: startedOn, generation: startedGeneration,
                        validationGeneration: startedValidationGeneration) }
                    probeResolved = true
                    result.complete(selection)
                    probeObserved?(.answered(candidates[index]))
                    group.cancelAll()
                    return selection
                }
                probeObserved?(.answered(candidates[index]))
            }
            return Result { try select(candidates: candidates, answers: answers, denied: denied,
                startedOn: startedOn, generation: startedGeneration,
                validationGeneration: startedValidationGeneration) }
        }
        return try selected.get()
    }

    private func select(candidates: [DirectEndpoint], answers: [DirectMacIdentity?], denied: Bool,
                        startedOn: String, generation startedGeneration: UInt64,
                        validationGeneration startedValidationGeneration: UInt64) throws -> DirectProbeSelection {
        try Task.checkCancellation()
        guard network() == startedOn, generation == startedGeneration,
              validationGeneration == startedValidationGeneration else {
            if validationGeneration == startedValidationGeneration {
                current = nil
                discardValidation()
                failedAt = nil
            }
            throw DirectRouteError.unreachable
        }
        var changedOn: String?
        for (index, answer) in answers.enumerated() where answer?.macID == pinned.macID && answer?.sameKeys(as: pinned) == false {
            if proven[candidates[index]] == startedOn { changedOn = startedOn }
            rejected[candidates[index]] = (now(), startedOn)
        }
        guard let index = answers.firstIndex(where: { $0?.macID == pinned.macID && $0?.sameKeys(as: pinned) == true }) else {
            current = nil
            discardValidation()
            failedAt = (now(), startedOn)
            if changedOn != nil { identityChangedOn = changedOn }
            localNetworkDeniedOn = denied ? startedOn : nil
            throw unreachable
        }
        identityChangedOn = nil
        localNetworkDeniedOn = nil
        current = candidates[index]
        validationGeneration &+= 1
        validated = (now(), startedOn)
        failedAt = nil
        return DirectProbeSelection(endpoint: candidates[index], network: startedOn, generation: startedGeneration,
                                    validationGeneration: validationGeneration)
    }

    private var unreachable: DirectRouteError {
        if identityChangedOn != nil && identityChangedOn == network() { return .identityChanged }
        if localNetworkDeniedOn != nil && localNetworkDeniedOn == network() { return .localNetworkDenied }
        return .unreachable
    }

    private static func unique(_ endpoints: [DirectEndpoint]) -> [DirectEndpoint] {
        var seen = Set<DirectEndpoint>()
        return endpoints.filter { seen.insert($0).inserted }
    }
}

private final class DirectProbeResult: Sendable {
    private struct State {
        var result: Result<DirectProbeSelection, any Error>?
        var waiters: [UUID: CheckedContinuation<DirectProbeSelection, any Error>] = [:]
    }
    private let state = Mutex(State())
    private let registered: @Sendable () -> Void
    init(registered: @escaping @Sendable () -> Void = {}) { self.registered = registered }
    var waiterCount: Int { state.withLock { $0.waiters.count } }

    func value() async throws -> DirectProbeSelection {
        let id = UUID()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                let result: Result<DirectProbeSelection, any Error>? = state.withLock { state -> Result<DirectProbeSelection, any Error>? in
                    if Task.isCancelled { return .failure(CancellationError()) }
                    if let result = state.result { return result }
                    state.waiters[id] = continuation
                    return nil as Result<DirectProbeSelection, any Error>?
                }
                if let result { continuation.resume(with: result) }
                else { registered() }
            }
        } onCancel: {
            let waiter = self.state.withLock { $0.waiters.removeValue(forKey: id) }
            waiter?.resume(throwing: CancellationError())
        }
    }

    func complete(_ result: Result<DirectProbeSelection, any Error>) {
        let waiters = state.withLock { state in
            guard state.result == nil else { return [CheckedContinuation<DirectProbeSelection, any Error>]() }
            state.result = result
            let waiters = Array(state.waiters.values)
            state.waiters.removeAll()
            return waiters
        }
        for waiter in waiters { waiter.resume(with: result) }
    }
}
