import AlondraDomain
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct ServiceRootFanOutTests {
    @Test func oneWorkspaceUpdateReachesInboxTasksFleetAndNewRequest() async throws {
        var fixture = V2Fixture()
        let question = fixture.question("q1")
        fixture.pullRequest(5)
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(CommandLog()) { _ in .obsolete }
        }
        await store.send(.triage(.triage(.workspaceUpdated(fixture.update())))).finish()
        #expect(store.triage.triage.queue.map(\.id).contains(question.id))
        #expect(store.triage.cards.map(\.id).contains("pr:pr-5"))
        #expect(store.tasks.taskBoard.task(V2Fixture.task) != nil)
        #expect(store.tasks.board.hasSnapshot)
        #expect(store.tasks.presentation.workspace?.revision == fixture.revision)
        #expect(store.fleet.fleet.snapshot?.node(V2Fixture.agent.rawValue) != nil)
        #expect(store.fleet.workspace?.revision == fixture.revision)
        #expect(store.newRequest.canSubmit)
        #expect(store.newRequest.sentRequests.map(\.id) == [V2Fixture.request])
        #expect(store.newRequest.pickedUp == [V2Fixture.request], "the request already has a Task")

        // An update from another session reaches nothing.
        var other = V2Fixture()
        other.tasks = []
        await store.send(.triage(.triage(.workspaceUpdated(other.update(V2Fixture.other))))).finish()
        #expect(store.tasks.taskBoard.task(V2Fixture.task) != nil)

        // Losing the projection leaves every feature without a snapshot.
        await store.send(.triage(.triage(.workspaceUpdated(DomainWorkspaceUpdate(identity: V2Fixture.identity, workspace: nil,
                                                                               connection: .offline(.macNotAnswering)))))).finish()
        #expect(!store.tasks.board.hasSnapshot)
        #expect(!store.newRequest.canSubmit)
        #expect(store.tasks.connection == .offline(.macNotAnswering))
    }

    @Test func aPullRequestDetailFollowsItsCardAndLeavesWithIt() async throws {
        var fixture = V2Fixture()
        fixture.pullRequest(5)
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.domainWorkspace = .scripted(CommandLog()) { _ in .obsolete }
        }
        await store.send(.triage(.triage(.workspaceUpdated(fixture.update())))).finish()
        await store.send(.focusInbox("pr:pr-5")).finish()
        #expect(store.pullRequestID == "pr-5")
        #expect(store.tab == .inbox)

        // A newer projection of the same PR reaches the open detail.
        fixture.revision = 2
        fixture.pullRequests[0].head = "head-2"
        await store.send(.triage(.triage(.workspaceUpdated(fixture.update())))).finish()
        #expect(store.pullRequest?.pr.pr?.head == "head-2")
        #expect(store.pullRequest?.lastKnownCard.pullRequest.head == "head-2")

        // Closing the detail keeps the owner while the card exists; once the server drops the
        // card, the owner goes too.
        await store.send(.pullRequestDismissed("pr-5")).finish()
        #expect(store.pullRequestID == nil && store.pullRequests[id: "pr-5"] != nil)
        var without = V2Fixture()
        without.revision = 3
        await store.send(.triage(.triage(.workspaceUpdated(without.update())))).finish()
        #expect(store.pullRequests.isEmpty)
    }

    @Test func eachScreenAsksTheClientForWhatItShowsWhenItOpens() async throws {
        let opened = LockIsolated<[DomainScreen]>([])
        var client = DomainWorkspaceClient.scripted(CommandLog()) { _ in .obsolete }
        client.open = { screen in opened.withValue { $0.append(screen) } }
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.domainWorkspace = client
        }
        await store.send(.appeared).finish()
        #expect(opened.value == [.inbox], "The inbox is what the app opens on")

        await store.send(.tabSelected(.tasks)).finish()
        await store.send(.tabSelected(.fleet)).finish()
        await store.send(.tabSelected(.newRequest)).finish()
        await store.send(.tabSelected(.account)).finish()
        await store.send(.tabSelected(.inbox)).finish()
        #expect(opened.value == [.inbox, .tasks, .fleet, .newRequest])

        await store.send(.triage(.machinePresented(true))).finish()
        await store.send(.triage(.machinePresented(false))).finish()
        #expect(opened.value.last == .machine)
        #expect(opened.value.filter { $0 == .machine }.count == 1)
    }

    @Test func openingAPullRequestAsksForItsData() async throws {
        let opened = LockIsolated<[DomainScreen]>([])
        var client = DomainWorkspaceClient.scripted(CommandLog()) { _ in .obsolete }
        client.open = { screen in opened.withValue { $0.append(screen) } }
        var fixture = V2Fixture()
        fixture.pullRequest(5)
        let store = Store(initialState: ServiceRootFeature.State()) { ServiceRootFeature() } withDependencies: {
            $0.domainWorkspace = client
        }
        await store.send(.triage(.triage(.workspaceUpdated(fixture.update())))).finish()
        await store.send(.focusInbox("pr:pr-5")).finish()
        #expect(opened.value == [.pullRequest])
    }
}
