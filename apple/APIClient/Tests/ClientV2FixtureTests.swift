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
        XCTAssertEqual(runtime.orchestrator?.activeRunId, "run-7")
        XCTAssertEqual(runtime.nodes.first { $0.isOrchestrator }?.orchestratorRuntime?.canRetry, true)
        XCTAssertNotNil(runtime.machine)
        XCTAssertNotNil(runtime.usage)
    }

    func testTheOrchestratorNodeCarriesTheRuntimesOrchestratorStatus() throws {
        let body = #"""
        {"orchestrator":{"phase":"failed","can_retry":true,"can_cancel":false,"active_run_id":"run-3","queue_depth":1},
         "nodes":[{"session_id":"alondra-orchestrator","short_id":"o","is_orchestrator":true,"has_plugin":true,"state":"working","created_at":1,
                   "state_since":1,"state_source":"daemon","busy":true,"waiting_permission":false,"turn":1,"usage_limited":false,"last_seen":1}]}
        """#
        let runtime = try DomainV2.Runtime.decode(from: Data(body.utf8))
        XCTAssertEqual(runtime.nodes.first?.orchestratorRuntime?.activeRunId, "run-3")
        XCTAssertEqual(runtime.nodes.first?.orchestratorRuntime?.canRetry, true)
    }

    func testNewCommandKindsAndUnknownOnesDecode() throws {
        func decode(_ json: String) throws -> DomainV2.Command { try JSONDecoder().decode(DomainV2.Command.self, from: Data(json.utf8)) }
        XCTAssertEqual(try decode(#"{"kind":"retitle_task","task_id":"t","title":"New"}"#), .retitleTask(taskId: "t", title: "New"))
        XCTAssertEqual(try decode(#"{"kind":"withdraw_question","interaction_id":"q"}"#), .withdrawQuestion(interactionId: "q"))
        XCTAssertEqual(try decode(#"{"kind":"close_request","request_id":"r","answer":"Done already","title":"Fix"}"#),
                       .closeRequest(requestId: "r", answer: "Done already", title: "Fix"))
        XCTAssertEqual(try decode(#"{"kind":"launch_rocket","x":1}"#), .unknown(kind: "launch_rocket"))
        let withdraw = try JSONSerialization.jsonObject(with: JSONEncoder().encode(DomainV2.Command.closeRequest(requestId: "r", withdrawn: true)))
        XCTAssertEqual(withdraw as? NSDictionary, ["kind": "close_request", "request_id": "r", "withdrawn": true])
        XCTAssertThrowsError(try JSONEncoder().encode(DomainV2.Command.unknown(kind: "launch_rocket")))
    }

    func testEffectsFromNewerDaemonsDecode() throws {
        func decode(_ json: String) throws -> DomainV2.DomainEffect { try JSONDecoder().decode(DomainV2.DomainEffect.self, from: Data(json.utf8)) }
        XCTAssertEqual(try decode(#"{"kind":"notify_orchestrator","request_id":"r"}"#), .notifyOrchestrator(interactionId: nil, requestId: "r"))
        XCTAssertEqual(try decode(#"{"kind":"notify_orchestrator","interaction_id":"q"}"#), .notifyOrchestrator(interactionId: "q", requestId: nil))
        XCTAssertEqual(try decode(#"{"kind":"notify_user","task_id":null,"interaction_id":"i"}"#), .notifyUser(taskId: nil, interactionId: "i"))
        XCTAssertEqual(try decode(#"{"kind":"notify_moon"}"#), .unknown(kind: "notify_moon"))
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
            XCTAssertEqual(catalog.options.map(\.id), ["pr", "effort"])
        }
        try await check("projects") { let projects = try await $0.domainProjects(); XCTAssertEqual(projects, ["App", "Web"]) }
        try await check("issues") { let issues = try await $0.domainIssues(refresh: false); XCTAssertEqual(issues.issues.map(\.number), [3]) }
        try await check("history") { let history = try await $0.domainTaskHistory("task-1"); XCTAssertEqual(history.history.map(\.summary), ["Started", "Presented PR #7"]) }
    }
}
