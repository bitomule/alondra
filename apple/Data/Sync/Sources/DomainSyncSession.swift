import AlondraAPIClient
import AlondraContracts
import Foundation

/// The read seam for the whole app: every screen reads what this transport returns, so moving
/// from one projection to sectioned reads changes only its implementation. The cursor is
/// (data_epoch, revision).
public protocol DomainSyncTransport: Sendable {
    func domainProjection() async throws -> DomainV2.DomainProjection
    func domainChanges(epoch: DomainV2.DataEpoch, after revision: DomainV2.Revision,
                       wait seconds: UInt) async throws -> DomainV2.Changes
    func domainRuntime(epoch: DomainV2.DataEpoch) async throws -> DomainV2.Runtime
}

extension AlondraAPIClient: DomainSyncTransport {}

public struct DomainSyncState: Equatable, Sendable {
    public let identity: SyncIdentity
    public let generation: UInt64
    public let publicationRevision: UInt64
    public let connection: SyncConnection
    public let projection: DomainV2.DomainProjection?
    /// Telemetry and remote observations of the same epoch; nil until the Mac serves them.
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
        publish(identity: identity, connection: .connecting, projection: nil)
        startIfNeeded()
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
        let runtime = projection.flatMap { projection in runtime?.dataEpoch == projection.dataEpoch ? runtime : nil }
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
            do {
                let projection = try await transport.domainProjection()
                guard !Task.isCancelled, generation == current else { return }
                if let previous = state.projection,
                   previous.dataEpoch == projection.dataEpoch,
                   projection.revision < previous.revision {
                    throw DomainV2.DomainError(code: "incompatible_protocol",
                        message: "Domain revision moved backwards within one epoch",
                        currentRevision: projection.revision, subject: nil)
                }
                failures = 0
                var runtime = await Self.runtime(transport, projection.dataEpoch) ?? state.runtime
                guard !Task.isCancelled, generation == current else { return }
                publish(identity: state.identity, connection: .online, projection: projection, runtime: runtime)
                while !Task.isCancelled && generation == current && !subscribers.isEmpty {
                    let changes = try await transport.domainChanges(
                        epoch: projection.dataEpoch, after: projection.revision, wait: 10)
                    guard !Task.isCancelled, generation == current else { return }
                    if changes.changed || changes.revision != projection.revision { break }
                    // Telemetry moves without a domain revision; refresh it on each idle wait.
                    if let fresh = await Self.runtime(transport, projection.dataEpoch), fresh != runtime {
                        guard !Task.isCancelled, generation == current else { return }
                        runtime = fresh
                        publish(identity: state.identity, connection: .online, projection: projection, runtime: fresh)
                    }
                }
            } catch is CancellationError {
                return
            } catch let error as DomainV2.DomainError where error.code == "stale_epoch" {
                guard !Task.isCancelled, generation == current else { return }
                // The next complete projection establishes the new epoch.
                publish(identity: state.identity, connection: .connecting, projection: nil)
            } catch let error as DomainV2.DomainError where error.code == "incompatible_protocol" {
                guard !Task.isCancelled, generation == current else { return }
                // A mismatched 200 response cannot keep an old projection visible.
                publish(identity: state.identity, connection: .offline(.unexpected), projection: nil)
                do { try await Task.sleep(for: nextDelay()) } catch { return }
            } catch {
                guard !Task.isCancelled, generation == current else { return }
                publish(identity: state.identity, connection: .offline(classify(error)),
                        projection: state.projection, runtime: state.runtime)
                do { try await Task.sleep(for: nextDelay()) } catch { return }
            }
        }
    }

    /// The runtime is display-only; a daemon that does not serve it yet leaves it empty.
    private static func runtime(_ transport: any DomainSyncTransport, _ epoch: DomainV2.DataEpoch) async -> DomainV2.Runtime? {
        try? await transport.domainRuntime(epoch: epoch)
    }
}
