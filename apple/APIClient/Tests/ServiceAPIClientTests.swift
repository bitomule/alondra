import AlondraAPIClient
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
