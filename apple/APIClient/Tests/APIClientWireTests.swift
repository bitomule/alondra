import AlondraAPIClient
import Foundation
import XCTest

private actor WireOperations: ServiceOperationTransport {
    private var requests: [ServiceRequest] = []
    let response: ServiceResponse
    init(status: Int = 200, body: Data = Data()) { response = .completed(status: status, body: body) }
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        requests.append(request)
        return response
    }
    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        XCTFail("An endpoint request must not enter the long-poll capability")
    }
    func recorded() -> [ServiceRequest] { requests }
}

private actor ControlledChangeWait: ServiceOperationTransport {
    private var started: (UInt64, Int)?
    private var startWaiter: CheckedContinuation<Void, Never>?
    private var response: CheckedContinuation<Void, any Error>?
    private(set) var cancellations = 0

    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        XCTFail("A service change wait must not be encoded as a daemon operation")
        return .unknown
    }
    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
                guard !Task.isCancelled else { continuation.resume(throwing: CancellationError()); return }
                response = continuation
                started = (afterSnapshotTick, seconds)
                startWaiter?.resume()
                startWaiter = nil
            }
        } onCancel: { Task { await self.cancel() } }
    }
    func waitUntilStarted() async -> (UInt64, Int) {
        if started == nil { await withCheckedContinuation { startWaiter = $0 } }
        return started!
    }
    private func cancel() {
        guard let response else { return }
        self.response = nil
        cancellations += 1
        response.resume(throwing: CancellationError())
    }
}

final class APIClientWireTests: XCTestCase {
    private func client(_ operations: any ServiceOperationTransport) -> AlondraAPIClient {
        AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "mac", operations: operations)
    }

    func testServiceReplyErrorsPreserveAuthoritativeBodiesAndRejectNonSuccess() async throws {
        let cases: [(Int, String, APIError)] = [
            (404, "", .unexpected(status: 404)),
            (404, #"{"error":"gone"}"#, .server(status: 404, kind: nil, detail: "gone", reasons: [])),
            (404, #"{"error":""}"#, .server(status: 404, kind: nil, detail: "", reasons: [])),
            (401, "", .unauthorized),
            (403, #"{"kind":"permission","error":"device revoked"}"#, .forbidden(kind: "permission", detail: "device revoked", reasons: [])),
            (409, #"{"kind":"task_refused","error":"closed"}"#, .conflict(kind: "task_refused", detail: "closed", reasons: [])),
            (502, "", .unavailable),
            (503, #"{"error":"daemon reply"}"#, .server(status: 503, kind: nil, detail: "daemon reply", reasons: [])),
            (302, "", .unexpected(status: 302)),
            (304, "", .unexpected(status: 304)),
        ]
        for (status, body, expected) in cases {
            let operations = WireOperations(status: status, body: Data(body.utf8))
            do {
                _ = try await client(operations).send(AlondraEndpoints.nodeActivity("node"))
                XCTFail("Expected service response \(status) to fail")
            } catch { XCTAssertEqual(error as? APIError, expected) }
            let requests = await operations.recorded()
            XCTAssertEqual(requests.count, 1, "Error handling must not replay the operation")
        }
    }

    func testAnswerKeepsDeviceIdentityAndLogicalPathAndQueryAreEscaped() async throws {
        let operations = WireOperations(body: Data("{}".utf8))
        let api = client(operations)
        try await api.answer(7, text: "yes", option: nil, attachments: ["file-1"], operationID: "answer-7")
        _ = try await api.task("a/b")
        _ = try await api.search("a b&c?#é")
        let requests = await operations.recorded()
        XCTAssertEqual(requests.count, 3)
        XCTAssertEqual(requests[0].operationID, "answer-7")
        XCTAssertEqual(requests[0].path, "/v1/questions/7/answer")
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: XCTUnwrap(requests[0].body)) as? [String: Any])
        XCTAssertEqual(object["device"] as? String, "mac")
        XCTAssertEqual(object["text"] as? String, "yes")
        XCTAssertEqual(object["attachments"] as? [String], ["file-1"])
        XCTAssertNil(object["option"])
        XCTAssertEqual(requests[1].path, "/v1/tasks/a%2Fb")
        XCTAssertEqual(requests[2].path, "/v1/search?q=a%20b%26c%3F%23%C3%A9")
    }

    func testOrchestratorRecoveryCommandsKeepRunAndOperationIdentity() async throws {
        let operations = WireOperations()
        let api = client(operations)
        _ = try await api.send(AlondraEndpoints.cancelOrchestrator("run-1"), operationID: "stop-1")
        _ = try await api.send(AlondraEndpoints.retryOrchestrator("run-2"), operationID: "retry-2")
        _ = try await api.send(AlondraEndpoints.acknowledgeOrchestrator("run-3"), operationID: "ack-3")
        let requests = await operations.recorded()
        XCTAssertEqual(requests.map(\.path), ["/v1/orchestrator/cancel", "/v1/orchestrator/retry", "/v1/orchestrator/acknowledge"])
        XCTAssertEqual(requests.map(\.operationID), ["stop-1", "retry-2", "ack-3"])
        for (index, request) in requests.enumerated() {
            let body = try XCTUnwrap(JSONSerialization.jsonObject(with: XCTUnwrap(request.body)) as? [String: String])
            XCTAssertEqual(body, ["run_id": "run-\(index + 1)"])
        }
    }

    func testCloseRefusalKeepsStructuredReasonsAndNeverRepeatsMutation() async throws {
        let body = Data(#"{"reasons":[{"kind":"worktree","detail":"uncommitted files"},{"kind":"busy"}]}"#.utf8)
        let operations = WireOperations(status: 409, body: body)
        do {
            _ = try await client(operations).send(AlondraEndpoints.closeNode("node-1", force: false), operationID: "close-1")
            XCTFail("Expected close refusal")
        } catch {
            XCTAssertEqual(error as? APIError, .conflict(kind: nil, detail: nil, reasons: [
                .init(kind: "worktree", detail: "uncommitted files"), .init(kind: "busy", detail: nil),
            ]))
        }
        let requests = await operations.recorded()
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests[0].operationID, "close-1")
        XCTAssertEqual(requests[0].body, Data("{}".utf8))
    }

    func testChangeWaitForwardsCursorAndDurationAndCancelsUnderlyingOperation() async throws {
        let operations = ControlledChangeWait()
        let api = client(operations)
        let task = Task { try await api.waitForChange(afterSnapshotTick: 42, seconds: 10) }
        let parameters = await operations.waitUntilStarted()
        XCTAssertEqual(parameters.0, 42)
        XCTAssertEqual(parameters.1, 10)
        task.cancel()
        do { try await task.value; XCTFail("Expected cancellation") }
        catch is CancellationError {}
        let cancellations = await operations.cancellations
        XCTAssertEqual(cancellations, 1)
    }
}
