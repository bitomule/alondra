import AlondraShared
import AlondraSync
import AlondraTriageFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct MacTriageIntegrationTests {
    private func snapshot(_ ids: [Int]) throws -> (SyncSnapshot, Overview) {
        let questions = ids.map { id in
            #"{"id":\#(id),"sessionId":"node","title":"Question \#(id)","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":\#(id)}"#
        }.joined(separator: ",")
        let json = #"{"tick":1,"nodes":[],"open_questions":[\#(questions)],"news":[],"orchestrator_messages":[],"machine":{"memory_total":1,"memory_available":1,"load":[0],"disk_available":1,"unreliable":false},"prs":[]}"#
        let body = Data(json.utf8)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return (try SyncSnapshot(body: body), try decoder.decode(Overview.self, from: body))
    }

    @Test
    func twoWindowsReceiveOneProcessProjectionButKeepIndependentCursorAndIdentity() async throws {
        let clientA = AlondraClient(base: URL(string: "http://127.0.0.1:47800")!)
        let services = MacAppSync(client: clientA, commandJournal: nil)
        let first = MacWindowContext(client: clientA, services: services)
        let second = MacWindowContext(client: clientA, services: services)
        let (initial, overview) = try snapshot([1, 2])
        let oldIdentity = SyncIdentity(server: clientA.base, deviceID: clientA.device, credentialRevision: "unpaired")
        let current = SyncState(identity: oldIdentity, generation: 0, connection: .online, snapshot: initial)

        let firstUpdate = try #require(first.bridge.apply(current, overview: overview))
        let secondUpdate = try #require(second.bridge.apply(current, overview: overview))
        first.feature.send(.received(firstUpdate))
        second.feature.send(.received(secondUpdate))
        first.feature.send(.binding(.set(\.selection, .item("q2"))))
        #expect(first.feature.selection == .item("q2"))
        #expect(second.feature.selection == .item("q1"))
        first.triage.send(.focus("q1"))
        first.triage.send(.skip("q1"))
        #expect(first.triage.queue.map(\.id) == ["q2", "q1"])
        #expect(second.triage.queue.map(\.id) == ["q1", "q2"])
        #expect(first.triage.identity == second.triage.identity)

        let oldTriageIdentity = try #require(first.triage.identity)
        let clientB = AlondraClient(base: URL(string: "http://127.0.0.1:47801")!)
        let scopeB = services.prepareUse(clientB)
        #expect(scopeB.generation == 1)
        #expect(await services.mutateTriage(.markRead(1), itemID: "r1", identity: oldTriageIdentity) == .obsolete)
        #expect(first.feature.selection == nil && second.feature.selection == nil)
        #expect(first.triage.queue.isEmpty && second.triage.queue.isEmpty)
        #expect(first.bridge.apply(current, overview: overview) == nil)
        #expect(second.bridge.apply(current, overview: overview) == nil)

        let (newSnapshot, newOverview) = try snapshot([3])
        let newIdentity = SyncIdentity(server: clientB.base, deviceID: clientB.device, credentialRevision: "unpaired")
        let next = SyncState(identity: newIdentity, generation: 1, connection: .online, snapshot: newSnapshot)
        #expect(first.bridge.apply(next, overview: newOverview)?.inboxIDs == ["q3"])
        #expect(second.bridge.apply(next, overview: newOverview)?.inboxIDs == ["q3"])
    }
}
