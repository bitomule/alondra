import AlondraContracts
import AlondraDomain
import AlondraShared
import AlondraWorkspaceFeature
import Foundation
import Testing

/// Details the apps showed before domain v2, rebuilt from the v2 projection and runtime.
@Suite struct RestoredDetailsIntegrationTests {
    private static let node = #"{"session_id":"agent-1","short_id":"a1","name":"Node","is_orchestrator":false,"has_plugin":true,"state":"working","created_at":1,"state_since":1,"state_source":"hooks","busy":true,"waiting_permission":false,"turn":1,"usage_limited":false,"last_seen":1,"last_message":"MESSAGE"}"#

    private static func runtime(lastMessage: String) -> DomainV2.Runtime {
        let text = String(data: try! JSONEncoder().encode(lastMessage), encoding: .utf8)!.dropFirst().dropLast()
        return V2Fixture.runtime(#""nodes":[\#(node.replacingOccurrences(of: "MESSAGE", with: String(text)))]"#)
    }

    @Test func aQuestionShowsWhatTheNodeSaidLastButNotARawToolCall() {
        #expect(Self.runtime(lastMessage: "  Tried both flags  ").node(V2Fixture.agent)?.lastSaid == "Tried both flags")
        #expect(Self.runtime(lastMessage: "mcp__alondra__ask").node(V2Fixture.agent)?.lastSaid == nil)
        #expect(Self.runtime(lastMessage: " ").node(V2Fixture.agent)?.lastSaid == nil)
    }

    @Test func aPendingActionRequestPutsItsTaskOnTheUsersTurn() {
        var fixture = V2Fixture()
        fixture.tasks = [V2Fixture.makeTask("t1"), V2Fixture.makeTask("t2")]
        func action(_ id: String, task: DomainV2.TaskId, pending: Bool) -> DomainV2.InboxItem {
            .init(id: id, kind: "action_request", subject: .interaction(interactionId: .init(rawValue: id)), title: "Rotate the key", body: nil,
                  taskId: task, requestId: V2Fixture.request, agentId: V2Fixture.agent, revision: 1, order: 1, pending: pending,
                  children: [], availableActions: [])
        }
        fixture.inbox = [action("interaction:a", task: "t1", pending: true), action("interaction:b", task: "t2", pending: false)]
        #expect(fixture.workspace.yourTurnTasks == ["t1"])
    }

    @MainActor @Test func aScheduleShowsWhereItsLastRequestStandsAndWhoWorksOnIt() throws {
        var fixture = V2Fixture()
        fixture.tasks = [V2Fixture.makeTask("t1"), V2Fixture.makeTask("t2", state: .done)]
        fixture.runtime = Self.runtime(lastMessage: "")
        let schedule = try JSONDecoder().decode(DomainV2.Schedule.self, from: Data(#"""
        {"id":"s1","title":"Nightly","text":"Check","options":{},"disallowed_tools":[],"frequency":"daily","hour":3,"minute":0,
         "enabled":true,"revision":1,"status":"sent","last_request":"request-1"}
        """#.utf8))
        let presentation = ServiceTaskPresentation(workspace: fixture.workspace)
        let run = presentation.lastRun(of: ScheduledTask(schedule))
        #expect(run.state == "in_progress")
        #expect(run.agents == ["Node"])
        fixture.tasks = [V2Fixture.makeTask("t1", state: .done)]
        #expect(ServiceTaskPresentation(workspace: fixture.workspace).lastRun(of: ScheduledTask(schedule)).state == "done")
        fixture.tasks = []
        #expect(ServiceTaskPresentation(workspace: fixture.workspace).lastRun(of: ScheduledTask(schedule)).state == "defining")
    }
}
