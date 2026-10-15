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
/// error the daemon sends back included) means it is answering. Each one is also measured.
public struct MonitoredOperations: OperationAPI {
    public typealias Stopwatch = @Sendable () -> @Sendable () -> Duration

    let inner: any OperationAPI
    let monitor: ConnectionMonitor
    let route: @Sendable () async -> ConnectionRoute
    let record: @Sendable (RequestObservation) async -> Void
    let stopwatch: Stopwatch

    public init(_ inner: any OperationAPI, monitor: ConnectionMonitor, route: @escaping @Sendable () async -> ConnectionRoute,
                record: @escaping @Sendable (RequestObservation) async -> Void = { _ in },
                stopwatch: @escaping Stopwatch = MonitoredOperations.continuousStopwatch) {
        self.inner = inner
        self.monitor = monitor
        self.route = route
        self.record = record
        self.stopwatch = stopwatch
    }

    public static let continuousStopwatch: Stopwatch = {
        let start = ContinuousClock.now
        return { ContinuousClock.now - start }
    }

    public func perform(_ request: APIRequest) async throws -> APIResponse {
        try await observe("perform", request, { try await inner.perform(request) }, unanswered: { $0 == .unknown },
                          status: Self.status)
    }

    public func recover(_ request: APIRequest) async throws -> APIResponse {
        try await observe("recover", request, { try await inner.recover(request) }, silent: { $0 == .unknown }, status: Self.status)
    }

    public func resend(_ request: APIRequest) async throws -> APIResponse {
        try await observe("resend", request, { try await inner.resend(request) }, silent: { $0 == .unknown }, status: Self.status)
    }

    public func lookup(_ request: APIRequest) async throws -> OperationLookup {
        try await observe("lookup", request, { try await inner.lookup(request) }, silent: { $0 == .unknown }) {
            if case .completed(let status, _) = $0 { status } else { nil }
        }
    }

    public func retireActions(_ acknowledgment: ActionJournalAcknowledgment) async throws {
        try await observe("retire", nil, { try await inner.retireActions(acknowledgment) })
    }

    private static func status(_ response: APIResponse) -> Int? {
        if case .completed(let status, _) = response { status } else { nil }
    }

    /// `unanswered`: the request went out and the Mac's answer never came back. `silent`: the
    /// result says nothing about the Mac, such as nothing having been sent to recover, so it is
    /// neither reported nor measured.
    private func observe<Result: Sendable>(_ operation: String, _ request: APIRequest?,
                                           _ run: @Sendable () async throws -> Result,
                                           unanswered: (Result) -> Bool = { _ in false },
                                           silent: (Result) -> Bool = { _ in false },
                                           status: (Result) -> Int? = { _ in nil }) async throws -> Result {
        let ticket = await monitor.begin()
        let elapsed = stopwatch()
        func measured(_ route: ConnectionRoute?, _ outcome: ConnectionMeasurement.Outcome, status: Int? = nil,
                      error: String? = nil) -> RequestObservation {
            RequestObservation(operation: operation, method: request?.method ?? "POST",
                kind: request.map { RequestObservation.kind(of: $0.path) } ?? "operation-journal", route: route,
                duration: elapsed(), outcome: outcome, status: status, error: error)
        }
        do {
            let result = try await run()
            if unanswered(result) {
                let observation = measured(nil, .unanswered)
                await monitor.handle(.failed(.macNotAnswering, ticket: ticket))
                await record(observation)
            } else if !silent(result) {
                let observation = measured(nil, .ok)
                let route = await route()
                await monitor.handle(.answered(route))
                var answered = observation
                answered.route = route
                if let code = status(result), !(200..<300).contains(code) {
                    answered.outcome = .macError
                    answered.status = code
                }
                await record(answered)
            }
            return result
        } catch {
            let cancelled = error is CancellationError || Task.isCancelled
            let observation = measured(nil, cancelled ? .cancelled : .failed, error: RequestObservation.label(error))
            if !cancelled { await monitor.failed(error, ticket: ticket) }
            await record(observation)
            throw error
        }
    }
}

/// Push registration goes to the relay, not the Mac: it passes through as it is.
extension MonitoredOperations: PushRegistrationAPI {
    public func registerServicePush(token: String, environment: String, binding: String) async throws {
        guard let push = inner as? any PushRegistrationAPI else { throw OperationError.invalidConfiguration }
        try await push.registerServicePush(token: token, environment: environment, binding: binding)
    }
}
