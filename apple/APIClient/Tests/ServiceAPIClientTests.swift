import AlondraAPIClient
import AlondraContracts
import Foundation
import XCTest

private actor ServiceRecorder: ServiceOperationTransport {
    var requests: [ServiceRequest] = []
    let result: ServiceResponse
    init(_ result: ServiceResponse) { self.result = result }
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        requests.append(request)
        return result
    }
    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {}
    func recorded() -> [ServiceRequest] { requests }
}

final class ServiceAPIClientTests: XCTestCase {
    func testAnswerRejectsMalformedSuccessButUnrelatedByteResponsesAnd204RemainValid() async throws {
        for body in ["not-json", "[]", ""] {
            let transport = ServiceRecorder(.completed(status: 200, body: Data(body.utf8)))
            let api = AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "client", operations: transport)
            do {
                try await api.answer(42, text: "Yes", option: nil, attachments: [], operationID: "original")
                XCTFail("Malformed answer response became success")
            } catch { XCTAssertTrue(error is DeliveryResponseError) }
            // The same transport bytes are legal for an endpoint declaring an opaque payload.
            let bytes = try await api.send(Endpoint<Data>.data(.post, path: ["v1", "opaque"]), operationID: "bytes")
            XCTAssertEqual(bytes, Data(body.utf8))
        }
        let transport = ServiceRecorder(.completed(status: 204, body: Data()))
        let api = AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "client", operations: transport)
        try await api.answer(42, text: "Yes", option: nil, attachments: [], operationID: "empty-no-content")
    }

    func testConfirmedHTTPDoesNotClaimUncertainHandoffAndNeverRepeatsAnswer() async throws {
        for state in ["uncertain", "dispatching", "future-state"] {
            let transport = ServiceRecorder(.completed(status: 200, body: Data("{\"delivery_state\":\"\(state)\",\"delivery_attempt\":77}".utf8)))
            let api = AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "client", operations: transport)
            do {
                try await api.answer(42, text: "Yes", option: "Yes", attachments: [], operationID: "original")
                XCTFail("Unconfirmed handoff became successful answer")
            } catch {
                XCTAssertEqual((error as? DeliveryHandoffUnconfirmed)?.attemptID, 77)
            }
            let sent = await transport.recorded()
            XCTAssertEqual(sent.map(\.operationID), ["original"])
        }
        for body in ["{}", "{\"delivery_state\":\"handed\"}", "{\"delivery_state\":\"queued\"}"] {
            let transport = ServiceRecorder(.completed(status: 200, body: Data(body.utf8)))
            let api = AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "client", operations: transport)
            try await api.answer(42, text: "Yes", option: nil, attachments: [], operationID: "original")
        }
    }

    func testMutationRequiresDurableIdentityAndUnknownIsNeverRetried() async throws {
        let transport = ServiceRecorder(.unknown)
        let api = AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "client-test", operations: transport)
        do {
            _ = try await api.send(AlondraEndpoints.markRead(42))
            XCTFail("Mutation without operation identity reached transport")
        } catch { XCTAssertEqual(error as? ServiceAPIError, .operationIdentityRequired) }
        var requests = await transport.recorded()
        XCTAssertTrue(requests.isEmpty)
        do {
            try await api.markRead(42, operationID: "persisted-command")
            XCTFail("Unknown outcome was reported as success")
        } catch { XCTAssertEqual(error as? ServiceAPIError, .outcomeUnknown) }
        requests = await transport.recorded()
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests.first?.operationID, "persisted-command")
        XCTAssertEqual(requests.first?.path, "/v1/reports/42/read")
    }

    func testLogicalPathCannotBecomeAnotherOrigin() async throws {
        let transport = ServiceRecorder(.completed(status: 200, body: Data("{}".utf8)))
        let api = AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "client-test", operations: transport)
        _ = try await api.task("https://attacker.test/a?b#c")
        let requests = await transport.recorded()
        XCTAssertEqual(requests.first?.path, "/v1/tasks/https%3A%2F%2Fattacker.test%2Fa%3Fb%23c")
    }
}
