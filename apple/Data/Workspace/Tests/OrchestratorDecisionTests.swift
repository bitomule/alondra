import AlondraDomain
import AlondraLoopbackSession
import AlondraWorkspace
import CryptoKit
import Foundation
import XCTest

/// Retry, cancel and acknowledge go to the daemon's orchestrator decision routes with the run ID.
final class OrchestratorDecisionTests: XCTestCase {
    private var directory: URL!

    override func setUp() {
        directory = FileManager.default.temporaryDirectory.appending(path: "orchestrator-\(UUID().uuidString)")
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
    }

    func testEachDecisionPostsItsRunToItsRoute() async throws {
        let daemon = mac { _ in .status(200, Data("{}".utf8)) }
        let operations = try await nodeOperations(daemon)

        let retried = await operations.retryOrchestrator("run-7", "op-1")
        let cancelled = await operations.cancelOrchestrator("run-8", "op-2")
        let acknowledged = await operations.acknowledgeOrchestrator("run-9", "op-3")

        XCTAssertEqual([retried, cancelled, acknowledged], [.confirmed, .confirmed, .confirmed])
        let decisions = daemon.requests.filter { $0.target.hasPrefix("/v2/orchestrator/") }
        XCTAssertEqual(decisions.map(\.target), ["/v2/orchestrator/retry", "/v2/orchestrator/cancel", "/v2/orchestrator/acknowledge"])
        XCTAssertEqual(try decisions.map { try XCTUnwrap(JSONSerialization.jsonObject(with: $0.body) as? [String: String])["run_id"] },
                       ["run-7", "run-8", "run-9"])
        XCTAssertTrue(decisions.allSatisfy { $0.method == "POST" && $0.headers["X-Alondra-Signature"] != nil })
    }

    func testAnExecutionThatNoLongerWaitsIsRejectedWithTheDaemonsReason() async throws {
        let daemon = mac { _ in .status(409, Data(#"{"code":"conflict","message":"no interrupted execution run-7"}"#.utf8)) }
        let operations = try await nodeOperations(daemon)

        let outcome = await operations.retryOrchestrator("run-7", "op-1")

        XCTAssertEqual(outcome, .rejected("no interrupted execution run-7"))
    }

    private func mac(_ decision: @escaping (StubDaemon.Received) -> StubDaemon.Reply) -> StubDaemon {
        StubDaemon { request in
            switch request.target {
            case "/v2/pairing": .status(200, Data(#"{"code":"1"}"#.utf8))
            case "/v2/pair": .status(200, Data(#"{"device_id":"device"}"#.utf8))
            default: decision(request)
            }
        }
    }

    private func nodeOperations(_ daemon: StubDaemon) async throws -> NodeOperations {
        let identity = LoopbackIdentityStore(directory: directory, makeKey: { .software(P256.Signing.PrivateKey()) })
        let session = LoopbackSession(identity: identity, deviceName: "Mac", urlSession: daemon.urlSession())
        return try await CloudAppServices.local(session, identity: identity).nodeOperations()
    }
}
