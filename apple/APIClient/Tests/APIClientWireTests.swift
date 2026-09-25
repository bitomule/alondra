import AlondraAPIClient
import Foundation
import XCTest

private struct EchoSigner: RequestSigner {
    let deviceID = "test-device"
    func signature(for message: Data) throws -> Data { message }
}

private actor RecordingTransport: HTTPTransport {
    var requests: [(URLRequest, RequestPolicy)] = []
    var uploadBodies: [Data] = []
    let response: HTTPResponse

    init(_ response: HTTPResponse) { self.response = response }

    func send(_ request: URLRequest, policy: RequestPolicy) async throws -> HTTPResponse {
        requests.append((request, policy))
        return response
    }

    func upload(_ request: URLRequest, body: Data, progress: @escaping @Sendable (Double) -> Void) async throws -> HTTPResponse {
        requests.append((request, .upload))
        uploadBodies.append(body)
        progress(1)
        return response
    }

    func recorded() -> [(URLRequest, RequestPolicy)] { requests }
    func bodies() -> [Data] { uploadBodies }
}

private actor WaitingTransport: HTTPTransport {
    private var started = false
    private var waiter: CheckedContinuation<Void, Never>?

    func waitUntilStarted() async {
        if started { return }
        await withCheckedContinuation { continuation in waiter = continuation }
    }

    func send(_ request: URLRequest, policy: RequestPolicy) async throws -> HTTPResponse {
        started = true
        waiter?.resume()
        waiter = nil
        try await Task.sleep(for: .seconds(30))
        return .init(status: 200, body: Data())
    }

    func upload(_ request: URLRequest, body: Data, progress: @escaping @Sendable (Double) -> Void) async throws -> HTTPResponse {
        try await Task.sleep(for: .seconds(30))
        return .init(status: 200, body: Data())
    }
}

final class APIClientWireTests: XCTestCase {
    private let base = URL(string: "https://example.invalid/api")!

    func testSignedLongPollPreservesQueryAndLane() async throws {
        let transport = RecordingTransport(.init(status: 200, body: Data()))
        let instant = Date(timeIntervalSince1970: 1_700_000_000)
        let client = AlondraAPIClient(base: base, signer: EchoSigner(), transport: transport, now: { instant })
        _ = try await client.send(AlondraEndpoints.waitForChange(after: 42))
        let calls = await transport.recorded()
        XCTAssertEqual(calls.count, 1)
        XCTAssertEqual(calls[0].1.timeout, 70)
        let request = calls[0].0
        XCTAssertEqual(request.url?.absoluteString, "https://example.invalid/api/v1/changes?after=42&wait=50")
        XCTAssertEqual(request.value(forHTTPHeaderField: RequestSignature.deviceHeader), "test-device")
        XCTAssertEqual(request.value(forHTTPHeaderField: RequestSignature.timestampHeader), "1700000000000")
        let signed = Data(base64Encoded: try XCTUnwrap(request.value(forHTTPHeaderField: RequestSignature.signatureHeader)))!
        XCTAssertEqual(signed, RequestSignature.message(method: "GET", target: "/api/v1/changes?after=42&wait=50", timestamp: 1_700_000_000_000, body: Data()))
    }

