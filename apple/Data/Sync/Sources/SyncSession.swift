import AlondraAPIClient
import AlondraContracts
import Foundation

/// The caller must change `credentialRevision` when a signing key changes, including rotation
/// of a key whose device ID stays the same. It is local identity metadata, never sent on the wire.
public struct SyncIdentity: Hashable, Sendable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String

    public init(server: URL, deviceID: String, credentialRevision: String) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
    }
}

public struct SyncSnapshot: Equatable, Sendable {
    public let tick: UInt64
    public let body: Data

    public init(body: Data) throws {
        struct Header: Decodable { let tick: UInt64 }
        tick = try JSONDecoder().decode(Header.self, from: body).tick
        self.body = body
    }
}

public enum SyncConnection: Equatable, Sendable {
    case connecting
    case online
    case offline(ConnectionProblem)
    case invalidConfiguration(String)
}

/// A complete renderable value. Every emission identifies the credential generation that owns
/// its snapshot, so a slow subscriber can safely discard intermediate buffered states.
public struct SyncState: Equatable, Sendable {
    public let identity: SyncIdentity
    public let generation: UInt64
    /// Changes once per session publication, even when an overview has identical contents.
    /// Scene subscribers observe the same revision for the same fetch.
    public let publicationRevision: UInt64
    public let connection: SyncConnection
    public let snapshot: SyncSnapshot?

    public init(identity: SyncIdentity, generation: UInt64, connection: SyncConnection, snapshot: SyncSnapshot?, publicationRevision: UInt64 = 0) {
        self.identity = identity
        self.generation = generation
        self.publicationRevision = publicationRevision
        self.connection = connection
        self.snapshot = snapshot
    }
}

