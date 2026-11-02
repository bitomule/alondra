import AlondraContract
import AlondraDesign
import AlondraFeed
import AlondraFeatures
import AlondraTransport
import Foundation
import ComposableArchitecture
import Testing

private let status = #"{"version":"s1","summary":"Ready","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}}"#

private func makeDocument(key: String, version: String, sections: String = "[]") throws -> ViewDocument {
    let json = #"{"protocol":1,"view":"\#(key)","key":"\#(key)","client":"mac","version":"\#(version)","cursor":"c1.abc","data_epoch":"epoch","status":\#(status),"sections":\#(sections),"notices":[]}"#
    return try JSONDecoder().decode(ViewDocument.self, from: Data(json.utf8))
}

private func settledCard(_ key: String, why: String) throws -> ViewDocument {
    let json = #"{"protocol":1,"view":"settled_card","key":"\#(key)","client":"mac","version":"vs","cursor":"c1.abc","data_epoch":"epoch","status":\#(status),"sections":[],"empty":{"text":"\#(why)","tone":"neutral"}}"#
    return try JSONDecoder().decode(ViewDocument.self, from: Data(json.utf8))
}

private func cardRows(_ ids: [String]) -> String {
    ids.map {
        #"{"type":"row","id":"\#($0)","icon":"question","title":"Card \#($0)","depth":0,"struck":false,"link":{"kind":"view","key":"card:\#($0)"},"children":[]}"#
    }.joined(separator: ",")
}

private func sidebar(version: String = "v1", waiting: [String]) throws -> ViewDocument {
    let sections = #"[{"id":"waiting","collapsed":false,"items":[\#(cardRows(waiting))]},{"id":"summary","collapsed":false,"items":[{"type":"notice","text":"Waiting","tone":"neutral"}]}]"#
    return try makeDocument(key: "sidebar", version: version, sections: sections)
}

private func decodeAction(_ json: String) throws -> AlondraContract.Action {
    try JSONDecoder().decode(AlondraContract.Action.self, from: Data(json.utf8))
}

