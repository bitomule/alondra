import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

final class RequestDetailTests: XCTestCase {
    func testTheRequestShowsItsStateTasksAndText() throws {
        let detail = RequestDetail(document: try Samples.document("request-detail"))

        XCTAssertEqual(detail.title, "Polish the Alondra beta")
        XCTAssertEqual(detail.state, "Active")
        XCTAssertEqual(detail.tasksTitle, "Tasks")
        XCTAssertEqual(detail.tasks.map(\.id), ["t-1", "t-2", "t-3"])
        XCTAssertEqual(detail.tasks.map(\.waiting), [4, 1, 0])
        XCTAssertEqual(detail.tasks.first?.link, .view(key: "task:t-1"))
        XCTAssertTrue(try XCTUnwrap(detail.text).hasPrefix("Prepare the next Alondra beta"))
        XCTAssertEqual(detail.withdraw?.id, "withdraw:r-1@4")
    }

    func testAFinishedRequestOffersNoWithdraw() throws {
        let document = try Fixtures.document("request-ios")

        XCTAssertNotNil(RequestDetail(document: document).withdraw, "an active request can be withdrawn")

        let finished = try Samples.document("request-detail") { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            sections.removeAll { $0["id"] as? String == "actions" }
            object["sections"] = sections
        }
        XCTAssertNil(RequestDetail(document: finished).withdraw)
    }

    @MainActor
    func testWithdrawingAsksForConfirmationThenSendsTheDaemonsAction() async throws {
        let sent = SentActions()
        let document = try Samples.document("request-detail")
        let store = TestStore(initialState: RequestDetailFeature.State(key: ViewKey(rawValue: "request:r-1"))) {
            RequestDetailFeature()
        } withDependencies: {
            $0.viewsClient.observe = ViewsClient.showing(document)
            $0.viewsClient.perform = { request in sent.record(request); return try Responses.applied("Withdrawn") }
            $0.uuid = .incrementing
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.finish()
        await store.skipReceivedActions()

        await store.send(.withdraw)
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.message, "Withdraw this request?")
        XCTAssertTrue(sent.requests.isEmpty)

        await store.send(.actions(.confirm))
        await store.receive(\.delegate.completed, "Withdrawn")
        XCTAssertEqual(sent.requests.map(\.actionId), ["withdraw:r-1@4"])
    }

    @MainActor
    func testATaskRowOpensTheTask() async throws {
        let store = TestStore(initialState: RequestDetailFeature.State(key: ViewKey(rawValue: "request:r-1"))) {
            RequestDetailFeature()
        }
        store.exhaustivity = .off

        await store.send(.open(.view(key: "task:t-1")))
        await store.receive(\.delegate.open, ViewKey(rawValue: "task:t-1"))
    }
}
