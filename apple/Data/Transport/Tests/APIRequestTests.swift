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
        for key in ["home", "sidebar", "tasks", "schedules", "machine", "new_request",
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
        XCTAssertThrowsError(try request("schedules", extra: [("after", "20")]))
        XCTAssertThrowsError(try request("fleet"), "the app no longer asks for the fleet")
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

    func testTheClaudeAccountsScreenIsAViewTheAppMayRead() {
        XCTAssertTrue(APIRequest.validClientContractRoute(method: .get, path: ["views", "accounts"]))
    }
}

final class CrashReportRouteTests: XCTestCase {
    func testAReportIsBuiltReadFileByFileAndSentOnlyByItsId() throws {
        let id = "r-20261027-0000abcd"
        XCTAssertTrue(APIRequest.validClientContractRoute(method: .post, path: ["reports"]))
        XCTAssertTrue(APIRequest.validClientContractRoute(method: .post, path: ["reports", id, "send"]))
        XCTAssertTrue(APIRequest.validClientContractRoute(method: .get, path: ["reports", id, "files", "daemon.log"]))
        XCTAssertNoThrow(try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["reports"], body: Data("{}".utf8)), operationID: "report-1"))
        XCTAssertNoThrow(try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["reports", id, "files", "daemon.log"], query: [("offset", "262144")]),
                                        operationID: "read-1"), "a long file is read a page at a time")
        XCTAssertThrowsError(try APIRequest(endpoint: Endpoint<Data>.data(.get, path: ["reports", id, "files", "daemon.log"], query: [("offset", "-1")]),
                                            operationID: "read-2"))
        XCTAssertThrowsError(try APIRequest(endpoint: Endpoint<Data>.data(.post, path: ["reports"], query: [("x", "1")], body: Data("{}".utf8)),
                                            operationID: "report-2"))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .get, path: ["reports"]))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .post, path: ["reports", "r-2026-1", "send"]))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .post, path: ["reports", id, "delete"]))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .get, path: ["reports", id, "files", ".pseudonyms.json"]))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .get, path: ["reports", id, "files", "a/b"]))
        XCTAssertFalse(APIRequest.validClientContractRoute(method: .post, path: ["reports", id, "files", "daemon.log"]))
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