/// The reply action of a card the daemon rendered, from its golden.
private func goldenReply(_ name: String) throws -> AlondraContract.Action {
    let root = ProcessInfo.processInfo.environment["TEST_SRCDIR"].map(URL.init(fileURLWithPath:))
    let workspace = ProcessInfo.processInfo.environment["TEST_WORKSPACE"] ?? "_main"
    let url = try #require(root).appendingPathComponent("\(workspace)/fixtures/views/cards/\(name)")
    let view = try #require(try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    let sections = try #require(view["sections"] as? [[String: Any]])
    let card = try #require((sections.first?["items"] as? [[String: Any]])?.first)
    return try JSONDecoder().decode(AlondraContract.Action.self, from: JSONSerialization.data(withJSONObject: try #require(card["reply"])))
}

/// The first button of the first card the daemon rendered, from its golden.
private func goldenButton(_ name: String) throws -> AlondraContract.Action {
    let root = ProcessInfo.processInfo.environment["TEST_SRCDIR"].map(URL.init(fileURLWithPath:))
    let workspace = ProcessInfo.processInfo.environment["TEST_WORKSPACE"] ?? "_main"
    let url = try #require(root).appendingPathComponent("\(workspace)/fixtures/views/cards/\(name)")
    let view = try #require(try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    let sections = try #require(view["sections"] as? [[String: Any]])
    let card = try #require((sections.first?["items"] as? [[String: Any]])?.first)
    let button = try #require((card["actions"] as? [[String: Any]])?.first)
    return try JSONDecoder().decode(AlondraContract.Action.self, from: JSONSerialization.data(withJSONObject: button))
}

private func answer() throws -> AlondraContract.Action {
    try decodeAction(#"{"id":"answer:x@1","label":"Answer","style":"primary","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":false,"enabled":true}"#)
}

private func optionButton() throws -> AlondraContract.Action {
    try decodeAction(#"{"id":"ans:x@1","label":"Ship it","style":"primary","role":"advance","option_id":"clear","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#)
}

private func request(_ command: String) -> ActionRequest {
    ActionRequest(commandId: command, dataEpoch: "epoch", client: .mac, lang: "en", actionId: "answer:x@1")
}

private func response(_ outcome: String) throws -> ActionResponse {
    try JSONDecoder().decode(ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"\#(outcome)"}"#.utf8))
}

private let unchangedSidebar: @Sendable (ViewKey) throws -> ViewFetch = { key in
    key.rawValue == "sidebar"
        ? .document(try sidebar(waiting: ["a", "b"]))
        : .document(try makeDocument(key: key.rawValue, version: "d1"))
}

@MainActor
private func makeStore(
    waiting: [String], selected: String? = nil, detail: String = "[]", clock: TestClock<Duration> = TestClock(),
    load: @escaping @Sendable (ViewKey) throws -> ViewFetch = { .document(try makeDocument(key: $0.rawValue, version: "d1")) }
) throws -> TestStoreOf<MacPresentationFeature> {
    var state = MacPresentationFeature.State()
    state.sidebarDocument = try sidebar(waiting: waiting)
    state.actionFeature.dataEpoch = try #require(state.sidebarDocument).dataEpoch
    if let selected {
        state.selection = .viewKey(selected)
        state.selectedViewKey = selected
        state.detailDocument = try makeDocument(key: selected, version: "d0", sections: detail)
    }
    let store = TestStore(initialState: state) { MacPresentationFeature() } withDependencies: {
        $0.viewsClient.openScreen = { _, _ in }
        $0.viewsClient.closeScreen = { _ in }
        $0.viewsClient.load = { key, _ in try load(key) }
        $0.continuousClock = clock
    }
    store.exhaustivity = .off
    return store
}

private let changedReply = #"{"protocol":1,"outcome":"changed","result":{"kind":"changed","title":"PR #12 has new commits","body":"You reviewed an older version.","next":{"id":"answer:x@2","label":"Send anyway","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}}"#

private func refused(_ title: String) throws -> ActionResponse {
    try JSONDecoder().decode(ActionResponse.self, from: Data(
        #"{"protocol":1,"outcome":"rejected","message":"raw","result":{"kind":"refused","title":"\#(title)"}}"#.utf8))
}

/// What the Mac sees of the daemon while an action travels: every request, and the answer it gets.
private final class Daemon: @unchecked Sendable {
    private let lock = NSLock()
    private var sent: [ActionRequest] = []
    private let answer: @Sendable (Int) async throws -> ActionResponse

    init(_ answer: @escaping @Sendable (Int) async throws -> ActionResponse) { self.answer = answer }

    var requests: [ActionRequest] { lock.withLock { sent } }

    func perform(_ request: ActionRequest) async throws -> ActionResponse {
        let call = lock.withLock { sent.append(request); return sent.count }
        return try await answer(call)
    }
}

/// Holds the first answer back until it is opened.
private actor Hold {
    private var open = false
    private var waiting: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if open { return }
        await withCheckedContinuation { waiting.append($0) }
    }

    func release() {
        open = true
        waiting.forEach { $0.resume() }
        waiting.removeAll()
    }
}

@MainActor
private func answering(_ daemon: Daemon, clock: TestClock<Duration> = TestClock()) throws -> TestStoreOf<MacPresentationFeature> {
    let store = try makeStore(waiting: ["a", "b"], selected: "card:a", clock: clock, load: unchangedSidebar)
    store.dependencies.viewsClient.perform = { try await daemon.perform($0) }
    return store
}

@MainActor
struct MacServiceNavigationTests {
    @Test
    func selectingAViewOpensItAndShowsItsDocument() async throws {
        let store = try makeStore(waiting: ["a", "b"])
        await store.send(.select(.viewKey("card:a")))
        await store.skipReceivedActions()
        #expect(store.state.selection == .viewKey("card:a"))
        #expect(store.state.selectedViewKey == "card:a")
        #expect(store.state.detailDocument?.key == "card:a")
        #expect(store.state.actionFeature.dataEpoch == "epoch")
    }

    @Test
    func selectingNothingClearsTheDetail() async throws {
        let store = try makeStore(waiting: ["a"], selected: "card:a")
        await store.send(.select(nil))
        #expect(store.state.selection == nil)
        #expect(store.state.detailDocument == nil)
    }

    @Test
    func theFirstSidebarSelectsTheFirstLink() async throws {
        let store = try makeStore(waiting: [])
        await store.send(.viewLoaded("sidebar", .document(try sidebar(waiting: ["a", "b"]))))
        await store.skipReceivedActions()
        #expect(store.state.selection == .viewKey("card:a"))
        #expect(store.state.detailDocument?.key == "card:a")
    }

    @Test
    func aFailedLoadKeepsWhatWasShownAndReportsTheError() async throws {
        let store = try makeStore(waiting: ["a"], selected: "card:a")
        await store.send(.viewFailed("card:a", "offline"))
        #expect(store.state.viewError == "offline")
        #expect(store.state.detailDocument?.key == "card:a")
        await store.send(.viewLoaded("card:a", .unchanged(cursor: "c2")))
        #expect(store.state.viewError == nil)
    }

    @Test
    func staleViewsReloadOnlyTheVisibleOnes() async throws {
        let loaded = LockIsolated<[String]>([])
        let store = try makeStore(waiting: ["a"], selected: "card:a") { key in
            loaded.withValue { $0.append(key.rawValue) }
            return .unchanged(cursor: "c2")
        }
        await store.send(.viewsStale(["sidebar", "card:z", "card:a"]))
        await store.skipReceivedActions()
        #expect(Set(loaded.value) == ["sidebar", "card:a"])
    }

    @Test
    func aCardSettledElsewhereLeavesForTheNextOneAndSaysWhy() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a") { key in
            key.rawValue == "card:a" ? .document(try settledCard(key.rawValue, why: "Ya respondida")) : .document(try makeDocument(key: key.rawValue, version: "d1"))
        }
        await store.send(.viewsStale(["card:a"]))
        await store.receive(\.settled, "Ya respondida")
        await store.skipReceivedActions()
        #expect(store.state.selectedViewKey == "card:b")
        #expect(store.state.detailDocument?.key == "card:b")
    }

    @Test
    func anOldNotificationForASettledCardShowsNothingButWhy() async throws {
        let store = try makeStore(waiting: []) { .document(try settledCard($0.rawValue, why: "Ya no hace falta")) }
        await store.send(.select(.viewKey("card:gone")))
        await store.receive(\.settled, "Ya no hace falta")
        #expect(store.state.selection == nil)
        #expect(store.state.selectedViewKey == nil)
        #expect(store.state.detailDocument == nil)
    }

    @Test
    func theWindowSaysWhyASettledCardLeft() {
        let store = Store(initialState: MacAppFeature.State()) { MacAppFeature() } withDependencies: {
            $0.continuousClock = TestClock()
        }
        store.send(.macPresentation(.settled("Ya aceptada")))
        #expect(store.notice?.event == .actionCompleted("Ya aceptada"))
    }

    /// The window used to keep «saved data · connection lost» until something was clicked. When the
    /// Mac answers again, what failed is asked for at once.
    @Test
    func theMacAnsweringAgainReloadsWhatFailed() async throws {
        let loaded = LockIsolated<[String]>([])
        let store = try makeStore(waiting: ["a"], selected: "card:a") { key in
            loaded.withValue { $0.append(key.rawValue) }
            return .unchanged(cursor: "c2")
        }
        await store.send(.viewFailed("card:a", "offline"))
        await store.send(.reconnected)
        await store.skipReceivedActions()
        #expect(Set(loaded.value) == ["sidebar", "card:a"])
        #expect(store.state.viewError == nil)
    }

    @Test
    func theMacAnsweringWithNothingFailedLoadsNothing() async throws {
        let loaded = LockIsolated<[String]>([])
        let store = try makeStore(waiting: ["a"], selected: "card:a") { key in
            loaded.withValue { $0.append(key.rawValue) }
            return .unchanged(cursor: "c2")
        }
        await store.send(.reconnected)
        #expect(loaded.value.isEmpty)
    }

    @Test
    func theWindowReloadsOnlyWhenTheMacStartsAnsweringAgain() {
        let store = Store(initialState: MacAppFeature.State()) { MacAppFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.load = { key, _ in .document(try makeDocument(key: key.rawValue, version: "d1")) }
            $0.viewsClient.openScreen = { _, _ in }
        }
        store.send(.macPresentation(.viewFailed("sidebar", "offline")))
        store.send(.connectionChanged(.unreachable(.macNotAnswering)))
        #expect(store.macPresentation.viewError == "offline")
        store.send(.connectionChanged(.connected(.irohRelay)))
        #expect(store.connection == .connected(.irohRelay))
    }

    /// The feed and the connection stay open for as long as the window does; the window used to wait
    /// for them to end before asking for its sidebar, so it never asked.
    @Test(.timeLimit(.minutes(1)))
    func theWindowAsksForItsSidebarWhileTheFeedStaysOpen() async throws {
        let (requested, request) = AsyncStream<String>.makeStream()
        let store = Store(initialState: MacAppFeature.State()) { MacAppFeature() } withDependencies: {
            $0.continuousClock = TestClock()
            $0.viewsClient.feed = { AsyncStream { _ in } }
            $0.viewsClient.connection = { AsyncStream { $0.yield(.connecting) } }
            $0.viewsClient.openScreen = { _, _ in }
            $0.viewsClient.closeScreen = { _ in }
            $0.viewsClient.load = { key, _ in
                request.yield(key.rawValue)
                return .document(try makeDocument(key: key.rawValue, version: "d1"))
            }
        }
        let window = store.send(.appeared)
        var keys = requested.makeAsyncIterator()
        #expect(await keys.next() == "sidebar")
        store.send(.disappeared)
        window.cancel()
    }

    @Test
    func eachWindowKeepsItsOwnViewsSelection() {
        let first = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        let second = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        first.send(.select(.viewKey("pull_request:p2")))
        second.send(.select(.viewKey("task:t1")))
        #expect(first.selection == .viewKey("pull_request:p2"))
        #expect(second.selection == .viewKey("task:t1"))
    }
}

@MainActor
struct MacAnsweringTests {
    @Test
    func anAnswerTheDaemonNeverConfirmsLeavesAtOnceAndComesBackWithANoticeAfterTheTimeout() async throws {
        let clock = TestClock<Duration>()
        let hold = Hold()
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", clock: clock, load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { _ in
            await hold.wait()
            return try response("applied")
        }
        await store.send(.beginViewAction(try answer()))
        await store.send(.action(.textChanged("Ship it")))
        await store.send(.action(.submit))
        await store.receive(\.action.sending)
        #expect(store.state.hides("a"), "it leaves at once")
        #expect(!store.state.actionPresented)
        #expect(store.state.selection == .viewKey("card:b"))

        await clock.advance(by: Optimistic.confirmationTimeout - .milliseconds(1))
        #expect(store.state.hides("a"), "nothing shows while it retries")

        await clock.advance(by: .milliseconds(1))
        await store.receive(\.answerOverdue, "a")
        await store.receive(\.answerNotConfirmed, "Your Mac hasn’t confirmed your answer to “Card a”.")
        #expect(!store.state.hides("a"), "the unconfirmed card waits again")

        await hold.release()
        await store.receive(\.action.settled)
        await store.skipReceivedActions(strict: false)
        #expect(store.state.hides("a"), "confirmed late, it leaves")
    }

    @Test
    func anAnswerTheDaemonConfirmsInTimeNeverComesBack() async throws {
        let clock = TestClock<Duration>()
        let hold = Hold()
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", clock: clock, load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { _ in
            await hold.wait()
            return try response("applied")
        }
        await store.send(.beginViewAction(try answer()))
        await store.send(.action(.textChanged("Ship it")))
        await store.send(.action(.submit))
        await clock.advance(by: .seconds(5))
        await hold.release()
        await store.receive(\.action.settled)
        await clock.advance(by: .seconds(120))
        await store.skipReceivedActions(strict: false)
        #expect(store.state.hides("a"))
        #expect(!store.state.optimistic.isOverdue("a"))
    }

    @Test
    func anUnconfirmedAnswerSaysSoInTheWindowsNotice() {
        let store = Store(initialState: MacAppFeature.State()) { MacAppFeature() } withDependencies: {
            $0.continuousClock = TestClock()
        }
        store.send(.macPresentation(.answerNotConfirmed("Your Mac hasn’t confirmed your answer to “Card a”.")))
        #expect(store.notice?.event == WorkspaceNotice.Event.failure("Your Mac hasn’t confirmed your answer to “Card a”."))
    }

    @Test
    func answeringTheOpenCardMovesToTheNextOneAtOnce() async throws {
        let store = try makeStore(waiting: ["a", "b", "c"], selected: "card:a")
        await store.send(.beginViewAction(try answer()))
        #expect(store.state.actionTarget == "a")
        #expect(store.state.actionPresented)

        await store.send(.action(.sending(request("1"))))
        await store.skipReceivedActions()
        #expect(store.state.optimistic.hides("a"))
        #expect(!store.state.actionPresented)
        #expect(store.state.selection == .viewKey("card:b"))
        #expect(store.state.detailDocument?.key == "card:b")
    }

    @Test
    func theLastCardOfTheListFallsBackToThePreviousOne() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:b")
        await store.send(.beginViewAction(try answer()))
        await store.send(.action(.sending(request("1"))))
        await store.skipReceivedActions()
        #expect(store.state.selection == .viewKey("card:a"))
    }

    @Test
    func aButtonThatNeedsNothingMoreSendsAtOnceWithItsOption() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { request in
            sent.withValue { $0.append(request) }
            return try response("applied")
        }
        await store.send(.beginViewAction(try optionButton()))
        await store.skipReceivedActions()
        #expect(sent.value.map(\.actionId) == ["ans:x@1"])
        #expect(sent.value.map(\.optionId) == ["clear"])
        #expect(!store.state.actionPresented)
        #expect(store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:b"))
    }

    @Test
    func thePullRequestCardsMergeButtonMergesAtOnceWithTheMethodItSays() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { request in
            sent.withValue { $0.append(request) }
            return try response("applied")
        }
        let merge = try goldenButton("card-pr-mac.json")
        #expect(merge.label == "Squash and merge")
        await store.send(.beginViewAction(merge))
        #expect(!store.state.actionPresented, "no sheet to choose the method")
        await store.skipReceivedActions()
        #expect(sent.value.map(\.actionId) == [merge.id])
        #expect(sent.value.map(\.optionId) == ["squash"])
    }

    @Test(arguments: [
        #"{"kind":"none"}"#,
        #"{"kind":"text","placeholder":"Add a note","required":false,"multiline":true}"#,
        #"{"kind":"text","placeholder":"Add a note","required":true,"multiline":true}"#,
    ])
    func pressingDoneAnswersAtOnceEvenWhenAnOlderDaemonMarkedItAsTakingANote(input: String) async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { request in
            sent.withValue { $0.append(request) }
            return try response("applied")
        }
        let done = try decodeAction(#"{"id":"ans:x@1","label":"Done","style":"primary","role":"advance","option_id":"done","presentation":"button","input":"# + input + #","attachments":true,"enabled":true}"#)
        await store.send(.beginViewAction(done))
        #expect(!store.state.actionPresented, "Done never opens the sheet")
        await store.skipReceivedActions()
        #expect(sent.value.map(\.optionId) == ["done"])
        #expect(sent.value.first?.input.text == nil)
    }

    @Test
    func whatIsWrittenInTheCardsFieldGoesWithTheOptionPressed() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { request in
            sent.withValue { $0.append(request) }
            return try response("applied")
        }
        let option = try optionButton()
        let words = try #require(option.carrying("  Only on the onboarding screen \n"))
        #expect(option.carrying("   ") == nil, "nothing written: the option goes alone")
        #expect(try answer().carrying("Hi") == nil, "only an option carries the field's words")
        await store.send(.sendReply(option, text: words))
        await store.skipReceivedActions()
        #expect(sent.value.map(\.actionId) == ["ans:x@1"])
        #expect(sent.value.map(\.optionId) == ["clear"])
        #expect(sent.value.first?.input.text == "Only on the onboarding screen")
        #expect(store.state.optimistic.hides("a"))
    }

    @Test
    func aReplyGoesWithoutHidingTheCard() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { request in
            sent.withValue { $0.append(request) }
            return try response("applied")
        }
        let reply = try decodeAction(#"{"id":"reply:a@1","label":"Reply","style":"secondary","role":"neutral","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":true,"enabled":true}"#)
        await store.send(.sendReply(reply, text: "Use the compact one"))
        await store.skipReceivedActions()
        #expect(sent.value.map(\.actionId) == ["reply:a@1"])
        #expect(sent.value.first?.input.text == "Use the compact one")
        #expect(!store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:a"))
    }

    @Test
    func requestChangesInTheCardsFieldOpensTheSheetWithItsWordsToAddBlockComments() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { request in
            sent.withValue { $0.append(request) }
            return try response("applied")
        }
        let changes = try decodeAction(#"{"id":"changes:a@1","label":"Request changes","style":"secondary","role":"neutral","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":true,"comments":{"kind":"blocks"},"enabled":true,"resolves_card":true}"#)
        await store.send(.sendReply(changes, text: "Split the second step"))
        await store.skipReceivedActions()
        #expect(store.state.actionPresented, "the sheet holds the comments on the blocks")
        #expect(store.state.actionFeature.text == "Split the second step")
        #expect(sent.value.isEmpty, "nothing goes until the sheet sends it")
    }

    @Test
    func requestChangesOnAPRCarriesTheDiffComments() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { _ in try response("applied") }
        await store.send(.diffCommentsChanged(["f.swift:3": ActionComment(anchor: "f.swift:3", text: "Rename it")]))
        let changes = try decodeAction(#"{"id":"changes:a@1","label":"Request changes","style":"secondary","role":"neutral","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":true,"comments":{"kind":"diff_lines"},"enabled":true,"resolves_card":true}"#)
        await store.send(.sendReply(changes, text: ""))
        await store.skipReceivedActions()
        #expect(store.state.actionPresented)
        #expect(store.state.actionFeature.comments.map(\.text) == ["Rename it"])
    }

    @Test
    func aReplyTheServerSaysAnswersTheQuestionTakesTheCardOutAtOnce() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { _ in try response("applied") }
        let reply = try decodeAction(#"{"id":"reply:a@1","label":"Reply","style":"secondary","role":"neutral","presentation":"button","input":{"kind":"text","required":true,"multiline":true},"attachments":true,"enabled":true,"resolves_card":true}"#)
        await store.send(.sendReply(reply, text: "Use the compact one"))
        await store.skipReceivedActions()
        #expect(store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:b"))
    }

    /// David wrote «Hazlo tú» under the orchestrator's Decide card and it stayed: the reply went
    /// out as a new message. The reply the daemon renders for it now answers that card.
    @Test
    func wordsUnderAnOrchestratorDecideAnswerItAndTakeItOut() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { request in
            sent.withValue { $0.append(request) }
            return try response("applied")
        }
        let reply = try goldenReply("card-orchestrator-decide-mac.json")
        await store.send(.sendReply(reply, text: "Hazlo tú"))
        await store.skipReceivedActions()
        #expect(sent.value.first?.actionId.hasPrefix("reply:memory-pause-livelock-ask@") == true)
        #expect(sent.value.first?.input.text == "Hazlo tú")
        #expect(store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:b"))
    }

    @Test
    func aButtonTheServerSaysKeepsItsCardLeavesItInPlace() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { _ in try response("applied") }
        let clarify = try decodeAction(#"{"id":"clarify:a@1","label":"Request clarification","style":"secondary","role":"neutral","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true,"resolves_card":false}"#)
        await store.send(.beginViewAction(clarify))
        await store.skipReceivedActions()
        #expect(!store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:a"))
    }

    @Test
    func clarifyingSendsTheReasonAndTheOptionalText() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = try makeStore(waiting: ["a"], selected: "card:a")
        store.dependencies.viewsClient.perform = { request in
            sent.withValue { $0.append(request) }
            return try response("applied")
        }
        let clarify = try decodeAction(#"{"id":"clarify:a@1","label":"Request clarification","style":"secondary","role":"neutral","presentation":"options","options":[{"id":"unclear","label":"It is unclear","style":"secondary","input":{"kind":"none"}},{"id":"missing_context","label":"It lacks context","style":"secondary","input":{"kind":"none"}}],"input":{"kind":"text","required":false,"multiline":true},"attachments":false,"enabled":true}"#)
        await store.send(.beginViewAction(clarify))
        #expect(store.state.actionPresented, "a reason is still to be chosen")
        await store.skipReceivedActions()
        await store.send(.action(.chooseOption("missing_context")))
        await store.send(.action(.textChanged("Which build?")))
        await store.send(.action(.submit))
        await store.skipReceivedActions()
        #expect(sent.value.map(\.optionId) == ["missing_context"])
        #expect(sent.value.first?.input.text == "Which build?")
    }

    @Test
    func aCardAnsweredFromTheSidebarKeepsTheOpenCardWhereItIs() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:b")
        await store.send(.beginViewAction(try answer(), target: "a"))
        await store.send(.action(.sending(request("1"))))
        #expect(store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:b"))
    }

    @Test
    func aRefusedActionKeepsTheCardOutUntilTheDaemonListsItAgain() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a", load: unchangedSidebar)
        await store.send(.beginViewAction(try answer()))
        await store.send(.action(.sending(request("1"))))
        await store.send(.action(.settled(request("1"), applied: false)))
        await store.skipReceivedActions(strict: false)
        #expect(store.state.optimistic.hides("a"), "never back as unanswered")

        await store.send(.viewLoaded("sidebar", .document(try sidebar(version: "v2", waiting: ["a", "b"]))))
        #expect(!store.state.optimistic.hides("a"), "the daemon's newer list decides")
    }

    @Test
    func aRefusalSaysTheServersWordsWithoutReopeningTheCard() async throws {
        let store = try answering(Daemon { _ in try refused("PR #12 must merge first: this one builds on it.") })
        await store.send(.beginViewAction(try optionButton()))
        await store.receive(\.action.delegate.failed, "PR #12 must merge first: this one builds on it.")
        await store.skipReceivedActions()
        #expect(store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:b"))
        #expect(store.state.returnReasons["a"] == "PR #12 must merge first: this one builds on it.")
    }

    @Test
    func anAnswerThatCanNeverBeDeliveredSaysWhyAndKeepsTheCardOut() async throws {
        let daemon = Daemon { _ in throw ActionDeliveryError.dataReset }
        let store = try answering(daemon)
        await store.send(.beginViewAction(try optionButton()))
        await store.receive(\.action.delegate.failed, "Your Mac’s data was reset, so this answer no longer applies.")
        await store.skipReceivedActions()
        #expect(store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:b"))
    }

    @Test
    func anAnswerStillOnItsWayKeepsTheCardOutUntilTheTimeoutAndLeavesOnceConfirmed() async throws {
        let clock = TestClock<Duration>()
        let hold = Hold()
        let daemon = Daemon { _ in
            await hold.wait()
            return try response("applied")
        }
        let store = try answering(daemon, clock: clock)
        await store.send(.beginViewAction(try optionButton()))
        await store.skipReceivedActions()
        #expect(store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:b"))

        await clock.advance(by: Optimistic.confirmationTimeout - .milliseconds(1))
        await store.skipReceivedActions(strict: false)
        #expect(store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:b"))
        #expect(store.state.actionFeature.error == nil, "no error while it is still going")

        await clock.advance(by: .seconds(600))
        await store.skipReceivedActions(strict: false)
        #expect(!store.state.hides("a"), "unconfirmed after the timeout, it waits again")
        #expect(daemon.requests.count == 1)
        await hold.release()
        await store.skipReceivedActions(strict: false)
        #expect(store.state.optimistic.hides("a"))
    }

    @Test
    func cardsWhoseAnswerIsStillOnItsWayStayOutOfTheSidebarAfterARelaunch() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:b")
        await store.send(.undeliveredChanged(["a"]))
        #expect(store.state.hides("a"))
        #expect(!store.state.hides("b"))
        await store.send(.undeliveredChanged([]))
        #expect(!store.state.hides("a"))
    }

    @Test
    func aLateAnswerToAnEarlierCardLeavesTheOneBeingAnsweredAlone() async throws {
        let hold = Hold()
        let daemon = Daemon { call in
            if call == 1 {
                await hold.wait()
                return try refused("It already moved on")
            }
            return try response("applied")
        }
        let store = try answering(daemon)
        await store.send(.beginViewAction(try optionButton()))
        await store.skipReceivedActions()
        #expect(store.state.selection == .viewKey("card:b"))

        await store.send(.beginViewAction(try answer()))
        await store.send(.action(.textChanged("Use the compact one")))
        await hold.release()
        await store.receive(\.action.delegate.failed, "It already moved on")
        await store.skipReceivedActions(strict: false)
        #expect(store.state.actionFeature.action?.id == "answer:x@1")
        #expect(store.state.actionFeature.text == "Use the compact one")
        #expect(store.state.actionFeature.error == nil)
        #expect(store.state.actionPresented)
        #expect(store.state.selection == .viewKey("card:b"), "the open sheet is not pulled away")
        #expect(store.state.optimistic.hides("a"), "the refused card is not put back as unanswered")
        #expect(store.state.returnReasons["a"] == "It already moved on")
    }

    @Test
    func cancellingTheChangedQuestionKeepsWhatWasBeingAnsweredOpen() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a")
        await store.send(.beginViewAction(try answer()))
        await store.send(.action(.textChanged("Use the compact one")))
        await store.send(.action(.response(try JSONDecoder().decode(ActionResponse.self, from: Data(changedReply.utf8)))))
        #expect(store.state.actionFeature.changed?.title == "PR #12 has new commits")
        #expect(store.state.actionPresented)

        await store.send(.action(.dismissChanged))
        #expect(store.state.actionFeature.changed == nil)
        #expect(store.state.actionPresented, "the sheet underneath stays")
        #expect(store.state.actionFeature.action?.id == "answer:x@1")
        #expect(store.state.actionFeature.text == "Use the compact one")
    }

    @Test
    func whyACardCameBackIsForgottenOnceTheServerSendsANewerList() async throws {
        let store = try answering(Daemon { _ in try refused("It already moved on") })
        await store.send(.beginViewAction(try optionButton()))
        await store.skipReceivedActions()
        #expect(store.state.returnReasons["a"] == "It already moved on")

        await store.send(.viewLoaded("sidebar", .document(try sidebar(version: "v2", waiting: ["a", "b"]))))
        #expect(store.state.returnReasons.isEmpty)
    }

    @Test
    func whyACardCameBackIsForgottenWhenTheConnectionComesBack() async throws {
        let store = try answering(Daemon { _ in try refused("It already moved on") })
        await store.send(.beginViewAction(try optionButton()))
        await store.skipReceivedActions()
        #expect(!store.state.returnReasons.isEmpty)

        await store.send(.viewsReset)
        #expect(store.state.returnReasons.isEmpty)
    }

    @Test
    func anAppliedActionStaysHiddenUntilANewerSidebarArrives() async throws {
        let store = try makeStore(waiting: ["a", "b"], selected: "card:a")
        await store.send(.beginViewAction(try answer()))
        await store.send(.action(.sending(request("1"))))
        await store.send(.action(.settled(request("1"), applied: true)))
        #expect(store.state.optimistic.hides("a"))

        await store.send(.viewLoaded("sidebar", .document(try sidebar(version: "v2", waiting: ["b"]))))
        #expect(!store.state.optimistic.hides("a"))
        #expect(store.state.selection == .viewKey("card:b"))
    }

    @Test
    func aChangedItemAsksAgainInTheSheet() async throws {
        let store = try makeStore(waiting: ["a"], selected: "card:a")
        await store.send(.beginViewAction(try answer()))
        await store.send(.action(.sending(request("1"))))
        #expect(!store.state.actionPresented)
        await store.send(.action(.response(try response("changed"))))
        #expect(store.state.actionPresented)
    }

    @Test
    func aCardRemovedElsewhereMovesToTheNextCardNotTheFirstLink() async throws {
        let store = try makeStore(waiting: ["a", "b", "c"], selected: "card:b")
        await store.send(.viewLoaded("sidebar", .document(try sidebar(version: "v2", waiting: ["a", "c"]))))
        await store.skipReceivedActions()
        #expect(store.state.selection == .viewKey("card:c"))
        #expect(store.state.detailDocument?.key == "card:c")
    }

    @Test
    func tasksATaskAndTheNewRequestFormStayOpenAcrossSidebarUpdates() async throws {
        for key in ["tasks", "task:t1", "new_request"] {
            let store = try makeStore(waiting: ["a", "b"], selected: key)
            await store.send(.viewLoaded("sidebar", .document(try sidebar(version: "v2", waiting: ["b"]))))
            #expect(store.state.selection == .viewKey(key))
        }
    }

    @Test
    func theFleetAndANodeOpenNothingAndTheTaskStaysOpen() async throws {
        let loaded = LockIsolated<[String]>([])
        let store = try makeStore(waiting: ["a"], selected: "task:t1") { key in
            loaded.withValue { $0.append(key.rawValue) }
            return .document(try makeDocument(key: key.rawValue, version: "d1"))
        }
        for key in ["fleet", "node:worker"] {
            await store.send(.select(.viewKey(key)))
            #expect(store.state.selection == .viewKey("task:t1"))
            #expect(store.state.detailDocument?.key == "task:t1")
        }
        #expect(loaded.value.isEmpty)
    }
}

