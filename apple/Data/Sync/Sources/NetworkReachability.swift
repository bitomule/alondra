import Foundation
import Network

/// Whether this device has a usable network path. Relay reads turn a dead network into an
/// unanswered request, so the path is what tells "no internet" apart from "your Mac is off".
public final class NetworkReachability: @unchecked Sendable {
    public static let shared = NetworkReachability()

    private let monitor = NWPathMonitor()
    private let lock = NSLock()
    private var online = true
    private var network = ""
    private var interface = "other"
    private var listeners: [UUID: AsyncStream<Bool>.Continuation] = [:]
    private var hintListeners: [UUID: AsyncStream<Void>.Continuation] = [:]

    private init() {
        monitor.pathUpdateHandler = { [weak self] path in
            guard let self else { return }
            let signature = Self.signature(of: path)
            let kind = Self.interface(of: path)
            let moved = lock.withLock {
                defer { network = signature; interface = kind }
                return online && path.status == .satisfied && !network.isEmpty && network != signature
            }
            update(path.status == .satisfied)
            if moved { hint() }
        }
        monitor.start(queue: DispatchQueue(label: "alondra.network-reachability"))
    }

    public var isOffline: Bool { lock.withLock { !online } }

    /// Which network this is: its interfaces and gateways. Joining another Wi-Fi changes it, so
    /// an address learned on one network is proved again before it is believed on the next.
    public var signature: String { lock.withLock { network } }

    /// The kind of network requests leave on now: `wifi`, `cellular`, `wired`, `other` or `offline`.
    public var interfaceKind: String { lock.withLock { interface } }

    private static func interface(of path: NWPath) -> String {
        guard path.status == .satisfied else { return "offline" }
        if path.usesInterfaceType(.wifi) { return "wifi" }
        if path.usesInterfaceType(.cellular) { return "cellular" }
        if path.usesInterfaceType(.wiredEthernet) { return "wired" }
        return "other"
    }

    private static func signature(of path: NWPath) -> String {
        let interfaces = path.availableInterfaces.map { "\($0.type)/\($0.name)" }
        let gateways = path.gateways.map { "\($0)" }
        return (interfaces.sorted() + gateways.sorted()).joined(separator: ",")
    }

    /// Yields the current state, then every change.
    public func changes() -> AsyncStream<Bool> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<Bool>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let current = lock.withLock {
            listeners[id] = continuation
            return online
        }
        continuation.yield(current)
        continuation.onTermination = { [weak self] _ in self?.lock.withLock { _ = self?.listeners.removeValue(forKey: id) } }
        return stream
    }

    /// Yields when this device moves to another network while online, and on every `hint()`:
    /// signs that a connection that failed may work now.
    public func retryHints() -> AsyncStream<Void> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        lock.withLock { hintListeners[id] = continuation }
        continuation.onTermination = { [weak self] _ in self?.lock.withLock { _ = self?.hintListeners.removeValue(forKey: id) } }
        return stream
    }

    /// Something reached this device from outside, such as a push.
    public func hint() {
        for target in lock.withLock({ Array(hintListeners.values) }) { target.yield() }
    }

    private func update(_ satisfied: Bool) {
        let targets: [AsyncStream<Bool>.Continuation] = lock.withLock {
            guard online != satisfied else { return [] }
            online = satisfied
            return Array(listeners.values)
        }
        for target in targets { target.yield(satisfied) }
    }
}
