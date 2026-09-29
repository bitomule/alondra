import AlondraAPIClient
import AlondraContracts
import Foundation

/// The read seam for the whole app: sectioned reads (docs/domain-v2-read-endpoints.md) that
/// DomainSections assembles into one view. The cursor is (data_epoch, revision).
public protocol DomainSyncTransport: Sendable {
    func domainSummary() async throws -> DomainV2.Summary
    func domainPage<Row: Decodable & Sendable & Equatable>(
        _ section: DomainV2.Section, of row: Row.Type, cursor: String?, pendingOnly: Bool
    ) async throws -> DomainV2.Page<Row>
    func domainInteraction(_ id: DomainV2.InteractionId) async throws -> DomainV2.Item<DomainV2.Interaction>
    func domainRequest(_ id: DomainV2.RequestId) async throws -> DomainV2.Item<DomainV2.Request>
    func domainChanges(epoch: DomainV2.DataEpoch, after revision: DomainV2.Revision,
                       wait seconds: UInt) async throws -> DomainV2.Changes
    /// Machine telemetry and agent activity: a daemon read outside the domain.
    func domainRuntime() async throws -> DomainV2.Runtime
}

extension AlondraAPIClient: DomainSyncTransport {}

/// What the running screens need. A session starts with the inbox only and grows this as screens
/// open; it never shrinks, so a section a screen once asked for stays fresh.
public struct DomainSyncDemand: Equatable, Sendable {
    public var sections: Set<DomainV2.Section> = [.inbox]
    public var runtime = false

    public init(sections: Set<DomainV2.Section> = [.inbox], runtime: Bool = false) {
        self.sections = sections
        self.runtime = runtime
    }

    public static let everything = DomainSyncDemand(sections: DomainSections.everything, runtime: true)
}

public struct DomainSyncState: Equatable, Sendable {
    public let identity: SyncIdentity
    public let generation: UInt64
    public let publicationRevision: UInt64
    public let connection: SyncConnection
    public let projection: DomainV2.DomainProjection?
    /// Telemetry and remote observations; nil until the Mac serves them.
    public let runtime: DomainV2.Runtime?
}