@MainActor
struct MacFormTests {
    @Test
    func beginningAFormOpensItsSheetWithoutAnAnswerTarget() async throws {
        let sections = #"[{"id":"form","collapsed":false,"items":[{"type":"form","id":"request","fields":[{"id":"project","label":"Project","kind":{"kind":"text","placeholder":null},"value":"alondra","required":true,"reload_on_change":true}],"submit":{"id":"request_send@1","label":"Send","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}]}]"#
        var state = MacPresentationFeature.State()
        state.detailDocument = try makeDocument(key: "new_request", version: "d1", sections: sections)
        state.actionTarget = "stale"
        let form = try #require(state.detailDocument?.sections.first?.items.first.flatMap { item -> AlondraContract.Form? in
            if case let .form(form) = item { form } else { nil }
        })
        let store = TestStore(initialState: state) { MacPresentationFeature() }
        store.exhaustivity = .off

        await store.send(.beginViewForm(form))
        #expect(store.state.activeForm == form)
        #expect(store.state.actionPresented)
        #expect(store.state.actionTarget == nil)
        await store.skipReceivedActions()
        #expect(store.state.actionFeature.fields["project"] == .string("alondra"))

        await store.send(.actionPresented(false))
        #expect(store.state.activeForm == nil)
        #expect(store.state.actionFeature.action == nil)
    }

    @Test
    func changingTheProjectReloadsTheFormForIt() async throws {
        let store = try makeStore(waiting: [], selected: "new_request")
        await store.send(.reloadForm("project", .string("alondra")))
        await store.skipReceivedActions()
        #expect(store.state.selectedViewKey == ViewKey.newRequest(project: "alondra").rawValue)
        await store.send(.reloadForm("other", .string("x")))
        #expect(store.state.selectedViewKey == ViewKey.newRequest(project: "alondra").rawValue)
    }

    @Test
    func theIssuePickerLoadsFiltersAndFillsTheField() async throws {
        let clock = TestClock()
        let picked = LockIsolated<[String]>([])
        let store = try makeStore(waiting: []) { key in
            picked.withValue { $0.append(key.rawValue) }
            return .document(try makeDocument(key: key.rawValue, version: "p1"))
        }
        store.dependencies.continuousClock = clock

        await store.send(.pickerOpened("issue", "issue_picker"))
        #expect(store.state.pickerFieldID == "issue")
        #expect(store.state.pickerLoading)
        await store.skipReceivedActions()
        #expect(!store.state.pickerLoading)
        #expect(store.state.pickerDocument != nil)

        await store.send(.pickerQueryChanged("crash"))
        await clock.advance(by: .milliseconds(250))
        await store.skipReceivedActions()
        #expect(picked.value.last == ViewKey.issuePicker(query: "crash").rawValue)

        await store.send(.pickerSelected("github.com/example/repo#7"))
        #expect(store.state.actionFeature.fields["issue"] == .string("github.com/example/repo#7"))
        #expect(store.state.pickerFieldID == nil)
    }

    @Test
    func aPickerThatIsNotTheIssuePickerIsIgnored() async throws {
        let store = try makeStore(waiting: [])
        await store.send(.pickerOpened("issue", "something_else"))
        #expect(store.state.pickerFieldID == nil)
    }
}

