import AlondraContract
@testable import AlondraTransport
@testable import AlondraFeed
import Foundation
import XCTest

final class ConnectionMonitorTests: XCTestCase {
    private let request = try! APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["capabilities"]), operationID: "read-1")

    /// 15 Oct, iPhone Settings: «Your Mac didn’t accept this device’s alerts». The monitor hid the
    /// relay's push registration, so every registration failed before it was sent.
    func testPushRegistrationGoesThroughTheMonitorToTheRelayAndTheMac() async throws {
        let inner = PushRecordingOperations()
        let monitored = MonitoredOperations(inner, monitor: ConnectionMonitor { _ in .unexpected }) { .relay }
        let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "phone-1", operations: monitored)
        try await api.registerServicePush(token: "abcd", environment: "sandbox", binding: String(repeating: "A", count: 43),
                                          preferencesRevision: 1)
        let registered = await inner.registered
        let paths = await inner.paths
        XCTAssertEqual(registered, ["abcd"])
        XCTAssertEqual(paths, ["/device/push-binding"])
    }

    func testAnyAnswerFromTheMacMeansItIsAnswering() async throws {
        let (monitor, operations) = make(answers: [.success(.completed(status: 404, body: Data()))])
        _ = try await operations.perform(request)
        let state = await monitor.state
        XCTAssertEqual(state, .connected(.tailscale))
    }

    func testAReadTheRelayCouldNotGetAnsweredCountsAsTheMacNotAnswering() async throws {
        let (monitor, operations) = make(answers: Array(repeating: .success(.unknown), count: 3))
        for _ in 0 ..< ConnectionMachine.passingFailuresShown { _ = try await operations.perform(request) }
        let state = await monitor.state
        XCTAssertEqual(state, .unreachable(.macNotAnswering))
    }

    func testAnErrorIsClassifiedOnce() async throws {
        let (monitor, operations) = make(answers: [.failure(URLError(.notConnectedToInternet))], problem: .noInternet)
        _ = try? await operations.perform(request)
        let state = await monitor.state
        XCTAssertEqual(state, .unreachable(.noInternet))
        XCTAssertEqual(monitor.problem(for: URLError(.notConnectedToInternet)), .noInternet)
    }

    func testACancelledRequestSaysNothingAboutTheMac() async throws {
        let (monitor, operations) = make(answers: [.failure(CancellationError())], problem: .sessionExpired)
        _ = try? await operations.perform(request)
        let state = await monitor.state
        XCTAssertEqual(state, .connecting)
    }

    func testNothingToRecoverSaysNothingAboutTheMac() async throws {
        let (monitor, operations) = make(answers: [.success(.unknown)], problem: .sessionExpired)
        _ = try await operations.recover(request)
        let state = await monitor.state
        XCTAssertEqual(state, .connecting)
    }

    func testEveryWatcherEndsOnTheSameState() async throws {
        let (monitor, operations) = make(answers: [.success(.completed(status: 200, body: Data()))])
        let first = await monitor.states()
        let second = await monitor.states()
        _ = try await operations.perform(request)
        await monitor.handle(.networkLost)
        await monitor.stop()
        var seen: [[ConnectionState]] = []
        for stream in [first, second] {
            var states: [ConnectionState] = []
            for await state in stream { states.append(state) }
            seen.append(states)
        }
        XCTAssertEqual(seen[0].last, .unreachable(.noInternet))
        XCTAssertEqual(seen[1].last, .unreachable(.noInternet))
    }

    private func make(answers: [Result<APIResponse, any Error>],
                      problem: ConnectionProblem = .macNotAnswering) -> (ConnectionMonitor, MonitoredOperations) {
        let monitor = ConnectionMonitor { _ in problem }
        return (monitor, MonitoredOperations(ScriptedOperations(answers), monitor: monitor) { .tailscale })
    }
}

private actor ScriptedOperations: OperationAPI {
    private var answers: [Result<APIResponse, any Error>]
    init(_ answers: [Result<APIResponse, any Error>]) { self.answers = answers }

    func perform(_ request: APIRequest) async throws -> APIResponse { try answers.removeFirst().get() }
    func recover(_ request: APIRequest) async throws -> APIResponse { try answers.removeFirst().get() }
}

private actor PushRecordingOperations: OperationAPI, PushRegistrationAPI {
    private(set) var registered: [String] = []
    private(set) var paths: [String] = []

    func perform(_ request: APIRequest) async throws -> APIResponse {
        paths.append(request.path)
        return .completed(status: 204, body: Data())
    }

    func registerServicePush(token: String, environment: String, binding: String) async throws {
        registered.append(token)
    }
}
