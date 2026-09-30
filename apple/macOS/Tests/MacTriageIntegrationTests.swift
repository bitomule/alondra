import AlondraShared
import AlondraWorkspaceFeature
import AlondraTriageFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct MacTriageIntegrationTests {
    private func snapshot(_ ids: [UInt64]) -> DomainWorkspace {
        let interactions = ids.map { id in
            DomainV2.Interaction(attachments: [], id: .init(rawValue: "q\(id)"), scope: .task(taskId: "t"), author: .agent(agentId: "node"),
                                 recipient: .user, body: "Question \(id)", blocking: true, subject: nil,
                                 payload: .question(options: [], answer: nil), revision: id, order: id)
        }
        let inbox = ids.map { id in
            DomainV2.InboxItem(id: "interaction:q\(id)", kind: "question", subject: .interaction(interactionId: .init(rawValue: "q\(id)")),
                               title: "Question \(id)", body: nil, taskId: "t", requestId: "r", agentId: "node", revision: id, order: id,
                               pending: true, children: [],
                               availableActions: [.init(kind: "answer_question", enabled: true, disabledReason: nil, expectedRevision: id,
                                                        presentationId: nil, expectedHead: nil)])
        }
        return DomainWorkspace(.init(protocolVersion: 2, dataEpoch: "epoch", revision: ids.max() ?? 1, inbox: inbox,
                                     counts: .init(roots: UInt64(ids.count), pending: UInt64(ids.count), unreadInformation: 0),
                                     tasks: [], agents: [], requests: [], interactions: interactions, pullRequests: [], documents: [],
                                     documentRevisions: [], documentReviews: [], mergeAuthorizations: [], stackReadiness: []))
    }

    @Test
    func sharedProjectionKeepsWindowCursorAndNavigationIndependentAndRejectsOldIdentity() async throws {
        let first = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() }
        let second = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() }
        let firstNavigation = first.scope(state: \.macPresentation, action: \.macPresentation)
        let secondNavigation = second.scope(state: \.macPresentation, action: \.macPresentation)
        let server = try #require(URL(string: "https://service.example.test"))
        let old = TriageIdentity(server: server, deviceID: "mac", credentialRevision: "old", generation: 0)
        let replacement = TriageIdentity(server: server, deviceID: "mac", credentialRevision: "new", generation: 1)
        let update = DomainWorkspaceUpdate(identity: old, workspace: snapshot([1, 2]), connection: .online)
        for store in [first, second] {
            await store.send(.triage(.triage(.identityChanged(old)))).finish()
            await store.send(.triage(.triage(.workspaceUpdated(update)))).finish()
        }
        let initialIDs = first.triage.triage.queue.map(\.id)
        #expect(Set(initialIDs) == ["interaction:q1", "interaction:q2"])
        #expect(second.triage.triage.queue.map(\.id) == initialIDs)
        firstNavigation.send(.select(.viewKey("pull_request:p2")))
        secondNavigation.send(.select(.viewKey("node:worker")))
        #expect(firstNavigation.selection == .viewKey("pull_request:p2"))
        #expect(secondNavigation.selection == .viewKey("node:worker"))
        let front = try #require(initialIDs.first)
        await first.send(.triage(.triage(.skip(front)))).finish()
        #expect(first.triage.triage.queue.map(\.id) == Array(initialIDs.dropFirst()) + [front])
        #expect(second.triage.triage.queue.map(\.id) == initialIDs)
        #expect(first.triage.triage.identity == second.triage.triage.identity)

        for store in [first, second] {
            await store.send(.triage(.triage(.identityChanged(replacement)))).finish()
            #expect(store.triage.triage.queue.isEmpty)
            #expect(store.triage.cards.isEmpty)
            await store.send(.triage(.triage(.workspaceUpdated(update)))).finish()
            #expect(store.triage.triage.queue.isEmpty, "An old projection cannot repopulate the new identity")
            let next = DomainWorkspaceUpdate(identity: replacement, workspace: snapshot([3]), connection: .online)
            await store.send(.triage(.triage(.workspaceUpdated(next)))).finish()
            #expect(store.triage.triage.queue.map(\.id) == ["interaction:q3"])
            #expect(store.triage.cards.map(\.id) == ["interaction:q3"])
        }
    }
}