@MainActor
struct MacContentTests {
    private func content(_ digest: String) throws -> ContentRef {
        let json = #"{"kind":"diff","title":"Changes","size":10,"digest":"\#(digest)","fetch":"content/\#(digest)","commentable":true}"#
        return try JSONDecoder().decode(ContentRef.self, from: Data(json.utf8))
    }

    @Test
    func openingContentLoadsItsBodyIntoTheSheet() async throws {
        var state = MacPresentationFeature.State()
        state.diffComments = ["a": ActionComment(anchor: "a", text: "old")]
        let store = TestStore(initialState: state) { MacPresentationFeature() } withDependencies: {
            $0.viewsClient.fetchDiff = { _ in FetchedDiff(text: Self.diff, truncated: true) }
        }
        store.exhaustivity = .off
        let ref = try content("d1")

        await store.send(.contentRequested(ref))
        #expect(store.state.contentPresented)
        #expect(store.state.contentLoading)
        #expect(store.state.diffComments.isEmpty, "comments belong to the diff they were written on")
        await store.skipReceivedActions()
        #expect(store.state.contentShown == .diff(UnifiedDiff.parse(Self.diff), truncated: true))
        #expect(store.state.contentIsDiff)
        #expect(!store.state.contentLoading)
    }

    nonisolated static let diff = "diff --git a/a.swift b/a.swift\n--- a/a.swift\n+++ b/a.swift\n@@ -1 +1 @@\n-old\n+added\n"

