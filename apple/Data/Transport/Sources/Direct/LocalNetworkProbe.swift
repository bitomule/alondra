import Foundation
import Network
import Synchronization

/// Why the system leaves this app without a path to a Mac's address, read from Network.framework.
/// iOS refuses a local address with -1009 when Local Network access is off, the same code as having
/// no internet; only the path's `unsatisfiedReason` tells the two apart.
public struct LocalNetworkProbe: Sendable {
    private let reason: @Sendable (DirectEndpoint) async -> NWPath.UnsatisfiedReason

    public init(reason: @escaping @Sendable (DirectEndpoint) async -> NWPath.UnsatisfiedReason) {
        self.reason = reason
    }

    func denies(_ endpoint: DirectEndpoint) async -> Bool {
        await reason(endpoint) == .localNetworkDenied
    }

    /// Opens a TCP connection to the address and reads the reason once it waits for a path.
    /// Recommended by Apple DTS: https://developer.apple.com/forums/thread/663769
    public static let network = LocalNetworkProbe { endpoint in
        guard let port = NWEndpoint.Port(rawValue: endpoint.port) else { return .notAvailable }
        let probe = PathProbe(NWConnection(host: NWEndpoint.Host(endpoint.host), port: port, using: .tcp))
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in probe.start(continuation) }
        } onCancel: {
            probe.finish(.notAvailable)
        }
    }
}

private final class PathProbe: Sendable {
    private static let patience: DispatchTimeInterval = .seconds(2)
    private let connection: NWConnection
    private let waiter = Mutex<(started: Bool, continuation: CheckedContinuation<NWPath.UnsatisfiedReason, Never>?)>((false, nil))

    init(_ connection: NWConnection) { self.connection = connection }

    func start(_ continuation: CheckedContinuation<NWPath.UnsatisfiedReason, Never>) {
        let finished = waiter.withLock { state in
            if state.started { return true }
            state = (true, continuation)
            return false
        }
        if finished { return continuation.resume(returning: .notAvailable) }
        connection.stateUpdateHandler = { [self] state in
            switch state {
            case .waiting: finish(connection.currentPath?.unsatisfiedReason ?? .notAvailable)
            case .ready, .failed, .cancelled: finish(.notAvailable)
            default: break
            }
        }
        let queue = DispatchQueue.global(qos: .userInitiated)
        connection.start(queue: queue)
        queue.asyncAfter(deadline: .now() + Self.patience) { [self] in finish(.notAvailable) }
    }

    func finish(_ reason: NWPath.UnsatisfiedReason) {
        let continuation = waiter.withLock { state in
            defer { state = (true, nil) }
            return state.continuation
        }
        guard let continuation else { return }
        connection.stateUpdateHandler = nil
        connection.cancel()
        continuation.resume(returning: reason)
    }
}