/// One subscription per identity. It starts with the first subscriber and republishes the
/// projection on every server revision and the runtime on every idle wait.
public actor DomainSyncSession {
    private var transport: any DomainSyncTransport
    private let backoff: ConnectionBackoff
    private let classify: @Sendable (any Error) -> ConnectionProblem
    private let random: @Sendable () -> Double
    private var failures = 0
    private var generation: UInt64 = 0
    private var publicationRevision: UInt64 = 0
    private var loop: Task<Void, Never>?
    private var subscribers: [UUID: AsyncStream<DomainSyncState>.Continuation] = [:]
    private var sections = DomainSections()
    private var demand = DomainSyncDemand()
    private var idle = false
    public private(set) var state: DomainSyncState

    public init(transport: any DomainSyncTransport, identity: SyncIdentity,
                backoff: ConnectionBackoff = .standard,
                classify: @escaping @Sendable (any Error) -> ConnectionProblem = { _ in .unexpected },
                random: @escaping @Sendable () -> Double = { Double.random(in: 0...1) }) {
        self.transport = transport
        self.backoff = backoff
        self.classify = classify
        self.random = random
        state = DomainSyncState(identity: identity, generation: 0,
                                publicationRevision: 0, connection: .connecting, projection: nil, runtime: nil)
    }

    public func subscribe() -> AsyncStream<DomainSyncState> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<DomainSyncState>.makeStream(bufferingPolicy: .bufferingNewest(1))
        subscribers[id] = continuation
        continuation.yield(state)
        continuation.onTermination = { [weak self] _ in Task { await self?.removeSubscriber(id) } }
        startIfNeeded()
        return stream
    }

    public func use(transport: any DomainSyncTransport, identity: SyncIdentity) {
        generation &+= 1
        loop?.cancel()
        loop = nil
        self.transport = transport
        sections = DomainSections()
        publish(identity: identity, connection: .connecting, projection: nil)
        startIfNeeded()
    }

    /// Adds what a screen shows to what the session keeps in sync. While the first load is still
    /// running the request waits for it; afterwards the idle long poll ends so the new sections are
    /// read now.
    public func require(_ needed: DomainSyncDemand) {
        var next = demand
        next.sections.formUnion(needed.sections)
        next.runtime = next.runtime || needed.runtime
        guard next != demand else { return }
        demand = next
        if idle { refresh() }
    }

    /// Skips the rest of the current backoff wait, for when the network returns or the app comes
    /// back to the foreground. A healthy session keeps its long poll.
    public func retryNow() {
        guard case .offline = state.connection else { return }
        refresh()
    }

    public func refresh() {
        guard !subscribers.isEmpty else { return }
        generation &+= 1
        loop?.cancel()
        loop = nil
        startIfNeeded()
    }

    public func stop() {
        generation &+= 1
        loop?.cancel()
        loop = nil
        for continuation in subscribers.values { continuation.finish() }
        subscribers.removeAll()
        publish(identity: state.identity, connection: .connecting, projection: nil)
    }

    private func removeSubscriber(_ id: UUID) {
        subscribers.removeValue(forKey: id)
        guard subscribers.isEmpty else { return }
        generation &+= 1
        loop?.cancel()
        loop = nil
    }

    private func startIfNeeded() {
        guard !subscribers.isEmpty, loop == nil else { return }
        let currentGeneration = generation
        let currentTransport = transport
        loop = Task { [weak self] in
            await self?.run(transport: currentTransport, generation: currentGeneration)
        }
    }

    private func publish(identity: SyncIdentity, connection: SyncConnection,
                         projection: DomainV2.DomainProjection?, runtime: DomainV2.Runtime? = nil) {
        publicationRevision &+= 1
        let runtime = projection == nil ? nil : runtime
        state = DomainSyncState(identity: identity, generation: generation,
                                publicationRevision: publicationRevision,
                                connection: connection, projection: projection, runtime: runtime)
        for continuation in subscribers.values { continuation.yield(state) }
    }

    private func nextDelay() -> Duration {
        defer { failures += 1 }
        return backoff.delay(attempt: failures, random: random())
    }

    private func run(transport: any DomainSyncTransport, generation current: UInt64) async {
        while !Task.isCancelled && generation == current && !subscribers.isEmpty {
            idle = false
            do {
                let wanted = demand
                let summary = try await transport.domainSummary()
                guard !Task.isCancelled, generation == current else { return }
                var next = sections
                let projection = try await next.refresh(to: summary, from: transport, demand: wanted.sections)
                guard !Task.isCancelled, generation == current else { return }
                sections = next
                failures = 0
                var runtime = wanted.runtime ? await Self.runtime(transport) ?? state.runtime : state.runtime
                guard !Task.isCancelled, generation == current else { return }
                publish(identity: state.identity, connection: .online, projection: projection, runtime: runtime)
                idle = true
                while !Task.isCancelled && generation == current && !subscribers.isEmpty && demand == wanted {
                    let changes = try await transport.domainChanges(
                        epoch: projection.dataEpoch, after: projection.revision, wait: 10)
                    guard !Task.isCancelled, generation == current else { return }
                    if changes.changed || changes.revision != projection.revision
                        || changes.sections.contains(where: { summary.sections[$0.key]?.revision != $0.value }) { break }
                    // Telemetry moves without a domain revision; refresh it on each idle wait.
                    if wanted.runtime, let fresh = await Self.runtime(transport), fresh != runtime {
                        guard !Task.isCancelled, generation == current else { return }
                        runtime = fresh
                        publish(identity: state.identity, connection: .online, projection: projection, runtime: fresh)
                    }
                }
            } catch is CancellationError {
                return
            } catch let error as DomainV2.DomainError where error.code == "stale_epoch" {
                guard !Task.isCancelled, generation == current else { return }
                // The next summary establishes the new epoch and every section is read again.
                sections = DomainSections()
                publish(identity: state.identity, connection: .connecting, projection: nil)
            } catch let error as DomainV2.DomainError where error.code == "incompatible_protocol" {
                guard !Task.isCancelled, generation == current else { return }
                // A mismatched 200 response cannot keep an old projection visible.
                sections = DomainSections()
                publish(identity: state.identity, connection: .offline(.unexpected), projection: nil)
                do { try await Task.sleep(for: nextDelay()) } catch { return }
            } catch {
                idle = false
                guard !Task.isCancelled, generation == current else { return }
                publish(identity: state.identity, connection: .offline(classify(error)),
                        projection: state.projection, runtime: state.runtime)
                do { try await Task.sleep(for: nextDelay()) } catch { return }
            }
        }
    }

    /// The runtime is display-only; a daemon that does not serve it yet leaves it empty.
    private static func runtime(_ transport: any DomainSyncTransport) async -> DomainV2.Runtime? {
        try? await transport.domainRuntime()
    }
}
