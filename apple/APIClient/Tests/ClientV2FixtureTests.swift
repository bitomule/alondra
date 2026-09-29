import AlondraAPIClient
import AlondraContracts
import Foundation
import XCTest

/// The routes the apps call next to the domain, checked against the fixtures the daemon's own
/// route tests serve, so a renamed path or field fails here and not on a device.
final class ClientV2FixtureTests: XCTestCase {
    private actor Recorder: ServiceOperationTransport {
        private var requests: [ServiceRequest] = []
        let body: Data
        init(_ body: Data) { self.body = body }
        func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
            requests.append(request)
            return .completed(status: 200, body: body)
        }
        func only() -> ServiceRequest? { requests.count == 1 ? requests[0] : nil }
    }

    private func fixture(_ name: String) throws -> Data {
        let env = ProcessInfo.processInfo.environment
        let root: URL
        if let runfiles = env["TEST_SRCDIR"] {
            root = URL(fileURLWithPath: runfiles).appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
        } else {
            root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
                .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        }
        return try Data(contentsOf: root.appendingPathComponent("fixtures/client-v2").appendingPathComponent(name))
    }

    private func route(_ name: String) throws -> [String: Any] {
        let all = try XCTUnwrap(JSONSerialization.jsonObject(with: fixture("requests.json")) as? [String: Any])
        return try XCTUnwrap(all[name] as? [String: Any])
    }

    /// Calls the client against the fixture's response and checks the request it sent.
    private func check(_ name: String, call: (AlondraAPIClient) async throws -> Void) async throws {
        let route = try route(name)
        let response = try JSONSerialization.data(withJSONObject: route["response"] ?? [:])
        let recorder = Recorder(response)
        try await call(AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "mac", operations: recorder))
        let sent = await recorder.only()
        let request = try XCTUnwrap(sent, "\(name) sends exactly one request")
        XCTAssertEqual(request.method, route["method"] as? String, name)
        XCTAssertEqual(request.path, route["path"] as? String, name)
        if let expected = route["body"] {
            let body = try JSONSerialization.jsonObject(with: XCTUnwrap(request.body, name))
            XCTAssertEqual(body as? NSDictionary, expected as? NSDictionary, name)
        }
    }

    private func attachments(of name: String) throws -> [DomainV2.AttachmentRef] {
        let body = try XCTUnwrap(route(name)["body"] as? [String: Any])
        return try DomainV2.decode([DomainV2.AttachmentRef].self, from: JSONSerialization.data(withJSONObject: body["attachments"] ?? []))
    }

    func testRuntimeKeepsIDKeyedMapsAsTheDaemonSendsThem() throws {
        let runtime = try DomainV2.Runtime.decode(from: fixture("runtime.json"))
        XCTAssertEqual(runtime.task("task-1")?.project, "App")
        XCTAssertEqual(runtime.task("task-1")?.stateSince, 1000)
        XCTAssertEqual(runtime.request("request-2")?.answer, "Nothing to change: it already works")
        XCTAssertEqual(runtime.request("request-1")?.createdAt, 1000)
        let pr = try XCTUnwrap(runtime.pullRequest("pr-1"))
        XCTAssertTrue(pr.hasConflicts)
        XCTAssertEqual(pr.checks.map(\.failed), [false, true])
        XCTAssertEqual(runtime.inboxTime("interaction:info-1"), 1000)
        XCTAssertEqual(runtime.node("agent-a")?.lastMessage, "Running the suite")
        XCTAssertNotNil(runtime.machine)
        XCTAssertNotNil(runtime.usage)
    }

    func testMessageToAnAgent() async throws {
        let attachments = try attachments(of: "message")
        try await check("message") { api in
            let receipt = try await api.sendAgentMessage("agent-a", AgentMessage(kind: .prChanges, text: "Rename it", attachments: attachments, prId: "pr-1"),
                                                         operationID: "op")
            XCTAssertEqual(receipt.deliveryState, "handed")
        }
    }

    func testCloseAnAgent() async throws {
        try await check("close") { try await $0.closeAgent("agent-a", force: true, operationID: "op") }
    }

    func testNewRequest() async throws {
        let attachments = try attachments(of: "new_request")
        try await check("new_request") { api in
            let outcome = try await api.createRequest(NewRequestSubmission(requestID: "request-9", text: "Ship it", project: "App",
                                                                           options: ["effort": .value("hard")], issue: "me/App#3",
                                                                           attachments: attachments), operationID: "op")
            XCTAssertEqual(outcome.revision, 12)
        }
    }

    func testReads() async throws {
        try await check("activity") { let activity = try await $0.domainAgentActivity("agent-a"); XCTAssertEqual(activity.count, 2) }
        try await check("search") { let found = try await $0.domainSearch("parser"); XCTAssertEqual(found.sessions, ["agent-b"]) }
        try await check("options") { api in
            let catalog = try await api.domainOptions(project: "App")
            XCTAssertEqual(catalog.project, "App")
            XCTAssertEqual(catalog.options.map(\.id), ["effort"])
        }
        try await check("projects") { let projects = try await $0.domainProjects(); XCTAssertEqual(projects, ["App", "Web"]) }
        try await check("issues") { let issues = try await $0.domainIssues(refresh: false); XCTAssertEqual(issues.issues.map(\.number), [3]) }
        try await check("history") { let history = try await $0.domainTaskHistory("task-1"); XCTAssertEqual(history.history.map(\.summary), ["Started", "Presented PR #7"]) }
    }
}
