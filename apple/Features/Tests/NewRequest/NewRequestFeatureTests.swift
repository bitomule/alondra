import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

final class NewRequestFeatureTests: XCTestCase {
    @MainActor
    private func loaded(
        sent: SentActions = SentActions(),
        delegates: LockIsolated<[ScreenDelegate]> = LockIsolated([]),
        keys: LockIsolated<[ViewKey]> = LockIsolated([]),
        respond: @escaping @Sendable (ActionRequest) throws -> ActionResponse = { _ in try Responses.applied("Sent to the orchestrator") }
    ) async throws -> TestStoreOf<NewRequestFeature> {
        let document = try Fixtures.document("new-request-ios")
        let probe = Reduce<NewRequestFeature.State, NewRequestFeature.Action> { _, action in
            if case .delegate(let delegate) = action { delegates.withValue { $0.append(delegate) } }
            return .none
        }
        let store = TestStore(initialState: NewRequestFeature.State()) {
            CombineReducers {
                probe
                NewRequestFeature()
            }
        } withDependencies: {
            $0.viewsClient.observe = { key in
                keys.withValue { $0.append(key) }
                return ViewsClient.showing(document)(key)
            }
            $0.viewsClient.perform = { request in sent.record(request); return try respond(request) }
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.finish()
        await store.skipReceivedActions()
        return store
    }

    @MainActor
    func testTheDraftStartsFromTheValuesTheDaemonSent() async throws {
        let store = try await loaded()

        XCTAssertEqual(store.state.values["text"], .string(""))
        XCTAssertEqual(store.state.values["pr"], .string("review"))
        XCTAssertEqual(store.state.values["effort"], .string("auto"))
        XCTAssertNil(store.state.project, "no project means the orchestrator picks")
        XCTAssertEqual(store.state.optionsSummary, ["Review each one", "Auto"])
        XCTAssertEqual(store.state.recent.map(\.title), ["Polish the Alondra beta"])
        XCTAssertFalse(store.state.canSend)
    }

    @MainActor
    func testSendingNeedsSomethingToSay() async throws {
        let sent = SentActions()
        let store = try await loaded(sent: sent)

        await store.send(.send)
        XCTAssertTrue(sent.requests.isEmpty)

        await store.send(.textChanged("   "))
        XCTAssertFalse(store.state.canSend, "spaces are not a request")

        await store.send(.textChanged("Polish the beta"))
        XCTAssertTrue(store.state.canSend)
    }

    @MainActor
    func testSendingSubmitsTheDraftAndClearsTheTextWithAToast() async throws {
        let sent = SentActions()
        let delegates = LockIsolated<[ScreenDelegate]>([])
        let store = try await loaded(sent: sent, delegates: delegates)

        await store.send(.textChanged("Polish the beta"))
        await store.send(.set("effort", .string("hard")))
        await store.send(.send)
        await store.receive(\.delegate.completed)

        let request = try XCTUnwrap(sent.requests.first)
        XCTAssertEqual(request.actionId, "new_request:view@0")
        XCTAssertEqual(request.input.fields?["text"], .string("Polish the beta"))
        XCTAssertEqual(request.input.fields?["effort"], .string("hard"))
        XCTAssertEqual(delegates.value, [.completed("Sent to the orchestrator")])
        XCTAssertEqual(store.state.text, "")
        XCTAssertEqual(store.state.values["effort"], .string("hard"), "the options stay for the next request")
    }

    @MainActor
    func testARefusedRequestKeepsTheDraftAndReportsIt() async throws {
        let delegates = LockIsolated<[ScreenDelegate]>([])
        let store = try await loaded(delegates: delegates) { _ in try Responses.rejected("Alondra can’t take requests right now") }

        await store.send(.textChanged("Polish the beta"))
        await store.send(.send)
        await store.receive(\.delegate.failed)

        XCTAssertEqual(store.state.text, "Polish the beta")
        XCTAssertEqual(delegates.value, [.failed("Alondra can’t take requests right now")])
    }

    @MainActor
    func testChoosingAProjectLoadsTheViewForThatProject() async throws {
        let keys = LockIsolated<[ViewKey]>([])
        let store = try await loaded(keys: keys)

        await store.send(.set("project", .string("Demo Garden")))
        await store.finish()

        XCTAssertEqual(keys.value.last, .newRequest(project: "Demo Garden"))
        XCTAssertEqual(store.state.screen.key, .newRequest(project: "Demo Garden"))
        XCTAssertEqual(store.state.project, "Demo Garden")
    }

    @MainActor
    func testChoosingTheOrchestratorBacktracksToTheViewWithoutAProject() async throws {
        let keys = LockIsolated<[ViewKey]>([])
        let store = try await loaded(keys: keys)
        await store.send(.set("project", .string("Demo Garden")))
        await store.finish()

        await store.send(.set("project", .string("")))
        await store.finish()

        XCTAssertEqual(store.state.screen.key, .newRequest())
        XCTAssertNil(store.state.project)
    }

    @MainActor
    func testOptionChangesFlowBackToTheDraft() async throws {
        let store = try await loaded()

        await store.send(.optionsTapped)
        XCTAssertEqual(store.state.options?.fields.map(\.id), ["project", "pr", "effort", "issue"])

        await store.send(.options(.presented(.choose("pr", "auto_merge"))))
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.values["pr"], .string("auto_merge"))
        XCTAssertEqual(store.state.optionsSummary, ["Auto-merge", "Auto"])
    }

    @MainActor
    func testAPickedIssueBecomesPartOfTheRequestAndCanBeRemoved() async throws {
        let sent = SentActions()
        let store = try await loaded(sent: sent)

        await store.send(.optionsTapped)
        await store.send(.options(.presented(.issueTapped)))
        XCTAssertNotNil(store.state.options?.issuePicker)

        await store.send(.options(.presented(.issuePicker(.presented(.pick("me/App#4"))))))
        await store.skipReceivedActions()

        XCTAssertNil(store.state.options?.issuePicker, "choosing closes the picker")
        XCTAssertEqual(store.state.values["issue"], .string("me/App#4"))
        XCTAssertEqual(store.state.optionsSummary, ["me/App#4", "Review each one", "Auto"])

        await store.send(.textChanged("Fix it"))
        await store.send(.send)
        await store.receive(\.delegate.completed)
        XCTAssertEqual(sent.requests.first?.input.fields?["issue"], .string("me/App#4"))

        await store.send(.options(.presented(.removeIssue)))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.values["issue"], .string(""))
        XCTAssertEqual(store.state.optionsSummary, ["Review each one", "Auto"])
    }

    func testProjectsAreFilteredByEveryWordTyped() throws {
        let projects = try ContractCodec.decode([ChoiceItem].self, from: Data(
            #"[{"id":"","label":"The orchestrator picks the project"},{"id":"Demo Garden","label":"Demo Garden"},{"id":"Lumo","label":"Lumo"}]"#.utf8))

        XCTAssertEqual(ProjectChoices.filter(projects, query: "").count, 3)
        XCTAssertEqual(ProjectChoices.filter(projects, query: "gar dem").map(\.id), ["Demo Garden"])
        XCTAssertEqual(ProjectChoices.filter(projects, query: "orchestrator").map(\.id), [], "the default is not a project to search for")
        XCTAssertTrue(ProjectChoices.filter(projects, query: "zzz").isEmpty)
    }
}
