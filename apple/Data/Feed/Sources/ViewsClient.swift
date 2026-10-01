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
    public var perform: @Sendable (ActionRequest) async throws -> ActionOutcome
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
            perform: { request in try await sender.send(request) },
            fetchContent: { content in try await session.fetchContent(content) },
            fetchAttachment: { attachment in try await session.fetchAttachment(attachment) }
        )
    }
}

/// Leases keep the server feed's open-key union equal to the screens currently visible.
private actor ViewOpenSet {
    private let session: FeedSession
    private var owners: [String: [ViewKey: ViewLease]] = [:]
    private var generations: [String: UInt64] = [:]

    init(session: FeedSession) { self.session = session }

    func replace(with keys: Set<ViewKey>) async {
        await replace(owner: "__root__", with: keys)
    }

    func replace(owner: String, with keys: Set<ViewKey>) async {
        let generation = (generations[owner] ?? 0) &+ 1
        generations[owner] = generation
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
        generations[owner, default: 0] &+= 1
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
        perform: { _ in throw ViewsClientError.notInstalled },
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