    func testUploadSignsExactBytesAndEncodedName() async throws {
        let transport = RecordingTransport(.init(status: 200, body: Data(#"{"id":"stored"}"#.utf8)))
        let instant = Date(timeIntervalSince1970: 1_700_000_000)
        let client = AlondraAPIClient(base: base, signer: EchoSigner(), transport: transport, now: { instant })
        let bytes = Data([0, 1, 255, 42])
        let id = try await client.upload(bytes, name: "a b&c.txt")
        XCTAssertEqual(id, "stored")
        let bodies = await transport.bodies()
        XCTAssertEqual(bodies, [bytes])
        let recorded = await transport.recorded()
        let request = try XCTUnwrap(recorded.first?.0)
        XCTAssertEqual(request.url?.absoluteString, "https://example.invalid/api/v1/uploads?name=a%20b%26c.txt")
        XCTAssertNil(request.httpBody)
        let signed = Data(base64Encoded: try XCTUnwrap(request.value(forHTTPHeaderField: RequestSignature.signatureHeader)))!
        XCTAssertEqual(signed, RequestSignature.message(method: "POST", target: "/api/v1/uploads?name=a%20b%26c.txt", timestamp: 1_700_000_000_000, body: bytes))
    }

    func testEmpty404AndJSON404RemainDistinct() async throws {
        let empty = AlondraAPIClient(base: base, transport: RecordingTransport(.init(status: 404, body: Data())))
        do {
            _ = try await empty.send(AlondraEndpoints.nodeActivity("node"))
            XCTFail("Expected the empty 404 used by the activity fallback")
        } catch let error as APIError {
            XCTAssertEqual(error, .unexpected(status: 404))
        }
        let json = AlondraAPIClient(base: base, transport: RecordingTransport(.init(status: 404, body: Data(#"{"error":"gone"}"#.utf8))))
        do {
            _ = try await json.send(AlondraEndpoints.nodeActivity("node"))
            XCTFail("Expected server JSON error")
        } catch let error as APIError {
            XCTAssertEqual(error, .server(status: 404, kind: nil, detail: "gone", reasons: []))
        }
        let emptyMessage = AlondraAPIClient(base: base, transport: RecordingTransport(.init(status: 404, body: Data(#"{"error":""}"#.utf8))))
        do {
            _ = try await emptyMessage.send(AlondraEndpoints.nodeActivity("node"))
            XCTFail("Expected JSON 404 to bypass the legacy activity fallback")
        } catch let error as APIError {
            XCTAssertEqual(error, .server(status: 404, kind: nil, detail: "", reasons: []))
        }
    }

    func testMutationIsSentOnceOnTransportFailure() async throws {
        let transport = RecordingTransport(.init(status: 503, body: Data()))
        let client = AlondraAPIClient(base: base, transport: transport)
        do {
            _ = try await client.send(AlondraEndpoints.activity(client: "ios"))
            XCTFail("Expected unavailable")
        } catch let error as APIError {
            XCTAssertEqual(error, .unavailable)
        }
        let recorded = await transport.recorded()
        XCTAssertEqual(recorded.count, 1)
    }

    func testAnswerKeepsCallingDeviceAndEscapesPathSegment() async throws {
        let transport = RecordingTransport(.init(status: 200, body: Data()))
        let client = AlondraAPIClient(base: base, device: "mac", transport: transport)
        _ = try await client.send(AlondraEndpoints.answer(7, text: "yes", option: nil, attachments: ["file-1"], device: client.device))
        _ = try await client.send(AlondraEndpoints.task("a/b"))
        let recorded = await transport.recorded()
        let body = try XCTUnwrap(recorded[0].0.httpBody)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertEqual(object["device"] as? String, "mac")
        XCTAssertEqual(object["text"] as? String, "yes")
        XCTAssertNil(object["option"])
        XCTAssertEqual(recorded[1].0.url?.absoluteString, "https://example.invalid/api/v1/tasks/a%2Fb")
    }

    func testAuthorizationConflictAndGatewayResponses() async throws {
        let cases: [(Int, Data, APIError)] = [
            (401, Data(), .unauthorized),
            (403, Data(#"{"kind":"permission","error":"device revoked"}"#.utf8), .forbidden(kind: "permission", detail: "device revoked", reasons: [])),
            (409, Data(#"{"kind":"task_refused","error":"closed"}"#.utf8), .conflict(kind: "task_refused", detail: "closed", reasons: [])),
            (502, Data(), .unavailable),
            (503, Data(#"{"error":"daemon reply"}"#.utf8), .server(status: 503, kind: nil, detail: "daemon reply", reasons: [])),
        ]
        for (status, body, expected) in cases {
            let client = AlondraAPIClient(base: base, transport: RecordingTransport(.init(status: status, body: body)))
            do {
                _ = try await client.send(AlondraEndpoints.health())
                XCTFail("Expected HTTP \(status) failure")
            } catch let error as APIError {
                XCTAssertEqual(error, expected)
            }
        }
    }

    func testCloseNodeConflictCarriesStructuredReasonsForPresentation() async throws {
        let body = Data(#"{"reasons":[{"kind":"worktree","detail":"uncommitted files"},{"kind":"busy"}]}"#.utf8)
        let transport = RecordingTransport(.init(status: 409, body: body))
        let client = AlondraAPIClient(base: base, transport: transport)
        do {
            _ = try await client.send(AlondraEndpoints.closeNode("node-1", force: false))
            XCTFail("Expected close refusal")
        } catch let error as APIError {
            XCTAssertEqual(error, .conflict(kind: nil, detail: nil, reasons: [
                .init(kind: "worktree", detail: "uncommitted files"),
                .init(kind: "busy", detail: nil),
            ]))
        }
        let recorded = await transport.recorded()
        XCTAssertEqual(recorded.count, 1)
        XCTAssertEqual(recorded[0].1.timeout, 70)
        XCTAssertEqual(recorded[0].0.httpBody, Data("{}".utf8))
    }

    func testNonSuccessRedirectAndNotModifiedCannotPassAsDaemonResponse() async throws {
        for status in [302, 304] {
            let transport = RecordingTransport(.init(status: status, body: Data()))
            let client = AlondraAPIClient(base: base, transport: transport)
            do {
                _ = try await client.send(AlondraEndpoints.health())
                XCTFail("Expected HTTP \(status) to fail")
            } catch let error as APIError {
                XCTAssertEqual(error, .unexpected(status: status))
            }
        }
    }

    func testCancellingLongPollStopsTransportWait() async throws {
        let transport = WaitingTransport()
        let client = AlondraAPIClient(base: base, transport: transport)
        let task = Task { try await client.send(AlondraEndpoints.waitForChange(after: 1)) }
        await transport.waitUntilStarted()
        task.cancel()
        do {
            _ = try await task.value
            XCTFail("Expected cancellation")
        } catch is CancellationError {
            // The transport sleep was cancelled by the caller's task.
        }
    }
}
