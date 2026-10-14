import AlondraContract
import AlondraTransport
import Foundation

/// The one place that decides whether this device's Mac is answering: every request to it, the
/// network and the person's retries go through here, and every screen reads what it says.
public actor ConnectionMonitor {
    private var machine = ConnectionMachine()
    private var subscribers: [UUID: AsyncStream<ConnectionState>.Continuation] = [:]
    private let classify: @Sendable (any Error) -> ConnectionProblem
    private var networkTask: Task<Void, Never>?

    public init(classify: @escaping @Sendable (any Error) -> ConnectionProblem) {
        self.classify = classify
    }

    public var state: ConnectionState { machine.state }

    public nonisolated func problem(for error: any Error) -> ConnectionProblem { classify(error) }

    /// Yields the current state, then every change.
    public func states() -> AsyncStream<ConnectionState> {
        let id = UUID()
        let (stream, continuation) = AsyncStream<ConnectionState>.makeStream(bufferingPolicy: .bufferingNewest(1))
        subscribers[id] = continuation
        continuation.yield(machine.state)
        continuation.onTermination = { [weak self] _ in Task { await self?.removeSubscriber(id) } }
        return stream
    }

    func begin() -> UInt64 { machine.begin() }

    public func handle(_ event: ConnectionMachine.Event) {
        let before = machine.state
        machine.handle(event)
        guard machine.state != before else { return }
        for continuation in subscribers.values { continuation.yield(machine.state) }
    }

    func failed(_ error: any Error, ticket: UInt64) {
        handle(.failed(classify(error), ticket: ticket))
    }

    /// Follows the device's network: losing it is known at once, getting it back starts over.
    public func follow(network changes: AsyncStream<Bool>) {
        networkTask?.cancel()
        networkTask = Task { [weak self] in
            for await online in changes {
                await self?.handle(online ? .networkBack : .networkLost)
            }
        }
    }

    public func stop() {
        networkTask?.cancel()
        networkTask = nil
        for continuation in subscribers.values { continuation.finish() }
        subscribers.removeAll()
    }

    private func removeSubscriber(_ id: UUID) {
        subscribers.removeValue(forKey: id)
    }
}

/// Every request to the Mac, reported to the monitor as it ends: any answer from the Mac (an
/// error the daemon sends back included) means it is answering.
public struct MonitoredOperations: OperationAPI {
    let inner: any OperationAPI
    let monitor: ConnectionMonitor
    let route: @Sendable () async -> ConnectionRoute

    public init(_ inner: any OperationAPI, monitor: ConnectionMonitor, route: @escaping @Sendable () async -> ConnectionRoute) {
        self.inner = inner
        self.monitor = monitor
        self.route = route
    }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        try await observe({ try await inner.perform(request) }, unanswered: { $0 == .unknown })
    }

    public func recover(_ request: APIRequest) async throws -> APIResponse {
        try await observe({ try await inner.recover(request) }, silent: { $0 == .unknown })
    }

    public func resend(_ request: APIRequest) async throws -> APIResponse {
        try await observe({ try await inner.resend(request) }, silent: { $0 == .unknown })
    }

    public func lookup(_ request: APIRequest) async throws -> OperationLookup {
        try await observe({ try await inner.lookup(request) }, silent: { $0 == .unknown })
    }

    public func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        try await observe({ try await inner.retireActions(acknowledgment) })
    }

    /// `unanswered`: the request went out and the Mac's answer never came back. `silent`: the
    /// result says nothing about the Mac, such as nothing having been sent to recover.
    private func observe<Result: Sendable>(_ operation: @Sendable () async throws -> Result,
                                           unanswered: (Result) -> Bool = { _ in false },
                                           silent: (Result) -> Bool = { _ in false }) async throws -> Result {
        let ticket = await monitor.begin()
        do {
            let result = try await operation()
            if unanswered(result) {
                await monitor.handle(.failed(.macNotAnswering, ticket: ticket))
            } else if !silent(result) {
                await monitor.handle(.answered(await route()))
            }
            return result
        } catch {
            if !(error is CancellationError), !Task.isCancelled { await monitor.failed(error, ticket: ticket) }
            throw error
        }
    }
}
