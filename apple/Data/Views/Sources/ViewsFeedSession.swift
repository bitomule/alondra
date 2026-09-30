import AlondraViews
import Foundation

public struct ViewsFeedSnapshot: Equatable, Sendable {
    public let documents: [String: ViewDocument]
    public let status: ViewStatus?
    public let cursor: String?
    public let dataEpoch: String?
    public let reset: Bool
}

public enum ViewsFeedEvent: Equatable, Sendable {
    case stale([String])
    case reset
    case snapshot(ViewsFeedSnapshot)
}

public struct ViewLease: Hashable, Sendable {
    public let id: UUID
    public let key: String
    public init(id: UUID = UUID(), key: String) { self.id = id; self.key = key }
}

/// One actor instance belongs to one authenticated device identity. Open view keys are the
/// union of active screens in that identity; changing the union cancels and replaces its poll.
public actor ViewsFeedSession {
    private let transport: any ViewsTransport
    private let client: ViewClient
    private let lang: String
    private var leases: [UUID: String] = [:]
    private var versions: [String: String] = [:]
    private var documents: [String: ViewDocument] = [:]
    private var cursor: String?
    private var dataEpoch: String?
    private var status: ViewStatus?
    private var reset = false
    private var active = true
    private var generation: UInt64 = 0
    private var feedTask: Task<Void, Never>?
    private var subscribers: [UUID: AsyncStream<ViewsFeedSnapshot>.Continuation] = [:]
    private var eventSubscribers: [UUID: AsyncStream<ViewsFeedEvent>.Continuation] = [:]

    public init(transport: any ViewsTransport, client: ViewClient, lang: String) {
        self.transport = transport; self.client = client; self.lang = lang
    }

    public func snapshots() -> AsyncStream<ViewsFeedSnapshot> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<ViewsFeedSnapshot>.makeStream(bufferingPolicy: .bufferingNewest(1))
        subscribers[id] = continuation
        continuation.yield(snapshot)
        continuation.onTermination = { [weak self] _ in Task { await self?.removeSubscriber(id) } }
        restartFeed()
        return stream
    }

    public func events() -> AsyncStream<ViewsFeedEvent> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<ViewsFeedEvent>.makeStream(bufferingPolicy: .bufferingNewest(16))
        eventSubscribers[id] = continuation
        continuation.yield(.snapshot(snapshot))
        continuation.onTermination = { [weak self] _ in Task { await self?.removeEventSubscriber(id) } }
        restartFeed()
        return stream
    }

    public func load(_ key: String, known: String? = nil) async throws -> ViewFetch {
        guard active, Self.validViewKey(key) else { throw ViewsDataError.invalidKey }
        let result = try await transport.fetchView(key: key, client: client, lang: lang, known: known)
        guard active else { throw CancellationError() }
        switch result {
        case let .document(document):
            documents[key] = document
            versions[key] = document.version
            cursor = document.cursor
            dataEpoch = document.dataEpoch
        case let .unchanged(newCursor):
            guard documents[key] != nil else { throw ViewsDataError.unexpectedUnchanged }
            cursor = newCursor
        }
        publish(reset: false)
        return result
    }

    public func fetchContent(_ content: ContentRef) async throws -> String {
        guard active else { throw CancellationError() }
        return try await transport.fetchContent(content)
    }

    public func fetchAttachment(_ attachment: Attachment) async throws -> Data {
        guard active else { throw CancellationError() }
        return try await transport.fetchAttachment(attachment)
    }

    @discardableResult
    public func open(_ key: String) async throws -> (lease: ViewLease, document: ViewDocument) {
        guard Self.validViewKey(key) else { throw ViewsDataError.invalidKey }
        let lease = ViewLease(key: key)
        let wasOpen = openKeys.contains(key)
        leases[lease.id] = key
        if let cached = documents[key] {
            if !wasOpen { restartFeed() }
            return (lease, cached)
        }
        do {
            let result = try await load(key)
            guard case let .document(document) = result else { throw ViewsDataError.unexpectedUnchanged }
            guard leases[lease.id] != nil else { return (lease, document) }
            documents[key] = document
            versions[key] = document.version
            publish(reset: false)
            restartFeed()
            return (lease, document)
        } catch {
            leases.removeValue(forKey: lease.id)
            restartFeed()
            throw error
        }
    }

    public func close(_ lease: ViewLease) {
        guard leases.removeValue(forKey: lease.id) != nil else { return }
        guard !openKeys.contains(lease.key) else { return }
        versions.removeValue(forKey: lease.key); documents.removeValue(forKey: lease.key)
        publish(reset: false)
        restartFeed()
    }

    public func stop() {
        active = false
        generation &+= 1; feedTask?.cancel(); feedTask = nil
        for continuation in subscribers.values { continuation.finish() }
        subscribers.removeAll()
        for continuation in eventSubscribers.values { continuation.finish() }
        eventSubscribers.removeAll()
    }

    public var openViewKeys: [String] { openKeys.sorted() }
    public func currentSnapshot() -> ViewsFeedSnapshot { snapshot }

    private var snapshot: ViewsFeedSnapshot {
        ViewsFeedSnapshot(documents: documents, status: status, cursor: cursor, dataEpoch: dataEpoch, reset: reset)
    }

    private var openKeys: Set<String> { Set(leases.values) }

    private func publish(reset: Bool) {
        self.reset = reset
        let value = snapshot
        for continuation in subscribers.values { continuation.yield(value) }
        for continuation in eventSubscribers.values { continuation.yield(.snapshot(value)) }
    }

    private func emit(_ event: ViewsFeedEvent) {
        for continuation in eventSubscribers.values { continuation.yield(event) }
    }

    private func removeSubscriber(_ id: UUID) {
        subscribers.removeValue(forKey: id)
        stopFeedIfUnobserved()
    }

    private func removeEventSubscriber(_ id: UUID) {
        eventSubscribers.removeValue(forKey: id)
        stopFeedIfUnobserved()
    }

    private func stopFeedIfUnobserved() {
        if subscribers.isEmpty && eventSubscribers.isEmpty {
            generation &+= 1; feedTask?.cancel(); feedTask = nil
        }
    }

    private func restartFeed() {
        generation &+= 1; feedTask?.cancel(); feedTask = nil
        guard active, !openKeys.isEmpty, (!subscribers.isEmpty || !eventSubscribers.isEmpty) else { return }
        let currentGeneration = generation
        feedTask = Task { [weak self] in await self?.runFeed(generation: currentGeneration) }
    }

    private func runFeed(generation current: UInt64) async {
        while !Task.isCancelled, generation == current, !openKeys.isEmpty,
              (!subscribers.isEmpty || !eventSubscribers.isEmpty) {
            do {
                let openVersions = openKeys.sorted().map { OpenViewVersion(key: $0, version: versions[$0] ?? "") }
                    .filter { !$0.version.isEmpty }
                let reply = try await transport.fetchChanges(client: client, lang: lang, wait: 50, cursor: cursor, views: openVersions)
                guard !Task.isCancelled, generation == current else { return }
                if reply.superseded { return }
                cursor = reply.cursor; dataEpoch = reply.dataEpoch; status = reply.status
                if reply.reset {
                    versions.removeAll(); documents.removeAll()
                    emit(.reset)
                    publish(reset: true)
                    try await refresh(Array(openKeys).sorted(), generation: current)
                } else if !reply.stale.isEmpty {
                    let keys = reply.stale.filter { openKeys.contains($0) }.sorted()
                    emit(.stale(keys))
                    try await refresh(keys, generation: current)
                } else {
                    publish(reset: false)
                }
            } catch is CancellationError { return }
            catch {
                guard !Task.isCancelled, generation == current else { return }
                // Ending the feed leaves the last complete views visible. The owning app can call
                // reconnect() after its ordinary connection recovery path succeeds.
                return
            }
        }
    }

    private func refresh(_ keys: [String], generation current: UInt64) async throws {
        for key in keys {
            guard !Task.isCancelled, generation == current, openKeys.contains(key) else { return }
            let result = try await transport.fetchView(key: key, client: client, lang: lang, known: versions[key])
            guard !Task.isCancelled, generation == current, openKeys.contains(key) else { return }
            switch result {
            case let .document(document):
                documents[key] = document; versions[key] = document.version
                dataEpoch = document.dataEpoch
            case let .unchanged(newCursor):
                guard documents[key] != nil else { throw ViewsDataError.unexpectedUnchanged }
                cursor = newCursor
            }
            publish(reset: false)
        }
    }

    /// Opening a new view after an ended/failed feed starts a fresh poll for the current union.
    public func reconnect() { restartFeed() }

    static func validViewKey(_ key: String) -> Bool {
        ViewRoute.parse(key) != nil
    }
}
