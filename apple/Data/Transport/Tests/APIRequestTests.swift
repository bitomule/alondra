import Foundation
import XCTest
@testable import AlondraTransport

final class APIRequestTests: XCTestCase {
    private let common = [("protocol", "1"), ("client", "ios"), ("lang", "es")]

    private func request(_ key: String, extra: [(String, String)] = []) throws -> APIRequest {
        try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["views", key], query: common + extra),
                           operationID: UUID().uuidString)
    }

    func testEveryShippingViewFamilyIsAccepted() throws {
        for key in ["home", "sidebar", "tasks", "fleet", "schedules", "machine", "new_request",
                    "issue_picker", "search", "card:i-1", "task:hf-weekly-2026-09-30",
                    "request:r-1", "document:rev-1", "node:550e8400-e29b-41d4-a716-446655440000",
                    "schedule:hf-weekly-2026-09-30", "pull_request:github.com/bitomule/alondra#154",
                    "pull_request:gitlab.example.com/bitomule/alondra#154"] {
            XCTAssertTrue(try request(key).path.hasPrefix("/views/"), key)
        }
        XCTAssertNoThrow(try request("tasks", extra: [("section", "active"), ("after", "20")]))
        XCTAssertNoThrow(try request("home", extra: [("focus", "i-1")]))
        XCTAssertNoThrow(try request("issue_picker", extra: [("refresh", "1")]))
        XCTAssertNoThrow(try request("schedule:new", extra: [("project", "App")]))
    }

    func testUnsafeViewKeysAndQueriesAreRejected() {
        for key in ["card:../secret", "node:", "pull_request:github.com/owner/repo#0",
                    "pull_request:github.com/Owner/repo#154",
                    "pull_request:github.com/owner/repo#154/extra", "unknown:thing"] {
            XCTAssertThrowsError(try request(key), key)
        }
        XCTAssertThrowsError(try request("tasks", extra: [("section", "invalid"), ("after", "20")]))
        XCTAssertThrowsError(try request("fleet", extra: [("after", "20")]))
        XCTAssertThrowsError(try request("home", extra: [("focus", "../secret")]))
    }
}

final class McpSignInRouteTests: XCTestCase {
    func testOnlyTheStepsOfAnMcpWaitsCardAreSentToTheMac() {
        for step in ["connect", "return", "reconnect", "without"] {
            XCTAssertTrue(APIRequest.validClientContractRoute(method: .post, path: ["mcp-sign-ins", "mcp-sign-in-t-7-12", step]), step)
        }
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .get, path: ["mcp-sign-ins", "mcp-sign-in-t-7", "connect"]))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .post, path: ["mcp-sign-ins", "mcp-sign-in-t-7", "open"]))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .post, path: ["mcp-sign-ins", "card-1", "connect"]))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .post, path: ["mcp-sign-ins", "mcp-sign-in-t.7", "connect"]))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .post, path: ["mcp-sign-ins", "mcp-sign-in-t-7"]))
    }

    func testAServerOfTheMcpServersScreenIsSignedInToButNeverGoneOnWithout() {
        for step in ["connect", "return", "reconnect"] {
            XCTAssertTrue(APIRequest.validClientContractRoute(method: .post, path: ["mcp-sign-ins", "mcp-server-0123456789abcdef", step]), step)
        }
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .post, path: ["mcp-sign-ins", "mcp-server-0123456789abcdef", "without"]))
        XCTAssertTrue(APIRequest.validClientContractRoute(method: .get, path: ["views", "mcp_servers"]))
    }
}

final class OperationErrorTests: XCTestCase {
    func testEveryCaseReadsAsASentenceNotATypeName() {
        let cases: [OperationError] = [.operationIdentityRequired, .invalidConfiguration, .invalidEndpoint, .outcomeUnknown]
        for error in cases {
            XCTAssertFalse(error.localizedDescription.contains("OperationError"), "\(error)")
            XCTAssertTrue(error.localizedDescription.hasSuffix("."), "\(error)")
        }
    }
}