    @Test
    func theSummaryFetchesTheDiffOnceAndTheViewerOpensItAtOnce() async throws {
        let fetches = LockIsolated(0)
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.viewsClient.fetchDiff = { _ in
                fetches.withValue { $0 += 1 }
                return FetchedDiff(text: Self.diff)
            }
        }
        store.exhaustivity = .off
        let ref = try content("d1")

        await store.send(.diffNeeded(ref))
        await store.skipReceivedActions()
        #expect(store.state.diff?.files == UnifiedDiff.parse(Self.diff))
        await store.send(.diffNeeded(ref))
        await store.send(.contentRequested(ref))
        #expect(!store.state.contentLoading, "the viewer opens on the diff the summary already read")
        #expect(fetches.value == 1)
    }

    @Test
    func aSummaryThatCouldNotLoadTriesAgain() async throws {
        struct Offline: Error {}
        let attempts = LockIsolated(0)
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.viewsClient.fetchDiff = { _ in
                if attempts.withValue({ $0 += 1; return $0 }) == 1 { throw Offline() }
                return FetchedDiff(text: Self.diff)
            }
        }
        store.exhaustivity = .off
        let ref = try content("d1")

        await store.send(.diffNeeded(ref))
        await store.skipReceivedActions()
        #expect(store.state.diffFailed == "d1")
        await store.send(.diffNeeded(ref))
        await store.skipReceivedActions()
        #expect(store.state.diffFailed == nil)
        #expect(store.state.diff?.digest == "d1")
    }

    @Test
    func aFetchedDocumentShowsItsLinksAndImages() async throws {
        let readme = """
        # Alondra

        <p align="center"><img src="https://example.com/logo.png" alt="Logo"></p>

        Read <a href="https://example.com/docs">the docs</a> or [the guide](https://example.com/guide).
        """
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.viewsClient.fetchContent = { _ in readme }
        }
        store.exhaustivity = .off
        let ref = try JSONDecoder().decode(ContentRef.self, from: Data(#"{"kind":"document","title":"README.md","size":120,"digest":"r1","fetch":"content/r1","commentable":false}"#.utf8))

        await store.send(.contentRequested(ref))
        await store.skipReceivedActions()
        guard case let .markdown(segments) = store.state.contentShown else {
            Issue.record("A fetched document must be drawn as Markdown, not as plain text")
            return
        }
        let blocks = segments.compactMap { segment -> MarkdownBlock? in
            if case let .block(block) = segment { block } else { nil }
        }
        #expect(blocks.contains { $0.kind == .image(url: URL(string: "https://example.com/logo.png")!, alt: "Logo") })
        let links = blocks.flatMap { block in block.content.runs[\.link].compactMap { link, _ in link } }
        #expect(links == [URL(string: "https://example.com/docs")!, URL(string: "https://example.com/guide")!])
    }

    @Test
    func reopeningTheSameDiffKeepsItsComments() async throws {
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.viewsClient.fetchDiff = { _ in FetchedDiff(text: Self.diff) }
        }
        store.exhaustivity = .off
        let ref = try content("d1")
        await store.send(.contentRequested(ref))
        await store.send(.diffCommentsChanged(["a": ActionComment(anchor: "a", text: "keep")]))
        await store.send(.contentRequested(ref))
        #expect(store.state.diffComments["a"]?.text == "keep")
    }

    @Test
    func aContentFailureIsShownInTheSheet() async throws {
        struct Offline: Error {}
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.viewsClient.fetchDiff = { _ in throw Offline() }
        }
        store.exhaustivity = .off
        await store.send(.contentRequested(try content("d1")))
        await store.skipReceivedActions()
        #expect(store.state.contentError == String(localized: "Couldn’t load this from your Mac. Try again."))
        #expect(!store.state.contentLoading)
    }
}

