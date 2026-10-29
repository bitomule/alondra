import AlondraContract
import CryptoKit
import Foundation

/// One live iroh connection to one paired Mac, kept open while the app is in the foreground. In
/// the background a request opens one for itself, which closes once nothing has used it for
/// `lingerAfterUse`. Each request is one stream on it: the sealed envelope goes out, the Mac's
/// answer comes back. See docs/iroh-protocol.md.
public actor IrohCarrier: EnvelopeCarrier {
    public enum Event: Sendable, Equatable {
        /// Connected for the first time since `start()`, this long after it.
        case connected(ConnectionRoute?, after: Duration)
        /// Connected in the background for a request, this long after it asked.
        case connectedOnDemand(ConnectionRoute?, after: Duration)
        case routeChanged(ConnectionRoute)
        case lost
    }

    private let dialer: any IrohDialer
    private let macID: String
    private let deviceID: String
    private let signing: P256.Signing.PrivateKey
    private let address: @Sendable () async -> IrohMacAddress?
    private let prepare: @Sendable (IrohMacAddress) async throws -> Void
    private let connectWait: Duration
    private let dialTimeout: Duration
    private let lingerAfterUse: Duration
    private let retryDelays: [Duration]
    private let clock = ContinuousClock()
    private var observer: (@Sendable (Event) -> Void)?
    private var link: (any IrohLink)?
    private var currentRoute: ConnectionRoute?
    private var keeper: Task<Void, Never>?
    private var generation = 0
    private var connectingNow = false
    private var waiters: [UUID: CheckedContinuation<(any IrohLink)?, Never>] = [:]
    private var running = false
    private var startedAt: ContinuousClock.Instant?
    private var prepared: IrohMacAddress?
    private var shut = false
    private var onDemandDial: Task<(any IrohLink)?, Never>?
    private var inUse = 0
    private var linger: Task<Void, Never>?

    /// `prepare` runs before dialing an address that goes through the relay: the iroh relay
    /// admits only endpoints the Alondra relay knows.
    public init(dialer: any IrohDialer, macID: String, deviceID: String, signing: P256.Signing.PrivateKey,
                address: @escaping @Sendable () async -> IrohMacAddress?,
                prepare: @escaping @Sendable (IrohMacAddress) async throws -> Void = { _ in },
                connectWait: Duration = .seconds(5), dialTimeout: Duration = .seconds(10),
                lingerAfterUse: Duration = .seconds(3),
                retryDelays: [Duration] = [.seconds(1), .seconds(2), .seconds(5), .seconds(15), .seconds(30)]) {
        self.dialer = dialer
        self.macID = macID
        self.deviceID = deviceID
        self.signing = signing
        self.address = address
        self.prepare = prepare
        self.connectWait = connectWait
        self.dialTimeout = dialTimeout
        self.lingerAfterUse = lingerAfterUse
        self.retryDelays = retryDelays
    }

    public func observe(_ observer: @escaping @Sendable (Event) -> Void) {
        self.observer = observer
    }

    /// The path the live connection uses, nil without one.
    public var route: ConnectionRoute? { liveLink == nil ? nil : currentRoute }

    public var isConnected: Bool { liveLink != nil }

    var isConnecting: Bool { connectingNow }

    var requestsWaitingForConnection: Int { waiters.count }

    /// The app came to the foreground: connect now and stay connected. A connection a request
    /// opened in the background is kept.
    public func start() {
        guard !running, !shut else { return }
        running = true
        linger?.cancel()
        linger = nil
        startedAt = liveLink == nil ? clock.now : nil
        if onDemandDial != nil {
            connectingNow = true
        } else if !connectingNow {
            keepConnected()
        }
    }

    /// Requests already using or opening a connection finish before it becomes idle.
    public func stop() async {
        running = false
        startedAt = nil
        if !shut, inUse > 0 {
            if !connectingNow, link == nil, let keeper {
                keeper.cancel()
                self.keeper = nil
                generation += 1
            }
            return
        }
        keeper?.cancel()
        keeper = nil
        linger?.cancel()
        linger = nil
        generation += 1
        connectingNow = false
        startedAt = nil
        resolveWaiters(nil)
        let closing = link
        link = nil
        currentRoute = nil
        await closing?.close()
    }

    /// The network changed: the connection migrates by itself, but a reconnect waiting out a
    /// failure goes now.
    public func networkChanged() {
        guard running, liveLink == nil, !connectingNow else { return }
        keepConnected()
    }

    /// The device moved to another network (Wi-Fi to mobile data, or back). A connection made
    /// on the old network keeps the path it had, so it is replaced at once by one that looks for
    /// paths on the new network; a read cut off by the switch goes again on the new connection.
    public func networkMoved() async {
        guard running else { return }
        let old = link
        link = nil
        currentRoute = nil
        await dialer.networkMoved()
        keepConnected()
        await old?.close()
    }

    public func shutdown() async {
        shut = true
        await stop()
        await dialer.shutdown()
    }

    func deliver(_ body: Data, open: @escaping @Sendable (Envelope) throws -> APIResponse, mutation: Bool,
                 timeout: Duration, attempts: Int, lifetime: DirectSessionLifetime) async throws -> CarrierAnswer {
        var reconnected = false
        for attempt in 0..<max(attempts, 2) {
            try MutationScope.check()
            guard lifetime.isActive else { throw CancellationError() }
            inUse += 1
            defer { release() }
            guard let link = await readyLink() else { throw IrohCarrierError.unavailable }
            try MutationScope.check()
            let reply: Data
            do {
                reply = try await Deadline.run(within: timeout) {
                    try await link.exchange(body, answerLimit: DirectHTTP.maxResponse + 64)
                }
            } catch IrohLinkError.notSent {
                drop(link)
                if reconnected { throw IrohCarrierError.unavailable }
                reconnected = true
                continue
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                guard lifetime.isActive else { throw CancellationError() }
                if link.isClosed || (error as? IrohLinkError) == .noAnswer {
                    drop(link)
                    if !mutation, !reconnected {
                        reconnected = true
                        continue
                    }
                }
                if mutation { return .unknown }
                if attempt + 1 < attempts { continue }
                throw IrohCarrierError.noAnswer
            }
            guard lifetime.isActive else { throw CancellationError() }
            return try Self.answer(reply, open: open, mutation: mutation)
        }
        throw IrohCarrierError.unavailable
    }

    static func answer(_ reply: Data, open: (Envelope) throws -> APIResponse, mutation: Bool) throws -> CarrierAnswer {
        struct Answer: Decodable {
            let response: Envelope?
            let revoked: Envelope?
            let error: String?
        }
        guard let answer = try? JSONDecoder().decode(Answer.self, from: reply) else {
            if mutation { return .unknown }
            throw IrohCarrierError.noAnswer
        }
        // The connection itself proves the Mac's iroh key, but only the pinned keys prove the answer.
        if let envelope = answer.response ?? answer.revoked {
            guard let response = try? open(envelope) else {
                if mutation { return .unknown }
                throw IrohCarrierError.noAnswer
            }
            return .sealed(envelope, response)
        }
        switch answer.error {
        case "idempotency_conflict": throw SealedRequestError.operationIdentityConflict
        case "rejected": return .plain(.completed(status: 400, body: reply))
        case let code?: throw IrohCarrierError.refused(code)
        case nil:
            if mutation { return .unknown }
            throw IrohCarrierError.noAnswer
        }
    }

    private var liveLink: (any IrohLink)? {
        guard let link, !link.isClosed else { return nil }
        return link
    }

    /// The live connection, or the one being made if it is ready within `connectWait`; a
    /// reconnect waiting out a failure goes now. In the background, one opened for this request.
    private func readyLink() async -> (any IrohLink)? {
        if let liveLink { return liveLink }
        guard !shut else { return nil }
        if let onDemandDial { return await onDemandDial.value }
        guard running || (keeper != nil && connectingNow) else { return await onDemandLink() }
        if !connectingNow { keepConnected() }
        let id = UUID()
        let wait = connectWait
        return await withCheckedContinuation { continuation in
            waiters[id] = continuation
            Task {
                try? await Task.sleep(for: wait)
                self.expire(id)
            }
        }
    }

    private func onDemandLink() async -> (any IrohLink)? {
        let dial: Task<(any IrohLink)?, Never>
        if let onDemandDial {
            dial = onDemandDial
        } else {
            let current = generation
            dial = Task { await self.dialOnDemand(current) }
            onDemandDial = dial
        }
        return await dial.value
    }

    private func dialOnDemand(_ current: Int) async -> (any IrohLink)? {
        let asked = clock.now
        let connected = await connectOnce()
        onDemandDial = nil
        guard generation == current, !shut else {
            await connected?.close()
            return running ? await readyLink() : nil
        }
        connectingNow = false
        guard let connected else {
            resolveWaiters(nil)
            if running { keepConnected() }
            return nil
        }
        linger?.cancel()
        linger = nil
        link = connected
        currentRoute = connected.path().route
        if running {
            connectingNow = false
            adopt(connected)
            resolveWaiters(connected)
            keepConnected()
        } else {
            observer?(.connectedOnDemand(currentRoute, after: asked.duration(to: clock.now)))
        }
        return connected
    }

    /// A connection opened in the background closes once nothing has used it for a moment, so
    /// a request that follows another one reuses it.
    private func release() {
        inUse -= 1
        guard !running, inUse == 0, link != nil else { return }
        linger?.cancel()
        let watched = link, wait = lingerAfterUse
        linger = Task {
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled else { return }
            await self.closeUnused(watched)
        }
    }

    private func closeUnused(_ watched: (any IrohLink)?) async {
        guard !running, inUse == 0, let closing = link, closing === watched else { return }
        link = nil
        currentRoute = nil
        linger = nil
        await closing.close()
    }

    private func expire(_ id: UUID) {
        waiters.removeValue(forKey: id)?.resume(returning: nil)
    }

    private func resolveWaiters(_ link: (any IrohLink)?) {
        let pending = waiters
        waiters.removeAll()
        for continuation in pending.values { continuation.resume(returning: link) }
    }

    private func drop(_ dropped: any IrohLink) {
        guard let link, link === dropped else { return }
        self.link = nil
        currentRoute = nil
        observer?(.lost)
        Task { await dropped.close() }
        if running { keepConnected() }
    }

    private func keepConnected() {
        keeper?.cancel()
        generation += 1
        connectingNow = liveLink == nil
        let current = generation
        keeper = Task { await self.connectLoop(current) }
    }

    private func connectLoop(_ current: Int) async {
        defer {
            if generation == current {
                connectingNow = false
                keeper = nil
            }
        }
        var failures = 0
        while running || inUse > 0, generation == current, !shut, !Task.isCancelled {
            if liveLink == nil {
                connectingNow = true
                let connected = await connectOnce()
                guard running || inUse > 0, generation == current, !shut else {
                    if generation == current {
                        connectingNow = false
                        keeper = nil
                    }
                    await connected?.close()
                    return
                }
                connectingNow = false
                resolveWaiters(connected)
                guard let connected else {
                    guard running else { return }
                    let delay = retryDelays[min(failures, retryDelays.count - 1)]
                    failures += 1
                    try? await Task.sleep(for: delay)
                    continue
                }
                failures = 0
                adopt(connected)
            }
            guard running else { return }
            guard let watched = link else { continue }
            for await path in watched.pathChanges() {
                guard link === watched, let route = path.route, route != currentRoute else { continue }
                currentRoute = route
                observer?(.routeChanged(route))
            }
            guard generation == current else { return }
            if link === watched {
                link = nil
                currentRoute = nil
                observer?(.lost)
            }
        }
    }

    private func adopt(_ connected: any IrohLink) {
        link = connected
        currentRoute = connected.path().route
        if let startedAt {
            observer?(.connected(currentRoute, after: startedAt.duration(to: clock.now)))
            self.startedAt = nil
        }
    }

    private func connectOnce() async -> (any IrohLink)? {
        guard let mac = await address(), let macEndpoint = IrohIdentity.endpointBytes(mac.endpointID) else { return nil }
        if mac.relay != nil, prepared != mac {
            do { try await prepare(mac) } catch { return nil }
            prepared = mac
        }
        let hello: Data
        do {
            hello = try IrohHello.message(macID: macID, deviceID: deviceID, deviceEndpoint: dialer.endpointID,
                                          macEndpoint: macEndpoint, signing: signing)
        } catch { return nil }
        let dialer = dialer
        let link: any IrohLink
        do { link = try await Deadline.run(within: dialTimeout) { try await dialer.dial(mac) } }
        catch { return nil }
        do {
            let reply = try await Deadline.run(within: dialTimeout) {
                try await link.exchange(hello, answerLimit: 4096)
            }
            if IrohHello.accepted(reply) { return link }
        } catch {}
        await link.close()
        return nil
    }
}
