import AlondraTransport
import AlondraContract
import Foundation

public struct FeedSnapshot: Equatable, Sendable {
    public let documents: [String: ViewDocument]
    public let status: ViewStatus?
    public let cursor: String?
    public let dataEpoch: String?
    public let reset: Bool
}

public enum FeedEvent: Equatable, Sendable {
    case stale([String])
    case reset
    case snapshot(FeedSnapshot)
}

public struct ViewLease: Hashable, Sendable {
    public let id: UUID
    public let key: String
    public init(id: UUID = UUID(), key: String) { self.id = id; self.key = key }
}

/// One actor instance belongs to one authenticated device identity. Open view keys are the
/// union of active screens in that identity; changing the union cancels and replaces its poll.
public actor FeedSession {
    private let transport: any ViewTransport
    private let client: ViewClient
    private let lang: String
    private var leases: [UUID: String] = [:]
    private var versions: [String: String] = [:]
    private var documents: [String: ViewDocument] = [:]
    /// The cards a view brought inside it, by that view's key, kept so a screen opening one draws it
    /// at once; a card a screen holds open is that screen's to keep current.
    private var embedded: [String: Set<String>] = [:]
    private enum LoadError: Error, Sendable { case epochMismatch }
    struct LoadKey: Hashable, Sendable {
        let key: String
        let known: String?
    }
    private struct LoadCompletion: Sendable {
        let owner: UUID
        let lifetime: UUID
        let result: Result<ViewFetch, any Error>
        let epochGeneration: UInt64
        let retired: Bool
        let leaseIDs: Set<UUID>

        init(owner: UUID, lifetime: UUID, result: Result<ViewFetch, any Error>, epochGeneration: UInt64, retired: Bool = false, leaseIDs: Set<UUID> = []) {
            self.owner = owner; self.lifetime = lifetime; self.result = result
            self.epochGeneration = epochGeneration; self.retired = retired; self.leaseIDs = leaseIDs
        }
    }
    public enum LoadPriority: Sendable { case interactive, background }
    private struct LoadWaiter {
        let priority: LoadPriority
        let continuation: CheckedContinuation<LoadCompletion, Never>
    }
    private struct InFlightLoad {
        let id: UUID
        var lifetime: UUID
        var epochGeneration: UInt64
        let requiredEpoch: String?
        var priority: LoadPriority
        var task: Task<Void, Never>?
        var waiters: [UUID: LoadWaiter]
    }
    private var inFlightLoads: [LoadKey: InFlightLoad] = [:]
    private static let loadLimit = 4
    private static let backgroundLimit = loadLimit - 1
    private var runningPriorities: [UUID: LoadPriority] = [:]
    private var interactiveAdmissions = 0
    private(set) var runningLoadTasks: [UUID: Task<Void, Never>] = [:]
    var runningLoadIDs: Set<UUID> { Set(runningLoadTasks.keys) }
    private var pendingLoads: [(request: LoadKey, id: UUID)] = []
    private(set) var loadOwners: [String: UUID] = [:]
    private(set) var loadLifetimes: [String: UUID] = [:]
    private var cursorLoadOwner: UUID?
    private var feedOwnsCursor = false
    private var epochGeneration: UInt64 = 0
    private var cursor: String?
    private var dataEpoch: String?
    private var status: ViewStatus?
    private var reset = false
    private var active = true
    private(set) var generation: UInt64 = 0
    private var feedTask: Task<Void, Never>?
    private var subscribers: [UUID: AsyncStream<FeedSnapshot>.Continuation] = [:]
    private var eventSubscribers: [UUID: AsyncStream<FeedEvent>.Continuation] = [:]
    private var reconnectionSubscribers: [UUID: AsyncStream<Void>.Continuation] = [:]
    private let retryDelay: @Sendable (Int) async throws -> Void
    private let reconcileActions: (@Sendable (String, @escaping @Sendable () async -> Bool) async -> Void)?
    private var recoveryTask: Task<Void, Never>?
    private var recoveryID: UUID?
    private var recoveryWanted: (generation: UInt64, epoch: String)?
    private var recoveredGeneration: UInt64?
    private var recoveredEpoch: String?

    public init(transport: any ViewTransport, client: ViewClient, lang: String,
                retryDelay: @escaping @Sendable (Int) async throws -> Void = FeedSession.backoff,
                reconcileActions: (@Sendable (String, @escaping @Sendable () async -> Bool) async -> Void)? = nil) {
        self.transport = transport; self.client = client; self.lang = lang; self.retryDelay = retryDelay
        self.reconcileActions = reconcileActions
    }

    /// Capped exponential backoff with full jitter: up to 1, 2, 4, 8, 16, then 30 seconds.
    public static func backoff(_ attempt: Int) async throws {
        let ceiling = min(30, pow(2, Double(min(max(attempt, 1), 6) - 1)))
        try await Task.sleep(for: .seconds(Double.random(in: 0...ceiling)))
    }

    public func snapshots() -> AsyncStream<FeedSnapshot> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<FeedSnapshot>.makeStream(bufferingPolicy: .bufferingNewest(1))
        guard active else { continuation.finish(); return stream }
        subscribers[id] = continuation
        continuation.yield(snapshot)
        continuation.onTermination = { [weak self] _ in Task { await self?.removeSubscriber(id) } }
        restartFeed()
        return stream
    }

    public func events() -> AsyncStream<FeedEvent> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<FeedEvent>.makeStream(bufferingPolicy: .bufferingNewest(16))
        guard active else { continuation.finish(); return stream }
        eventSubscribers[id] = continuation
        continuation.yield(.snapshot(snapshot))
        continuation.onTermination = { [weak self] _ in Task { await self?.removeEventSubscriber(id) } }
        restartFeed()
        return stream
    }

    public func load(_ key: String, known: String?, priority: LoadPriority) async throws -> ViewFetch {
        try await ViewLoadContext.$priority.withValue(priority) { try await self.load(key, known: known) }
    }

    public func load(_ key: String, known: String? = nil) async throws -> ViewFetch {
        guard active else { throw CancellationError() }
        guard Self.validViewKey(key) else { throw FeedError.invalidKey }
        var request = LoadKey(key: key, known: known)
        var requiredEpoch: String?
        while true {
            let completion = try await waitForLoad(request, requiredEpoch: requiredEpoch)
            self.preconditionIsolated()
            try Task.checkCancellation()
            guard active else { throw CancellationError() }
            if case let .failure(error) = completion.result, error is CancellationError { throw error }
            var ownsLiveLease = completion.leaseIDs.isEmpty
            for id in completion.leaseIDs {
                if leases[id] == key { ownsLiveLease = true; break }
            }
            guard ownsLiveLease else { throw CancellationError() }
            if completion.epochGeneration != epochGeneration {
                if let document = documents[key], document.dataEpoch == dataEpoch { return .document(document) }
                request = LoadKey(key: key, known: nil)
                requiredEpoch = dataEpoch
                continue
            }
            // A terminal one-off result owns its bytes without retaining the key forever.
            if completion.retired {
                if let document = documents[key] {
                    return known == document.version ? .unchanged(cursor: cursor ?? document.cursor) : .document(document)
                }
                if let current = currentLoad(key) { request = current; continue }
                return try completion.result.get()
            }
            if loadLifetimes[key] != completion.lifetime {
                guard openKeys.contains(key) else { throw CancellationError() }
                if let document = documents[key] { return .document(document) }
                request = LoadKey(key: key, known: nil)
                continue
            }
            if loadOwners[key] != completion.owner {
                if let document = documents[key] {
                    return known == document.version ? .unchanged(cursor: cursor ?? document.cursor) : .document(document)
                }
                guard let current = currentLoad(key) else { throw CancellationError() }
                request = current
                continue
            }
            switch try completion.result.get() {
            case .document:
                guard let document = documents[key] else { throw CancellationError() }
                return .document(document)
            case let .unchanged(newCursor):
                guard let document = documents[key] else { throw FeedError.unexpectedUnchanged }
                return known == document.version ? .unchanged(cursor: newCursor) : .document(document)
            }
        }
    }

    private func waitForLoad(_ request: LoadKey, requiredEpoch: String?) async throws -> LoadCompletion {
        let priority = ViewLoadContext.priority
        let waiter = UUID()
        return try await withTaskCancellationHandler {
            try Task.checkCancellation()
            return await withCheckedContinuation { continuation in
                if Task.isCancelled {
                    continuation.resume(returning: LoadCompletion(owner: UUID(), lifetime: UUID(), result: .failure(CancellationError()), epochGeneration: epochGeneration))
                } else if var load = inFlightLoads[request] {
                    load.waiters[waiter] = LoadWaiter(priority: priority, continuation: continuation)
                    if priority == .interactive {
                        load.priority = .interactive
                    }
                    inFlightLoads[request] = load
                    admitLoads()
                } else {
                    startLoad(request, waiters: [waiter: LoadWaiter(priority: priority, continuation: continuation)], requiredEpoch: requiredEpoch)
                }
            }
        } onCancel: {
            Task { await self.cancelLoadWaiter(waiter) }
        }
    }

    private static func priority(of waiters: [UUID: LoadWaiter]) -> LoadPriority {
        for waiter in waiters.values where waiter.priority == .interactive { return .interactive }
        return .background
    }

    private func startLoad(_ request: LoadKey, waiters: [UUID: LoadWaiter], requiredEpoch: String? = nil) {
        let id = UUID()
        let lifetime = loadLifetimes[request.key] ?? UUID()
        loadLifetimes[request.key] = lifetime
        loadOwners[request.key] = id
        cursorLoadOwner = id
        inFlightLoads[request] = InFlightLoad(id: id, lifetime: lifetime, epochGeneration: epochGeneration, requiredEpoch: requiredEpoch, priority: Self.priority(of: waiters), task: nil, waiters: waiters)
        pendingLoads.append((request, id))
        admitLoads()
    }

    private func admitLoads() {
        guard active else { return }
        var live: [(request: LoadKey, id: UUID)] = []
        for pending in pendingLoads where inFlightLoads[pending.request]?.id == pending.id { live.append(pending) }
        pendingLoads = live
        while runningLoadTasks.count < Self.loadLimit, !pendingLoads.isEmpty {
            var interactive: Int?
            var background: Int?
            var runningBackground = 0
            for priority in runningPriorities.values where priority == .background { runningBackground += 1 }
            let backgroundAvailable = runningBackground < Self.backgroundLimit
            for index in pendingLoads.indices {
                let priority = inFlightLoads[pendingLoads[index].request]?.priority
                if interactive == nil, priority == .interactive { interactive = index }
                if background == nil, backgroundAvailable, priority == .background { background = index }
            }
            let index: Int
            if let background, interactive == nil || interactiveAdmissions >= 3 {
                index = background
                interactiveAdmissions = 0
            } else if let interactive {
                index = interactive
                interactiveAdmissions = min(3, interactiveAdmissions + 1)
            } else { break }
            let next = pendingLoads.remove(at: index)
            guard var load = inFlightLoads[next.request], load.id == next.id else { continue }
            let transport = self.transport
            let client = self.client
            let lang = self.lang
            let task = Task { @concurrent [weak self] in
                let result: Result<ViewFetch, any Error>
                do {
                    try Task.checkCancellation()
                    let fetched = try await transport.fetchView(key: next.request.key, client: client, lang: lang, known: next.request.known)
                    try Task.checkCancellation()
                    result = .success(fetched)
                } catch { result = .failure(error) }
                await self?.finishLoad(next.request, id: next.id, result: result)
            }
            load.task = task
            runningLoadTasks[next.id] = task
            runningPriorities[next.id] = load.priority
            inFlightLoads[next.request] = load
        }
    }

    private func finishLoad(_ request: LoadKey, id: UUID, result: Result<ViewFetch, any Error>) {
        completeLoad(request, id: id, result: result)
        runningLoadTasks.removeValue(forKey: id)
        runningPriorities.removeValue(forKey: id)
        admitLoads()
    }

    private func currentLoad(_ key: String) -> LoadKey? {
        inFlightLoads.first { $0.key.key == key && $0.value.id == loadOwners[key] }?.key
    }

    func completeLoad(_ request: LoadKey, id: UUID, result: Result<ViewFetch, any Error>) {
        guard let load = inFlightLoads[request], load.id == id else { return }
        guard loadOwners[request.key] == id else {
            inFlightLoads.removeValue(forKey: request)
            pendingLoads.removeAll { $0.id == load.id }
            if let current = currentLoad(request.key), var owner = inFlightLoads[current] {
                owner.waiters.merge(load.waiters) { existing, _ in existing }
                if Self.priority(of: owner.waiters) == .interactive {
                    owner.priority = .interactive
                }
                inFlightLoads[current] = owner
                admitLoads()
            } else {
                let cancelled = LoadCompletion(owner: load.id, lifetime: load.lifetime, result: .failure(CancellationError()), epochGeneration: load.epochGeneration)
                for waiter in load.waiters.values { waiter.continuation.resume(returning: cancelled) }
                pruneLoadMetadata(request.key)
            }
            return
        }
        var accepted = result
        var changedEpoch = false
        switch result {
        case let .success(.document(document)):
            if let required = load.requiredEpoch, document.dataEpoch != required {
                accepted = .failure(LoadError.epochMismatch)
            } else if document.dataEpoch != dataEpoch {
                if dataEpoch == nil || (cursorLoadOwner == id && load.epochGeneration == epochGeneration && load.requiredEpoch == nil) {
                    changedEpoch = dataEpoch != nil
                    dataEpoch = document.dataEpoch
                    cursor = document.cursor
                    status = document.status
                    feedOwnsCursor = false
                    epochGeneration &+= 1
                    versions.removeAll(); documents.removeAll(); embedded.removeAll()
                    resetLoads(preserving: request)
                    if changedEpoch { emit(.reset) }
                } else {
                    recoverUnconditional(request.key)
                    return
                }
            }
            if case .success = accepted {
                let transientPicker = request.key == "issue_picker" || request.key.hasPrefix("issue_picker?")
                if transientPicker && !openKeys.contains(request.key) {
                    documents[request.key] = nil
                    versions[request.key] = nil
                } else {
                    documents[request.key] = document
                    versions[request.key] = document.version
                    keepEmbedded(document.firstCards.filter { $0.dataEpoch == document.dataEpoch }, from: request.key)
                }
                if !feedOwnsCursor, cursorLoadOwner == id || cursor == nil {
                    cursor = document.cursor
                }
                publish(reset: changedEpoch)
                scheduleRecovery()
            }
        case let .success(.unchanged(newCursor)):
            if let document = documents[request.key], request.known == document.version,
               document.dataEpoch == dataEpoch {
                if !feedOwnsCursor, cursorLoadOwner == id { cursor = newCursor }
                publish(reset: false)
            } else if request.known != nil {
                recoverUnconditional(request.key)
                return
            } else {
                accepted = .failure(FeedError.unexpectedUnchanged)
            }
        case let .failure(error):
            if Self.isMissingView(error) {
                if versions.removeValue(forKey: request.key) != nil {
                    documents.removeValue(forKey: request.key)
                    emit(.stale([request.key]))
                    publish(reset: false)
                }
                accepted = .failure(FeedError.missingView)
            }
        }
        guard let current = inFlightLoads[request], current.id == id else { return }
        let matching = inFlightLoads.keys.filter { $0.key == request.key }
        var waiters: [UUID: LoadWaiter] = [:]
        for key in matching {
            guard let finished = inFlightLoads.removeValue(forKey: key) else { continue }
            pendingLoads.removeAll { $0.id == finished.id }
            if finished.id != id { finished.task?.cancel() }
            waiters.merge(finished.waiters) { existing, _ in existing }
        }
        let retired = pruneLoadMetadata(request.key)
        let completion = LoadCompletion(owner: id, lifetime: current.lifetime, result: accepted, epochGeneration: epochGeneration, retired: retired,
            leaseIDs: Set(leases.filter { $0.value == request.key }.keys))
        for waiter in waiters.values { waiter.continuation.resume(returning: completion) }
    }

    private func recoverUnconditional(_ key: String) {
        // An older unconditional request can still describe the cache rejected by the newer answer.
        let requests = inFlightLoads.keys.filter { $0.key == key }
        var waiters: [UUID: LoadWaiter] = [:]
        for request in requests {
            guard let load = inFlightLoads.removeValue(forKey: request) else { continue }
            pendingLoads.removeAll { $0.id == load.id }
            waiters.merge(load.waiters) { existing, _ in existing }
            load.task?.cancel()
        }
        guard !waiters.isEmpty else { pruneLoadMetadata(key); return }
        startLoad(LoadKey(key: key, known: nil), waiters: waiters, requiredEpoch: dataEpoch)
    }

    @discardableResult
    private func pruneLoadMetadata(_ key: String) -> Bool {
        guard documents[key] == nil, !openKeys.contains(key),
              !inFlightLoads.contains(where: { $0.key.key == key }) else { return false }
        let owner = loadOwners.removeValue(forKey: key)
        loadLifetimes.removeValue(forKey: key)
        if cursorLoadOwner == owner { cursorLoadOwner = nil }
        return true
    }

    private func cancelLoadWaiter(_ id: UUID) {
        guard let request = inFlightLoads.first(where: { $0.value.waiters[id] != nil })?.key,
              var load = inFlightLoads[request], let waiter = load.waiters.removeValue(forKey: id) else { return }
        if load.task == nil {
            load.priority = Self.priority(of: load.waiters)
        }
        inFlightLoads[request] = load
        waiter.continuation.resume(returning: LoadCompletion(owner: load.id, lifetime: load.lifetime, result: .failure(CancellationError()), epochGeneration: load.epochGeneration))
        let needed = inFlightLoads.contains { $0.key.key == request.key && !$0.value.waiters.isEmpty }
        if !needed {
            cancelLoads(request.key)
        } else if load.waiters.isEmpty, loadOwners[request.key] != load.id {
            inFlightLoads.removeValue(forKey: request)?.task?.cancel()
            pendingLoads.removeAll { $0.id == load.id }
        }
    }

    private func cancelLoads(_ key: String) {
        let requests = inFlightLoads.keys.filter { $0.key == key }
        loadOwners.removeValue(forKey: key)
        loadLifetimes.removeValue(forKey: key)
        for request in requests {
            guard let load = inFlightLoads.removeValue(forKey: request) else { continue }
            pendingLoads.removeAll { $0.id == load.id }
            load.task?.cancel()
            let cancelled = LoadCompletion(owner: load.id, lifetime: load.lifetime, result: .failure(CancellationError()), epochGeneration: load.epochGeneration)
            for waiter in load.waiters.values { waiter.continuation.resume(returning: cancelled) }
        }
    }

    private func resetLoads(preserving preservedRequest: LoadKey? = nil) {
        var preserved = preservedRequest.flatMap { inFlightLoads[$0] }
        var waiters: [String: [UUID: LoadWaiter]] = [:]
        for (request, load) in inFlightLoads {
            if load.id == preserved?.id { continue }
            load.task?.cancel()
            if request.key == preservedRequest?.key {
                preserved?.waiters.merge(load.waiters) { existing, _ in existing }
            } else {
                waiters[request.key, default: [:]].merge(load.waiters) { existing, _ in existing }
            }
        }
        inFlightLoads.removeAll()
        pendingLoads.removeAll()
        loadOwners.removeAll(); loadLifetimes.removeAll()
        for key in openKeys { loadLifetimes[key] = UUID() }
        if let preservedRequest, var load = preserved {
            load.lifetime = UUID(); load.epochGeneration = epochGeneration
            loadLifetimes[preservedRequest.key] = load.lifetime
            loadOwners[preservedRequest.key] = load.id
            inFlightLoads[preservedRequest] = load
        }
        for (key, waiting) in waiters where !waiting.isEmpty {
            startLoad(LoadKey(key: key, known: nil), waiters: waiting, requiredEpoch: dataEpoch)
        }
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
    public func open(_ key: String) async throws -> (lease: ViewLease, document: ViewDocument?) {
        guard active else { throw CancellationError() }
        guard Self.validViewKey(key) else { throw FeedError.invalidKey }
        let lease = ViewLease(key: key)
        let wasOpen = openKeys.contains(key)
        leases[lease.id] = key
        if let cached = documents[key] {
            if !wasOpen { restartFeed() }
            return (lease, cached)
        }
        do {
            let result = try await load(key)
            guard active, leases[lease.id] != nil else { throw CancellationError() }
            guard case let .document(document) = result else { throw FeedError.unexpectedUnchanged }
            if !wasOpen || feedTask == nil { restartFeed() }
            return (lease, document)
        } catch FeedError.missingView {
            guard active, leases[lease.id] != nil else { throw CancellationError() }
            if !wasOpen || feedTask == nil { restartFeed() }
            return (lease, nil)
        } catch {
            close(lease)
            throw error
        }
    }

    private func keepEmbedded(_ cards: [ViewDocument], from key: String) {
        let cards = cards.filter { Self.validViewKey($0.key) && $0.key != key && !openKeys.contains($0.key) }
        let kept = Set(cards.map(\.key))
        let elsewhere = embedded.filter { $0.key != key }.values.reduce(into: Set<String>()) { $0.formUnion($1) }
        for gone in (embedded[key] ?? []).subtracting(kept).subtracting(elsewhere) where !openKeys.contains(gone) {
            documents[gone] = nil; versions[gone] = nil
        }
        for card in cards {
            documents[card.key] = card; versions[card.key] = card.version
        }
        embedded[key] = kept.isEmpty ? nil : kept
    }

    public func close(_ lease: ViewLease) {
        guard leases.removeValue(forKey: lease.id) != nil else { return }
        guard !openKeys.contains(lease.key) else { return }
        cancelLoads(lease.key)
        versions.removeValue(forKey: lease.key); documents.removeValue(forKey: lease.key)
        keepEmbedded([], from: lease.key)
        publish(reset: false)
        restartFeed()
    }

    public func stop() {
        active = false
        recoveryWanted = nil; recoveryTask?.cancel()
        for key in Set(inFlightLoads.keys.map(\.key)) { cancelLoads(key) }
        leases.removeAll(); versions.removeAll(); documents.removeAll(); embedded.removeAll()
        loadOwners.removeAll(); loadLifetimes.removeAll()
        cursor = nil; dataEpoch = nil; status = nil
        generation &+= 1; feedTask?.cancel(); feedTask = nil
        for continuation in subscribers.values { continuation.finish() }
        subscribers.removeAll()
        for continuation in eventSubscribers.values { continuation.finish() }
        eventSubscribers.removeAll()
        for continuation in reconnectionSubscribers.values { continuation.finish() }
        reconnectionSubscribers.removeAll()
    }

    public var openViewKeys: [String] { openKeys.sorted() }
    public func currentSnapshot() -> FeedSnapshot { snapshot }

    private var snapshot: FeedSnapshot {
        FeedSnapshot(documents: documents, status: status, cursor: cursor, dataEpoch: dataEpoch, reset: reset)
    }

    private var openKeys: Set<String> { Set(leases.values) }

    private func publish(reset: Bool) {
        self.reset = reset
        let value = snapshot
        for continuation in subscribers.values { continuation.yield(value) }
        for continuation in eventSubscribers.values { continuation.yield(.snapshot(value)) }
    }

    /// Yields each time the connection is retried on purpose (back in the foreground, a new
    /// network, a push), so a screen that failed tries again then instead of waiting out its backoff.
    public func reconnections() -> AsyncStream<Void> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        guard active else { continuation.finish(); return stream }
        reconnectionSubscribers[id] = continuation
        continuation.onTermination = { [weak self] _ in Task { await self?.removeReconnectionSubscriber(id) } }
        return stream
    }

    private func removeReconnectionSubscriber(_ id: UUID) {
        reconnectionSubscribers.removeValue(forKey: id)
    }

    private func emit(_ event: FeedEvent) {
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
        recoveryWanted = nil; recoveryTask?.cancel()
        scheduleRecovery()
        guard active, !openKeys.isEmpty, (!subscribers.isEmpty || !eventSubscribers.isEmpty) else { return }
        let currentGeneration = generation
        feedTask = Task { [weak self] in await self?.runFeed(generation: currentGeneration) }
    }

    private func runFeed(generation current: UInt64) async {
        var failures = 0
        while !Task.isCancelled, generation == current, !openKeys.isEmpty,
              (!subscribers.isEmpty || !eventSubscribers.isEmpty) {
            do {
                let openVersions = openKeys.sorted().map { OpenViewVersion(key: $0, version: versions[$0] ?? "") }
                    .filter { !$0.version.isEmpty }
                let pollEpoch = epochGeneration
                // After a failure the Mac answers at once, so the app knows it is back without
                // waiting out a long poll; then the poll holds again until something changes.
                let reply = try await transport.fetchChanges(client: client, lang: lang, wait: failures == 0 ? 50 : 0,
                                                             cursor: cursor, views: openVersions)
                guard !Task.isCancelled, generation == current else { return }
                guard pollEpoch == epochGeneration else { continue }
                if reply.superseded {
                    // Another poll of this device reached the Mac after this one: over the relay,
                    // usually one this session already cancelled. Ask again, slower each time, so two
                    // live pollers of one device cannot take turns as fast as they can.
                    failures += 1
                    do { try await retryDelay(failures) } catch { return }
                    continue
                }
                let priorStatus = status
                let priorEpoch = dataEpoch
                feedOwnsCursor = true
                cursor = reply.cursor; dataEpoch = reply.dataEpoch; status = reply.status
                if reply.reset || priorEpoch != reply.dataEpoch {
                    epochGeneration &+= 1
                    versions.removeAll(); documents.removeAll(); embedded.removeAll()
                    resetLoads()
                    emit(.reset)
                    publish(reset: true)
                    try await refresh(Array(openKeys).sorted(), generation: current)
                } else if !reply.stale.isEmpty {
                    let keys = reply.stale.filter { openKeys.contains($0) }.sorted()
                    emit(.stale(keys))
                    try await refresh(keys, generation: current)
                } else if priorStatus != reply.status || priorEpoch != reply.dataEpoch {
                    publish(reset: false)
                }
                let missing = openKeys.filter { versions[$0] == nil }.sorted()
                if !missing.isEmpty { try await refresh(missing, generation: current) }
                scheduleRecovery()
                failures = 0
            } catch is CancellationError { return }
            catch {
                // The cursor is kept, so the next poll resumes where this one stopped, or is
                // answered with a reset when the Mac's data changed meanwhile. Only a refusal
                // that retrying cannot fix ends the feed; reconnect() starts it again. Any route,
                // relay or network failure is retried, slower each time.
                guard !Task.isCancelled, generation == current, Self.isTransient(error) else { return }
                failures += 1
                do { try await retryDelay(failures) } catch { return }
            }
        }
    }

    private func scheduleRecovery() {
        guard active, let epoch = dataEpoch, let reconcileActions else { return }
        guard recoveredGeneration != generation || recoveredEpoch != epoch else { return }
        if recoveryTask != nil {
            recoveryWanted = (generation, epoch)
            recoveryTask?.cancel()
            return
        }
        let id = UUID()
        let current = generation
        recoveredGeneration = current; recoveredEpoch = epoch
        recoveryID = id
        recoveryTask = Task { @concurrent [weak self] in
            await reconcileActions(epoch, { [weak self] in
                guard let self else { return false }
                return await self.recoveryIsCurrent(id, generation: current, epoch: epoch)
            })
            await self?.finishedRecovery(id)
        }
    }

    private func recoveryIsCurrent(_ id: UUID, generation current: UInt64, epoch: String) -> Bool {
        active && !Task.isCancelled && recoveryID == id && generation == current && dataEpoch == epoch
    }

    private func finishedRecovery(_ id: UUID) {
        guard recoveryID == id else { return }
        recoveryTask = nil; recoveryID = nil
        guard let wanted = recoveryWanted else { return }
        recoveryWanted = nil
        guard active, wanted.generation == generation, wanted.epoch == dataEpoch else { return }
        scheduleRecovery()
    }

    func drainRecovery() async {
        while let recoveryTask { await recoveryTask.value }
    }

    /// False only for a refusal from the Mac itself or a request no retry can fix.
    static func isTransient(_ error: any Error) -> Bool {
        func retryable(_ status: Int) -> Bool { status == 408 || status == 429 || status >= 500 }
        switch error {
        case let error as APIError:
            switch error {
            case .conflict, .unavailable: return true
            case let .server(status, _, _, _), let .unexpected(status): return retryable(status)
            case .unauthorized, .forbidden: return false
            }
        case let error as FeedError:
            return error == .unexpectedUnchanged
        case is DirectSessionError, SealedRequestError.invalidRequest, SealedRequestError.operationIdentityConflict,
             RelayWireError.unauthorized, RelayWireError.invalidEndpoint, RelayWireError.invalidField,
             OperationError.invalidEndpoint:
            return false
        default:
            return true
        }
    }

    func refresh(_ keys: [String], generation current: UInt64) async throws {
        guard !Task.isCancelled, generation == current else { return }
        try await withThrowingTaskGroup(of: Void.self, isolation: self) { group in
            var remaining = keys.makeIterator()
            for _ in 0..<Self.loadLimit {
                guard let key = remaining.next() else { break }
                enqueueRefresh(key, generation: current, into: &group)
            }
            while try await group.next() != nil {
                guard !Task.isCancelled, generation == current else { group.cancelAll(); return }
                if let key = remaining.next() { enqueueRefresh(key, generation: current, into: &group) }
            }
        }
    }

    private func enqueueRefresh(_ key: String, generation current: UInt64, into group: inout ThrowingTaskGroup<Void, any Error>) {
        guard !Task.isCancelled, generation == current, openKeys.contains(key), !group.isCancelled else { return }
        let task = Task.immediate { try await self.refreshKey(key, generation: current) }
        group.addTask {
            try await withTaskCancellationHandler {
                try await task.value
                try Task.checkCancellation()
            } onCancel: { task.cancel() }
        }
    }

    private func refreshKey(_ key: String, generation current: UInt64) async throws {
        guard !Task.isCancelled, generation == current, openKeys.contains(key) else { return }
        let wasMissing = versions[key] == nil
        do { _ = try await load(key, known: versions[key], priority: .background) }
        catch FeedError.missingView { return }
        guard !Task.isCancelled, generation == current, openKeys.contains(key) else { return }
        if wasMissing, documents[key] != nil { emit(.stale([key])) }
    }

    private static func isMissingView(_ error: any Error) -> Bool {
        switch error {
        case APIError.unexpected(status: 404), APIError.server(status: 404, kind: _, detail: _, reasons: _):
            return true
        case FeedError.missingView:
            return true
        default:
            return false
        }
    }

    /// Opening a new view after an ended/failed feed starts a fresh poll for the current union.
    public func reconnect() {
        restartFeed()
        guard active else { return }
        for continuation in reconnectionSubscribers.values { continuation.yield() }
    }

    static func validViewKey(_ key: String) -> Bool {
        ViewRoute.parse(key) != nil
    }
}

private enum ViewLoadContext {
    @TaskLocal static var priority: FeedSession.LoadPriority = .interactive
}