@MainActor
struct MacSearchTests {
    @Test
    func changingSearchCancelsThePreviousViewsRequest() async throws {
        let clock = TestClock()
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.load = { key, _ in
                #expect(key.rawValue == "search?q=new+query")
                let rows = #"[{"id":"results","collapsed":false,"items":[{"type":"row","id":"new-node","icon":"node","title":"New node","depth":0,"struck":false,"children":[]}]}]"#
                return .document(try makeDocument(key: key.rawValue, version: "v0123456789abcdef", sections: rows))
            }
        }
        await store.send(.queryChanged("old query")) { $0.query = "old query"; $0.searchRevision = 1 }
        await store.send(.searchRequested)
        await clock.advance(by: .milliseconds(600))
        await store.send(.queryChanged("new query")) { $0.query = "new query"; $0.searchRevision = 2 }
        await store.send(.searchRequested)
        await clock.advance(by: .milliseconds(1200))
        await store.receive(\.searchStarted) { $0.smartStatus = String(localized: "Searching the conversations…") }
        await store.receive(\.searchFinished) {
            $0.smartMatches = ["new-node"]
            $0.smartStatus = String(localized: "Search results")
        }
    }

    @Test
    func aQueryUnderThreeCharactersNeverReachesTheDaemon() async throws {
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        await store.send(.queryChanged("ab")) { $0.query = "ab"; $0.searchRevision = 1 }
        await store.send(.searchRequested)
    }

    @Test
    func anEmptySearchResultSaysSo() async throws {
        let clock = TestClock()
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.load = { key, _ in .document(try makeDocument(key: key.rawValue, version: "v1")) }
        }
        await store.send(.queryChanged("nothing")) { $0.query = "nothing"; $0.searchRevision = 1 }
        await store.send(.searchRequested)
        await clock.advance(by: .milliseconds(1200))
        await store.receive(\.searchStarted) { $0.smartStatus = String(localized: "Searching the conversations…") }
        await store.receive(\.searchFinished) {
            $0.smartMatches = []
            $0.smartStatus = String(localized: "No results")
        }
    }

    @Test
    func sidebarQueryMatchesRowsAndExpandsProjectsWithoutReadingLegacyModels() async {
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        await store.send(.queryChanged("release checklist")) {
            $0.query = "release checklist"
            $0.searchRevision = 1
        }
        #expect(store.state.matches("Release checklist for Mac"))
        #expect(!store.state.matches("Something else"))
        #expect(store.state.projectExpanded("Alondra"))
        await store.send(.queryChanged("   ")) { $0.query = "   "; $0.smartMatches = nil; $0.smartStatus = ""; $0.searchRevision = 2 }
        await store.send(.sidebarSectionExpanded("tasks", false)) { $0.tasksExpanded = false }
    }
}

