import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

@MainActor
final class NodeDetailFeatureTests: XCTestCase {
    private let node = "d887a34b-249e-4487-a89b-57de38c26372"
    private var key: ViewKey { .node(node) }

    private func document(revision: Int = 6) throws -> ViewDocument {
        try Fixtures.document("node-ios", edit: { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            for index in sections.indices where sections[index]["id"] as? String == "actions" {
                var bar = sections[index]["items"] as? [[String: Any]] ?? []
                var actions = bar[0]["actions"] as? [[String: Any]] ?? []
                for action in actions.indices {
                    let id = actions[action]["id"] as? String ?? ""
                    actions[action]["id"] = id.replacingOccurrences(of: "@6", with: "@\(revision)")
                }
                bar[0]["actions"] = actions
                sections[index]["items"] = bar
            }
            object["sections"] = sections
        })
    }

    private func store(
        requests: LockIsolated<[ActionRequest]> = LockIsolated([]),
        reply: String = #"{"protocol":1,"outcome":"applied","message":"Sent"}"#,
        fail: Bool = false,
        dismissed: LockIsolated<Bool> = LockIsolated(false),
        handedOff: LockIsolated<[ActionRequest]> = LockIsolated([]),
        key: ViewKey? = nil
    ) -> TestStoreOf<NodeDetailFeature> {
        let store = TestStore(initialState: NodeDetailFeature.State(key: key ?? self.key)) {
            CombineReducers {
                NodeDetailFeature()
                Reduce { _, action in
                    if case .delegate(.handOff(let request)) = action { handedOff.withValue { $0.append(request) } }
                    return .none
                }
            }
        } withDependencies: {
            $0.uuid = .incrementing
            $0.dismiss = DismissEffect { dismissed.setValue(true) }
            $0.viewsClient.perform = { request in
                requests.withValue { $0.append(request) }
                if fail { throw URLError(.notConnectedToInternet) }
                return try Fixtures.response(reply)
            }
        }
        store.exhaustivity = .off
        return store
    }

    func testReadsTheViewIntoItsParts() throws {
        let detail = NodeDetail(document: try document())

        XCTAssertEqual(detail.title, "Alondra · iOS")
        XCTAssertEqual(detail.state, "Waiting for an answer")
        XCTAssertEqual(detail.stateTone, .warning)
        XCTAssertEqual(detail.folder, "/demo/Demo Garden/ios")
        XCTAssertEqual(detail.readings.map(\.label), ["Context", "Cost"])
        XCTAssertEqual(detail.readings.map(\.value), ["42% of 1M tokens", "$1.25"])
        XCTAssertEqual(detail.waiting.count, 2)
        XCTAssertEqual(detail.needs?.text, "A decision on the welcome copy")
        XCTAssertEqual(detail.lastMessage?.text, "The simulator pass completed successfully.")
        XCTAssertEqual(detail.lastMessageAt, "2026-09-30T09:58:00Z")
        XCTAssertEqual(detail.now.map(\.title), ["Reading the localization catalog"])
        XCTAssertEqual(detail.now.first?.detail, "Task Check the Spanish strings")
        XCTAssertEqual(detail.message?.id, "msg:\(node)@6")
        XCTAssertEqual(detail.close?.id, "close:\(node)@6")
    }

    func testTheFolderIsTheFactThatIsAPathWhateverTheLanguage() throws {
        let document = try Fixtures.document("node-ios", edit: { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            var facts = (sections[0]["items"] as? [[String: Any]]) ?? []
            facts[0]["items"] = [["label": "Estado", "value": "Trabajando"], ["label": "Tareas", "value": "2"],
                                 ["label": "Carpeta", "value": "/Users/x/proyecto"]]
            sections[0]["items"] = facts
            object["sections"] = sections
        })

        let detail = NodeDetail(document: document)

        XCTAssertEqual(detail.state, "Trabajando")
        XCTAssertEqual(detail.folder, "/Users/x/proyecto")
    }

    func testReadsTheBackgroundWorkAndFlagsASuspectOne() throws {
        let document = try Fixtures.document("node-ios", edit: { object in
            var sections = object["sections"] as? [[String: Any]] ?? []
            sections.append(["id": "background", "title": "Running in the background", "collapsed": false, "items": [[
                "type": "row", "id": "2026-09-30T09:40:00Z", "icon": "history", "title": "cargo watch", "subtitle": "Shell",
                "badges": [["text": "Possibly hung", "tone": "warning"]],
                "trailing": ["kind": "time", "at": "2026-09-30T09:40:00Z", "format": "relative"], "depth": 0, "struck": false,
            ]]])
            object["sections"] = sections
        })

        let background = try XCTUnwrap(NodeDetail(document: document).background.first)

        XCTAssertEqual(background.title, "cargo watch")
        XCTAssertEqual(background.detail, "Shell")
        XCTAssertEqual(background.warning, "Possibly hung")
    }

