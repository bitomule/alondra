import AlondraShared
import Foundation
import Testing

struct OrchestratorRuntimeTests {
    @Test func additiveStatusSurvivesOldAndNewDaemonVersions() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let base = #"{"session_id":"alondra-orchestrator","short_id":"orchestrator","is_orchestrator":true,"has_plugin":true,"state":"done","state_since":1,"state_source":"runtime","orchestrator_state":"on_duty"}"#
        let legacy = try decoder.decode(Node.self, from: Data(base.utf8))
        #expect(legacy.orchestratorRuntime == nil)
        #expect(legacy.stateLabel == legacy.orchestratorState?.label)
        var value = try #require(JSONSerialization.jsonObject(with: Data(base.utf8)) as? [String: Any])
        value["orchestrator_runtime"] = ["phase": "a_future_phase", "queue_depth": 3]
        let future = try decoder.decode(Node.self, from: JSONSerialization.data(withJSONObject: value))
        #expect(future.orchestratorRuntime?.phase == .unknown)
        #expect(future.orchestratorRuntime?.queueDepth == 3)
        #expect(future.stateLabel == future.orchestratorRuntime?.label)
    }
}
