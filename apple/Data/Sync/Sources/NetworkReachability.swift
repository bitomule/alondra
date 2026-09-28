import Foundation
import Network

/// Whether this device has a usable network path. Relay reads turn a dead network into an
/// unanswered request, so the path is what tells "no internet" apart from "your Mac is off".
public final class NetworkReachability: @unchecked Sendable {
    public static let shared = NetworkReachability()

    private let monitor = NWPathMonitor()
    private let lock = NSLock()
    private var online = true
    private var listeners: [UUID: AsyncStream<Bool>.Continuation] = [:]

    private init() {
        monitor.pathUpdateHandler = { [weak self] path in self?.update(path.status == .satisfied) }
        monitor.start(queue: DispatchQueue(label: "alondra.network-reachability"))
    }

    public var isOffline: Bool { lock.withLock { !online } }

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