    func testThePanelOffersTheMessageActionAsSoonAsTheViewLoads() async throws {
        let store = store()

        await store.send(.screen(.loaded(try document())))
        await store.receive(\.composer.present)

        XCTAssertEqual(store.state.composer.action?.id, "msg:\(node)@6")
        XCTAssertEqual(store.state.composer.dataEpoch, "test-epoch")
    }

    func testANewRevisionKeepsWhatYouAreWritingAndSendsToTheCurrentOne() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.composer.present)
        await store.send(.messageChanged("Almost done"))
        await store.send(.composer(.textChanged("Almost done")))

        await store.send(.screen(.loaded(try document(revision: 7))))

        XCTAssertEqual(store.state.composer.action?.id, "msg:\(node)@7")
        XCTAssertEqual(store.state.composer.text, "Almost done")
        XCTAssertEqual(store.state.message, "Almost done")
        await store.send(.send)
        await store.skipReceivedActions()
        XCTAssertEqual(requests.value.map(\.actionId), ["msg:\(node)@7"])
    }

    func testSendingAMessageSendsTheTextAndThenClearsTheComposer() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.composer.present)
        await store.send(.messageChanged("Use the new copy"))

        await store.send(.send)
        await store.receive(\.delegate.completed, "Sent")

        XCTAssertEqual(requests.value.map(\.input.text), ["Use the new copy"])
        XCTAssertEqual(store.state.message, "")
        XCTAssertEqual(store.state.composer.text, "")
        XCTAssertNil(store.state.composer.stamp)
        XCTAssertEqual(store.state.composer.action?.id, "msg:\(node)@6", "the composer stays ready for the next message")
    }

    func testAFailedSendKeepsTheMessageAndSaysSo() async throws {
        let store = store(fail: true)
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.composer.present)
        await store.send(.messageChanged("Use the new copy"))

        await store.send(.send)
        await store.receive(\.delegate.failed, "Couldn’t reach your Mac. Nothing was sent; try again.")

        XCTAssertEqual(store.state.message, "Use the new copy")
        XCTAssertTrue(store.state.canSend)
    }

    func testAMessageLeavesTheComposerAtOnceAndComesBackIfItCannotBeSent() async throws {
        let gate = Gate()
        let store = store()
        store.dependencies.viewsClient.perform = { _ in
            await gate.wait()
            throw ViewsClientError.missingView
        }
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.composer.present)

        await store.send(.messageChanged("Ship it"))
        await store.send(.send)
        await store.receive(\.composer.sent)
        XCTAssertEqual(store.state.message, "", "the composer clears before the daemon answers")
        await store.receive(\.composer.present)
        await store.send(.messageChanged("And then"))
        XCTAssertTrue(store.state.canSend, "the next message does not wait for the first")
        await store.send(.messageChanged(""))

        await gate.open()
        await store.receive(\.composer.settled)
        XCTAssertEqual(store.state.message, "Ship it")
    }

    func testABlankMessageCannotBeSent() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let store = store(requests: requests)
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.composer.present)
        await store.send(.messageChanged("  \n "))

        XCTAssertFalse(store.state.canSend)
        await store.send(.send)
        XCTAssertTrue(requests.value.isEmpty)
    }

    func testClosingTheNodeAsksFirstAndThenLeavesAtOnceHandingTheCloseToTheApp() async throws {
        let requests = LockIsolated<[ActionRequest]>([])
        let handedOff = LockIsolated<[ActionRequest]>([])
        let dismissed = LockIsolated(false)
        let store = store(requests: requests, dismissed: dismissed, handedOff: handedOff)
        await store.send(.screen(.loaded(try document())))
        await store.receive(\.composer.present)
        let close = try XCTUnwrap(store.state.detail?.close)

        await store.send(.perform(close))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.actions.confirmation?.title, "Close Node")
        XCTAssertTrue(handedOff.value.isEmpty)
        XCTAssertFalse(dismissed.value)

        await store.send(.actions(.confirm))
        await store.skipReceivedActions()

        XCTAssertEqual(handedOff.value.map(\.actionId), [close.id])
        XCTAssertTrue(requests.value.isEmpty, "the app delivers the close; the screen never waits for it")
        XCTAssertTrue(dismissed.value)
    }

    func testTheTaskAndTheWaitingCardOpenTheirScreens() async {
        let store = store()

        await store.send(.follow(.view(key: "task:t-1")))
        await store.receive(\.delegate.open, ViewKey(rawValue: "task:t-1"))
        await store.send(.follow(.url(url: "https://example.com")))
    }
}
