import AlondraAPIClient
import AlondraContracts
import AlondraDomain
import AlondraWorkspace
import Foundation
import XCTest

/// Intents the daemon serves as plain agent messages carry the same words the apps typed into
/// the node before domain v2, addressed to the agent that owns the item.
final class ClientRouteTests: XCTestCase {
    private func workspace(orchestrator: Bool = false) throws -> DomainWorkspace {
        let task = DomainV2.Task(id: "t-1", requestId: "r", agentId: "agent-a", title: "Menus", state: .running, revision: 1, started: true,
                                 reviewPrs: false, finalPresentation: nil, prId: "pr-1", dependencies: [], stack: nil, documents: [:], archived: false)
        let pr = DomainV2.PullRequest(id: "pr-1", taskId: "t-1", host: "github.com", repository: "a/b", number: 12, head: "h", base: "main",
                                      state: .open, mergedHead: nil, allowedMethods: [.squash], review: nil)
        func interaction(_ id: DomainV2.InteractionId, _ payload: DomainV2.InteractionPayload) -> DomainV2.Interaction {
            .init(attachments: [], id: id, scope: .task(taskId: "t-1"), author: .agent(agentId: "agent-a"), recipient: .user, body: "Which one?",
                  blocking: true, subject: nil, payload: payload, revision: 1, order: 1)
        }
        let projection = DomainV2.DomainProjection(
            protocolVersion: 2, dataEpoch: "e", revision: 7, inbox: [], counts: .init(roots: 0, pending: 0, unreadInformation: 0),
            tasks: [task], agents: [], requests: [], interactions: [interaction("q", .question(options: [], answer: nil)), interaction("i", .information(readBy: []))],
            pullRequests: [pr], documents: [], documentRevisions: [], documentReviews: [], mergeAuthorizations: [], stackReadiness: [])
        let runtime = try DomainV2.Runtime.decode(from: Data(#"{"nodes":[{"session_id":"alondra-orchestrator","short_id":"o","is_orchestrator":\#(orchestrator),"has_plugin":true,"state":"working","created_at":1,"state_since":1,"state_source":"daemon","busy":true,"waiting_permission":false,"turn":1,"usage_limited":false,"last_seen":1}]}"#.utf8))
        return DomainWorkspace(projection, runtime: runtime)
    }

    func testMoreContextAsksTheAuthorAgainWithWhatIsMissing() throws {
        let route = try ClientRoute.route(for: .requestMoreContext("q", body: "the cost"), in: workspace())
        XCTAssertEqual(route, .message("agent-a", AgentMessage(
            kind: .moreContext,
            text: "I need more context before deciding. What is missing: the cost. Explain what is at stake, the options with their consequences and your recommendation, and ask again.",
            interactionId: "q", taskId: "t-1")))
    }

    func testAReplyToInformationMarksItReadInTheSameStep() throws {
        let route = try ClientRoute.route(for: .replyToInformation("i", body: " Thanks "), in: workspace())
        XCTAssertEqual(route, .message("agent-a", AgentMessage(kind: .reply, text: "Thanks", interactionId: "i", taskId: "t-1", markRead: true)))
        XCTAssertThrowsError(try ClientRoute.route(for: .replyToInformation("i", body: "  "), in: workspace()))
    }

    func testPullRequestQuestionsAndConflictsGoToTheAgentOfItsTask() throws {
        let asked = try ClientRoute.route(for: .askAboutPullRequest("pr-1", head: "h", body: "Why?"), in: workspace())
        XCTAssertEqual(asked, .message("agent-a", AgentMessage(kind: .prQuestion, text: "Why?", prId: "pr-1", taskId: "t-1")))
        let conflicts = try ClientRoute.route(for: .requestConflictResolution("pr-1", head: "h"), in: workspace())
        XCTAssertEqual(conflicts, .message("agent-a", AgentMessage(
            kind: .prChanges,
            text: "Resolve the conflicts of https://github.com/a/b/pull/12 with main: bring main into the branch, resolve them, run the tests and push.",
            prId: "pr-1", taskId: "t-1")))
    }

    func testTheOrchestratorGetsItsOwnRoute() throws {
        let intent = DomainIntent.messageAgent("alondra-orchestrator", body: "Go")
        XCTAssertEqual(try ClientRoute.route(for: intent, in: workspace(orchestrator: true)), .orchestratorMessage(AgentMessage(text: "Go")))
        XCTAssertEqual(try ClientRoute.route(for: intent, in: workspace()), .message("alondra-orchestrator", AgentMessage(text: "Go")))
    }

    func testARequestWithContextUsesTheRequestRouteAndAPlainOneStaysACommand() throws {
        let plain = DomainIntent.createRequest(body: "Ship it", project: nil, options: [:], issue: nil, attachments: [])
        XCTAssertNil(try ClientRoute.route(for: plain, in: workspace()))
        let scoped = DomainIntent.createRequest(body: " Ship it ", project: "App", options: [:], issue: nil, attachments: [])
        XCTAssertEqual(try ClientRoute.route(for: scoped, in: workspace(), newID: { "request-op" }),
                       .request(NewRequestSubmission(requestID: "request-op", text: "Ship it", project: "App", options: [:], issue: nil, attachments: [])))
    }
}
