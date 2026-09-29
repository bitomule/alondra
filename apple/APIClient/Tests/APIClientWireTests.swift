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
    func recorded() -> [ServiceRequest] { requests }
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
                _ = try await client(operations).send(Endpoint<Data>.data(.get, path: ["runtime"]))
                XCTFail("Expected service response \(status) to fail")
            } catch { XCTAssertEqual(error as? APIError, expected) }
            let requests = await operations.recorded()
            XCTAssertEqual(requests.count, 1, "Error handling must not replay the operation")
        }
    }

    func testClientContractPathsAndQueriesAreEscaped() async throws {
        let operations = WireOperations(body: Data(#"{"via":"index","sessions":[]}"#.utf8))
        let api = client(operations)
        _ = try await api.domainSearch("a b&c?#é")
        _ = try? await api.domainAgentActivity("a/b")
        let requests = await operations.recorded()
        XCTAssertEqual(requests[0].path, "/agents/search?q=a%20b%26c%3F%23%C3%A9")
        XCTAssertEqual(requests[1].path, "/agents/a%2Fb/activity")
    }

    func testV1RoutesAreNoLongerAdmitted() async throws {
        let operations = WireOperations()
        do {
            _ = try await client(operations).send(Endpoint<Data>.data(.get, path: ["v1", "overview"]))
            XCTFail("The v1 data path is gone")
        } catch { XCTAssertEqual(error as? ServiceAPIError, .invalidEndpoint) }
        let requests = await operations.recorded()
        XCTAssertTrue(requests.isEmpty)
    }
}