struct MacWaitingQueueTests {
    @Test
    func theNextCardFollowsTheSidebarOrderAndSkipsWhatIsGone() throws {
        let document = try sidebar(waiting: ["a", "b", "c", "p"])
        let all: Set = ["card:a", "card:b", "card:c", "card:p"]
        #expect(MacPresentationFeature.nextKey(after: "card:a", in: document, available: all) == "card:b")
        #expect(MacPresentationFeature.nextKey(after: "card:a", in: document, available: all.subtracting(["card:b"])) == "card:c")
        #expect(MacPresentationFeature.nextKey(after: "card:c", in: document, available: all) == "card:p")
        #expect(MacPresentationFeature.nextKey(after: "card:p", in: document, available: all) == "card:c")
        #expect(MacPresentationFeature.nextKey(after: "card:a", in: document, available: ["card:a"]) == nil)
        #expect(MacPresentationFeature.nextKey(after: "node:x", in: document, available: all) == nil)
    }

    @Test
    func aCardIdIsReadFromCardAndPullRequestKeysOnly() {
        #expect(MacPresentationFeature.cardID(forViewKey: "card:abc") == "abc")
        #expect(MacPresentationFeature.cardID(forViewKey: "pull_request:github.com/x/y#1") == "github.com/x/y#1")
        #expect(MacPresentationFeature.cardID(forViewKey: "node:abc") == nil)
        #expect(MacPresentationFeature.cardID(forViewKey: "fleet") == nil)
    }
}

