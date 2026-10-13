import AlondraContract
import ComposableArchitecture
import Foundation

public typealias ViewLoad = ViewFetch
public typealias ActionOutcome = ActionResponse

@DependencyClient
public struct ViewsClient: Sendable {
    public var load: @Sendable (ViewKey, String?) async throws -> ViewLoad
    public var open: @Sendable (Set<ViewKey>) async -> Void
    /// Replaces the keys owned by one independently presented screen. The feed remains open
    /// for the union of screen owners and the app-level owner above.
    public var openScreen: @Sendable (String, Set<ViewKey>) async -> Void
    public var closeScreen: @Sendable (String) async -> Void
    public var feed: @Sendable () async -> AsyncStream<FeedEvent> = { AsyncStream { $0.finish() } }
    /// One screen's view: the first document, then a newer one each time the daemon says it changed.
    /// The key stays open while the stream is consumed and is released when the consumer stops.
    /// Throws `ViewsClientError.missingView` when the daemon no longer serves the key.
    public var observe: @Sendable (ViewKey) -> AsyncThrowingStream<ViewFetch, any Error> = { _ in
        AsyncThrowingStream { $0.finish(throwing: ViewsClientError.notInstalled) }
    }
    /// Yields each time the app retries the connection on purpose; never finishes on its own.
    public var reconnections: @Sendable () async -> AsyncStream<Void> = { AsyncStream { _ in } }
    public var prepare: @Sendable (ActionRequest) async throws -> ActionRequest = { $0 }
    /// Delivers an answer until the Mac answers it; throws only when it can never be delivered.
    public var perform: @Sendable (ActionRequest) async throws -> ActionOutcome
    /// The cards answered on this device whose answer the Mac has not confirmed yet.
    public var undeliveredCards: @Sendable () -> AsyncStream<Set<String>> = { AsyncStream { $0.finish() } }
    public var fetchContent: @Sendable (ContentRef) async throws -> String
    public var fetchAttachment: @Sendable (Attachment) async throws -> Data

    /// Install one instance at the authenticated scene boundary. Its session and sender are
    /// bound to that identity; discard them when the identity changes or logs out.
    public static func live(session: FeedSession, sender: ViewActionSender) -> Self {
        let openSet = ViewOpenSet(session: session)
        return Self(
            load: { key, known in
                do { return try await session.load(key.rawValue, known: known) }
                catch FeedError.missingView { throw ViewsClientError.missingView }
            },
            open: { keys in await openSet.replace(with: keys) },
            openScreen: { owner, keys in await openSet.replace(owner: owner, with: keys) },
            closeScreen: { owner in await openSet.remove(owner: owner) },
            feed: { await session.events() },
            observe: { key in ViewObservation.stream(session: session, key: key) },
            reconnections: { await session.reconnections() },
            prepare: { request in try await sender.prepare(request) },
            perform: { request in try await sender.deliver(request) },
            undeliveredCards: { sender.undelivered.stream() },
            fetchContent: { content in try await session.fetchContent(content) },
            fetchAttachment: { attachment in try await session.fetchAttachment(attachment) }
        )
    }
}

/// Leases keep the server feed's open-key union equal to the screens currently visible.
actor ViewOpenSet {
    private let session: FeedSession
    private(set) var owners: [String: [ViewKey: ViewLease]] = [:]
    private(set) var generations: [String: UInt64] = [:]
    private var nextGeneration: UInt64 = 0

    init(session: FeedSession) { self.session = session }

    func replace(with keys: Set<ViewKey>) async {
        await replace(owner: "__root__", with: keys)
    }

    func replace(owner: String, with keys: Set<ViewKey>) async {
        nextGeneration &+= 1
        let generation = nextGeneration
        generations[owner] = generation
        defer {
            if generations[owner] == generation, owners[owner]?.isEmpty == true {
                owners[owner] = nil
                generations[owner] = nil
            }
        }
        var leases = owners[owner] ?? [:]
        let removed = leases.keys.filter { !keys.contains($0) }
        let retired = removed.compactMap { leases.removeValue(forKey: $0) }
        owners[owner] = leases
        for lease in retired { await session.close(lease) }
        guard generations[owner] == generation else { return }
        for key in keys where owners[owner]?[key] == nil {
            guard generations[owner] == generation else { return }
            guard let opened = try? await session.open(key.rawValue) else { continue }
            guard generations[owner] == generation else {
                await session.close(opened.lease)
                return
            }
            owners[owner, default: [:]][key] = opened.lease
        }
    }

    func remove(owner: String) async {
        generations[owner] = nil
        let leases = owners.removeValue(forKey: owner) ?? [:]
        for lease in leases.values { await session.close(lease) }
    }
}

private enum ViewsClientKey: DependencyKey {
    static let liveValue = ViewsClient(
        load: { _, _ in throw ViewsClientError.notInstalled },
        open: { _ in },
        openScreen: { _, _ in },
        closeScreen: { _ in },
        feed: { AsyncStream { $0.finish() } },
        observe: { _ in AsyncThrowingStream { $0.finish(throwing: ViewsClientError.notInstalled) } },
        reconnections: { AsyncStream { _ in } },
        prepare: { $0 },
        perform: { _ in throw ViewsClientError.notInstalled },
        undeliveredCards: { AsyncStream { $0.finish() } },
        fetchContent: { _ in throw ViewsClientError.notInstalled },
        fetchAttachment: { _ in throw ViewsClientError.notInstalled }
    )
    static let testValue = liveValue
}

public enum ViewsClientError: Error, Equatable, Sendable { case notInstalled, missingView }

public extension DependencyValues {
    var viewsClient: ViewsClient {
        get { self[ViewsClientKey.self] }
        set { self[ViewsClientKey.self] = newValue }
    }
}