/// One poll loop for every subscriber of a server/device/credential identity.
/// UI windows own their presentation state; this actor owns only transport and the last snapshot.
public actor SyncSession {
    /// The daemon's tick is process-local. A bounded wait forces a new overview after restart
    /// even when the new daemon never reaches the previous tick.
    private static let changeWaitSeconds = 10

    private var transport: any SyncTransport
    private var classify: @Sendable (any Error) -> ConnectionProblem
    public var identity: SyncIdentity { state.identity }
    public private(set) var state: SyncState
    private let backoff: ConnectionBackoff
    private let sleep: @Sendable (Duration) async throws -> Void
    private let random: @Sendable () -> Double
    private var failures = 0
    private var generation: UInt64 = 0
    private var publicationRevision: UInt64 = 0
    private var loop: Task<Void, Never>?
    private var subscribers: [UUID: AsyncStream<SyncState>.Continuation] = [:]

    public init(transport: any SyncTransport, identity: SyncIdentity,
                backoff: ConnectionBackoff = .standard, initialInvalidConfiguration: String? = nil,
                classify: @escaping @Sendable (any Error) -> ConnectionProblem = { _ in .unexpected },
                sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) },
                random: @escaping @Sendable () -> Double = { Double.random(in: 0...1) }) {
        self.transport = transport
        self.classify = classify
        self.sleep = sleep
        self.random = random
        self.state = SyncState(
            identity: identity,
            generation: 0,
            connection: initialInvalidConfiguration.map(SyncConnection.invalidConfiguration) ?? .connecting,
            snapshot: nil
        )
        self.backoff = backoff
    }

    public func subscribe() -> AsyncStream<SyncState> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<SyncState>.makeStream(bufferingPolicy: .bufferingNewest(1))
        subscribers[id] = continuation
        continuation.yield(state)
        continuation.onTermination = { [weak self] _ in
            Task { await self?.removeSubscriber(id) }
        }
        startIfNeeded()
        return stream
    }

    /// Replaces the service transport and invalidates in-flight reads under an explicit identity.
    public func use(transport next: any SyncTransport, identity: SyncIdentity,
                    classify: @escaping @Sendable (any Error) -> ConnectionProblem = { _ in .unexpected }) {
        generation &+= 1
        loop?.cancel()
        loop = nil
        failures = 0
        transport = next
        self.classify = classify
        publish(SyncState(identity: identity, generation: generation, connection: .connecting, snapshot: nil))
        startIfNeeded()
    }

    /// Keep scene subscriptions alive while a saved server address is invalid. A later `use`
    /// resumes the same subscribers with a new identity; no request reaches the old server.
    public func suspendForInvalidConfiguration(_ reason: String) {
        generation &+= 1
        loop?.cancel()
        loop = nil
        publish(SyncState(identity: identity, generation: generation, connection: .invalidConfiguration(reason), snapshot: nil))
    }

    /// Requests a fresh snapshot without discarding the visible workspace or its identity.
    public func refresh() {
        guard !subscribers.isEmpty else { return }
        if case .invalidConfiguration = state.connection { return }
        generation &+= 1
        loop?.cancel()
        loop = nil
        startIfNeeded()
    }

    /// Skips the rest of the current backoff wait, for when the network returns or the app comes
    /// back to the foreground. A healthy session keeps its long poll.
    public func retryNow() {
        guard case .offline = state.connection else { return }
        refresh()
    }

    /// Ends all subscriptions and cancels the active long poll.
    public func stop() {
        generation &+= 1
        loop?.cancel()
        loop = nil
        for continuation in subscribers.values { continuation.finish() }
        subscribers.removeAll()
        state = stamped(SyncState(identity: identity, generation: generation, connection: .connecting, snapshot: state.snapshot))
    }

    private func removeSubscriber(_ id: UUID) {
        subscribers.removeValue(forKey: id)
        guard subscribers.isEmpty else { return }
        generation &+= 1
        loop?.cancel()
        loop = nil
        let connection: SyncConnection
        if case .invalidConfiguration = state.connection {
            connection = state.connection
        } else {
            connection = .connecting
        }
        state = stamped(SyncState(identity: identity, generation: generation, connection: connection, snapshot: state.snapshot))
    }

    private func startIfNeeded() {
        guard !subscribers.isEmpty, loop == nil else { return }
        if case .invalidConfiguration = state.connection { return }
        let currentGeneration = generation
        let currentTransport = transport
        let currentClassify = classify
        loop = Task { [weak self] in
            await self?.run(transport: currentTransport, generation: currentGeneration, classify: currentClassify)
        }
    }

    private func publish(_ next: SyncState) {
        let published = stamped(next)
        state = published
        for continuation in subscribers.values { continuation.yield(published) }
    }

    private func stamped(_ next: SyncState) -> SyncState {
        publicationRevision &+= 1
        return SyncState(
            identity: next.identity,
            generation: next.generation,
            connection: next.connection,
            snapshot: next.snapshot,
            publicationRevision: publicationRevision
        )
    }

    private func publish(_ connection: SyncConnection, snapshot: SyncSnapshot? = nil) {
        publish(SyncState(
            identity: identity,
            generation: generation,
            connection: connection,
            snapshot: snapshot ?? state.snapshot
        ))
    }

    private func run(transport: any SyncTransport, generation current: UInt64,
                     classify: @Sendable (any Error) -> ConnectionProblem) async {
        while !Task.isCancelled && generation == current && !subscribers.isEmpty {
            do {
                // Always fetch the complete state after a change, reconnection, or daemon restart.
                let body = try await transport.fetchOverview()
                let snapshot = try SyncSnapshot(body: body)
                guard !Task.isCancelled, generation == current else { return }
                failures = 0
                publish(.online, snapshot: snapshot)
                do {
                    try await transport.waitForChange(afterSnapshotTick: snapshot.tick,
                                                       seconds: Self.changeWaitSeconds)
                } catch let error as URLError where error.code == .timedOut {
                    // A timeout is a normal empty poll; re-fetch to detect daemon restarts.
                }
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled, generation == current else { return }
                publish(.offline(classify(error)))
                let delay = backoff.delay(attempt: failures, random: random())
                failures += 1
                do { try await sleep(delay) } catch { return }
            }
        }
    }
}
