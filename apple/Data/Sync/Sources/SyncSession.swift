import AlondraAPIClient
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

public struct SyncSnapshot: Sendable {
    public let tick: UInt64
    public let body: Data

    public init(body: Data) throws {
        struct Header: Decodable { let tick: UInt64 }
        tick = try JSONDecoder().decode(Header.self, from: body).tick
        self.body = body
    }
}

public enum SyncConnection: Sendable {
    case connecting
    case online
    case offline(String)
    case unpaired
}

/// A complete renderable value. Every emission identifies the credential generation that owns
/// its snapshot, so a slow subscriber can safely discard intermediate buffered states.
public struct SyncState: Sendable {
    public let identity: SyncIdentity
    public let generation: UInt64
    public let connection: SyncConnection
    public let snapshot: SyncSnapshot?

    public init(identity: SyncIdentity, generation: UInt64, connection: SyncConnection, snapshot: SyncSnapshot?) {
        self.identity = identity
        self.generation = generation
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

    private var client: AlondraAPIClient
    public var identity: SyncIdentity { state.identity }
    public private(set) var state: SyncState
    private let retryDelay: Duration
    private var generation: UInt64 = 0
    private var loop: Task<Void, Never>?
    private var subscribers: [UUID: AsyncStream<SyncState>.Continuation] = [:]

    public init(client: AlondraAPIClient, credentialRevision: String, retryDelay: Duration = .seconds(2)) {
        self.client = client
        let identity = SyncIdentity(
            server: client.base,
            deviceID: client.signer?.deviceID ?? client.device,
            credentialRevision: credentialRevision
        )
        self.state = SyncState(identity: identity, generation: 0, connection: .connecting, snapshot: nil)
        self.retryDelay = retryDelay
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

    /// Replaces transport and invalidates every in-flight response before cancelling the old poll.
    /// The new identity may be the same when the caller explicitly requests a fresh connection.
    public func use(_ client: AlondraAPIClient, credentialRevision: String) {
        generation &+= 1
        loop?.cancel()
        loop = nil
        self.client = client
        let identity = SyncIdentity(
            server: client.base,
            deviceID: client.signer?.deviceID ?? client.device,
            credentialRevision: credentialRevision
        )
        publish(SyncState(identity: identity, generation: generation, connection: .connecting, snapshot: nil))
        startIfNeeded()
    }

    /// Ends all subscriptions and cancels the active long poll.
    public func stop() {
        generation &+= 1
        loop?.cancel()
        loop = nil
        for continuation in subscribers.values { continuation.finish() }
        subscribers.removeAll()
        state = SyncState(identity: identity, generation: generation, connection: .connecting, snapshot: state.snapshot)
    }

    private func removeSubscriber(_ id: UUID) {
        subscribers.removeValue(forKey: id)
        guard subscribers.isEmpty else { return }
        generation &+= 1
        loop?.cancel()
        loop = nil
        state = SyncState(identity: identity, generation: generation, connection: .connecting, snapshot: state.snapshot)
    }

    private func startIfNeeded() {
        guard !subscribers.isEmpty, loop == nil else { return }
        let currentGeneration = generation
        let currentClient = client
        loop = Task { [weak self] in
            await self?.run(client: currentClient, generation: currentGeneration)
        }
    }

    private func publish(_ next: SyncState) {
        state = next
        for continuation in subscribers.values { continuation.yield(next) }
    }

    private func publish(_ connection: SyncConnection, snapshot: SyncSnapshot? = nil) {
        publish(SyncState(
            identity: identity,
            generation: generation,
            connection: connection,
            snapshot: snapshot ?? state.snapshot
        ))
    }

    private func run(client: AlondraAPIClient, generation current: UInt64) async {
        while !Task.isCancelled && generation == current && !subscribers.isEmpty {
            do {
                // Always fetch the complete state after a change, reconnection, or daemon restart.
                let body = try await client.send(AlondraEndpoints.overview())
                let snapshot = try SyncSnapshot(body: body)
                guard !Task.isCancelled, generation == current else { return }
                publish(.online, snapshot: snapshot)
                do {
                    let poll = Endpoint<Data>.data(
                        .get,
                        path: ["v1", "changes"],
                        query: [("after", String(snapshot.tick)), ("wait", String(Self.changeWaitSeconds))],
                        policy: .longPoll
                    )
                    _ = try await client.send(poll)
                } catch let error as URLError where error.code == .timedOut {
                    // A timeout is a normal empty poll; re-fetch to detect daemon restarts.
                }
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled, generation == current else { return }
                if case APIError.unauthorized = error {
                    publish(.unpaired)
                } else {
                    publish(.offline(String(describing: error)))
                }
                do { try await Task.sleep(for: retryDelay) } catch { return }
            }
        }
    }
}
