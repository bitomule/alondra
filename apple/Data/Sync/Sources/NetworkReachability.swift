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
    private var listeners: [UUID: AsyncStream<Bool>.Continuation] = [:]

    private init() {
        monitor.pathUpdateHandler = { [weak self] path in
            self?.lock.withLock { self?.network = Self.signature(of: path) }
            self?.update(path.status == .satisfied)
        }
        monitor.start(queue: DispatchQueue(label: "alondra.network-reachability"))
    }

    public var isOffline: Bool { lock.withLock { !online } }

    /// Which network this is: its interfaces and gateways. Joining another Wi-Fi changes it, so
    /// an address learned on one network is proved again before it is believed on the next.
    public var signature: String { lock.withLock { network } }

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

    private func update(_ satisfied: Bool) {
        let targets: [AsyncStream<Bool>.Continuation] = lock.withLock {
            guard online != satisfied else { return [] }
            online = satisfied
            return Array(listeners.values)
        }
        for target in targets { target.yield(satisfied) }
    }
}
