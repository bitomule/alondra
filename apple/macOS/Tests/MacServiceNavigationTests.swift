import AlondraContract
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
    waiting: [String], selected: String? = nil, clock: TestClock<Duration> = TestClock(),
    load: @escaping @Sendable (ViewKey) throws -> ViewFetch = { .document(try makeDocument(key: $0.rawValue, version: "d1")) }
) throws -> TestStoreOf<MacPresentationFeature> {
    var state = MacPresentationFeature.State()
    state.sidebarDocument = try sidebar(waiting: waiting)
    state.actionFeature.dataEpoch = try #require(state.sidebarDocument).dataEpoch
    if let selected {
        state.selection = .viewKey(selected)
        state.selectedViewKey = selected
        state.detailDocument = try makeDocument(key: selected, version: "d0")
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

    @Test
    func eachWindowKeepsItsOwnViewsSelection() {
        let first = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        let second = Store(initialState: MacPresentationFeature.State()) { MacPresentationFeature() }
        first.send(.select(.viewKey("pull_request:p2")))
        second.send(.select(.viewKey("node:worker")))
        #expect(first.selection == .viewKey("pull_request:p2"))
        #expect(second.selection == .viewKey("node:worker"))
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
        await store.receive(\.answerNotConfirmed, "Your answer to “Card a” didn’t reach your Mac.")
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
        store.send(.macPresentation(.answerNotConfirmed("Your answer to “Card a” didn’t reach your Mac.")))
        #expect(store.notice?.event == WorkspaceNotice.Event.failure("Your answer to “Card a” didn’t reach your Mac."))
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
    func fleetTasksAndTheNewRequestFormStayOpenAcrossSidebarUpdates() async throws {
        for key in ["fleet", "tasks", "new_request"] {
            let store = try makeStore(waiting: ["a", "b"], selected: key)
            await store.send(.viewLoaded("sidebar", .document(try sidebar(version: "v2", waiting: ["b"]))))
            #expect(store.state.selection == .viewKey(key))
        }
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
            $0.viewsClient.fetchContent = { _ in "+added" }
        }
        store.exhaustivity = .off
        let ref = try content("d1")

        await store.send(.contentRequested(ref))
        #expect(store.state.contentPresented)
        #expect(store.state.contentLoading)
        #expect(store.state.diffComments.isEmpty, "comments belong to the diff they were written on")
        await store.skipReceivedActions()
        #expect(store.state.contentBody == "+added")
        #expect(store.state.contentIsDiff)
        #expect(!store.state.contentLoading)
    }

    @Test
    func reopeningTheSameDiffKeepsItsComments() async throws {
        let store = TestStore(initialState: MacPresentationFeature.State()) { MacPresentationFeature() } withDependencies: {
            $0.viewsClient.fetchContent = { _ in "+added" }
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
            $0.viewsClient.fetchContent = { _ in throw Offline() }
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
