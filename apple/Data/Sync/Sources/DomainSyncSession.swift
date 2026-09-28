import AlondraAPIClient
import AlondraContracts
import Foundation

/// The v2 domain cursor is (data_epoch, revision), independent of the v1
/// process-local overview tick. No v1 projection is synthesized from it.
public protocol DomainSyncTransport: Sendable {
    func domainProjection() async throws -> DomainV2.DomainProjection
    func domainChanges(epoch: DomainV2.DataEpoch, after revision: DomainV2.Revision,
                       wait seconds: UInt) async throws -> DomainV2.Changes
}

extension AlondraAPIClient: DomainSyncTransport {}

public struct DomainSyncState: Equatable, Sendable {
    public let identity: SyncIdentity
    public let generation: UInt64
    public let publicationRevision: UInt64
    public let connection: SyncConnection
    public let projection: DomainV2.DomainProjection?
}

/// A data-only v2 subscription. It starts on first subscription and never
/// changes the UI-facing v1 SyncSession or its mutation routes.
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
                                publicationRevision: 0, connection: .connecting, projection: nil)
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
                         projection: DomainV2.DomainProjection?) {
        publicationRevision &+= 1
        state = DomainSyncState(identity: identity, generation: generation,
                                publicationRevision: publicationRevision,
                                connection: connection, projection: projection)
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
                publish(identity: state.identity, connection: .online, projection: projection)
                while !Task.isCancelled && generation == current && !subscribers.isEmpty {
                    let changes = try await transport.domainChanges(
                        epoch: projection.dataEpoch, after: projection.revision, wait: 10)
                    guard !Task.isCancelled, generation == current else { return }
                    if changes.changed || changes.revision != projection.revision { break }
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
                        projection: state.projection)
                do { try await Task.sleep(for: nextDelay()) } catch { return }
            }
        }
    }
}