@MainActor
struct MacSectionActionTests {
    private let clear = #"{"id":"clear_completed:view@6","label":"Clear Completed","style":"secondary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true,"resolves_card":true}"#
    private var tasks: String {
        #"[{"id":"active","collapsed":false,"items":[]},{"id":"finished","title":"Completed","count":2,"collapsed":true,"items":[],"actions":[\#(clear)]}]"#
    }

    @Test
    func clearingEmptiesCompletedAtOnceAndARefusalBringsItBackSayingWhy() async throws {
        let hold = Hold()
        let daemon = Daemon { _ in
            await hold.wait()
            return try refused("Nothing to clear")
        }
        let store = try makeStore(waiting: [], selected: "tasks", detail: tasks, load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { try await daemon.perform($0) }
        let button = try #require(store.state.detailDocument?.sections.last?.actions.first)

        await store.send(.beginViewAction(button, target: "finished"))
        await store.receive(\.action.sending)
        #expect(store.state.hides("finished"), "Completed empties before the Mac answers")
        #expect(!store.state.actionPresented, "it asks nothing more")

        await hold.release()
        await store.receive(\.action.delegate.failed, "Nothing to clear")
        await store.skipReceivedActions(strict: false)
        #expect(daemon.requests.map(\.actionId) == ["clear_completed:view@6"])
        #expect(!store.state.hides("finished"), "refused, Completed is back")
        #expect(store.state.returnReasons["finished"] == "Nothing to clear")
        #expect(store.state.selection == .viewKey("tasks"), "the list stays open")
    }

    @Test
    func clearedCompletedStaysOutUntilTheMacSendsTheNewList() async throws {
        let store = try makeStore(waiting: [], selected: "tasks", detail: tasks, load: unchangedSidebar)
        store.dependencies.viewsClient.perform = { _ in try response("applied") }
        let button = try #require(store.state.detailDocument?.sections.last?.actions.first)

        await store.send(.beginViewAction(button, target: "finished"))
        await store.receive(\.action.sending)
        await store.skipReceivedActions(strict: false)
        #expect(store.state.hides("finished"))
        #expect(store.state.returnReasons["finished"] == nil)
    }
}
