import AlondraContract
import AlondraFeatures
import AlondraTransport
import ComposableArchitecture
import XCTest

/// Design §4/§5: the server words what happened; the clients only decide how it looks.
final class OutcomeStyleTests: XCTestCase {
    private func reply(_ json: String) throws -> ActionResponse {
        try ContractCodec.decode(ActionResponse.self, from: Data(json.utf8))
    }

    private func action(_ json: String) throws -> AlondraContract.Action {
        try ContractCodec.decode(AlondraContract.Action.self, from: Data(json.utf8))
    }

    private static let changes = #"{"id":"changes:pr-12@2","label":"Request changes","style":"secondary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#
    private static let merging = #"{"protocol":1,"outcome":"applied","stamp":"Merging…","message":"Merge authorized","revision":4,"result":{"kind":"started","title":"PR #734 is merging.","body":"You'll see it in its task when GitHub confirms."}}"#
    private static let refused = #"{"protocol":1,"outcome":"rejected","code":"upstream_pr_open","message":"raw domain text","result":{"kind":"refused","title":"PR #12 must merge first: this one builds on it."}}"#
    private static let failed = #"{"protocol":1,"outcome":"rejected","message":"x","result":{"kind":"failed","title":"Alondra couldn't reach GitHub. Nothing was changed; try again.","body":"Nothing was sent."}}"#
    private static let changed = #"{"protocol":1,"outcome":"changed","message":"old","confirm":{"action_id":"old:1","button":"old button"},"result":{"kind":"changed","title":"PR #12 has new commits","body":"You reviewed an older version. Send anyway, or open the PR to see what changed.","next":{"id":"changes:pr-12@3","label":"Send anyway","style":"primary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}}}"#

    func testAStartedOrDoneOutcomeIsASuccessWorthTheServersTitle() throws {
        XCTAssertEqual(OutcomeStyle(try reply(Self.merging)), .success("PR #734 is merging."))
        let done = #"{"protocol":1,"outcome":"applied","stamp":"Sent","result":{"kind":"done","title":"Answer sent."}}"#
        XCTAssertEqual(OutcomeStyle(try reply(done)), .success("Answer sent."))
    }

    func testARefusedOrFailedOutcomeIsAnErrorWithItsTitleAndBody() throws {
        XCTAssertEqual(OutcomeStyle(try reply(Self.refused)), .failure("PR #12 must merge first: this one builds on it."))
        XCTAssertEqual(OutcomeStyle(try reply(Self.failed)),
                       .failure("Alondra couldn't reach GitHub. Nothing was changed; try again. Nothing was sent."))
    }

    func testAChangedOutcomeAsksWithTheActionTheServerOffers() throws {
        guard case .ask(let changed) = OutcomeStyle(try reply(Self.changed)) else { return XCTFail("a changed outcome asks") }
        XCTAssertEqual(changed.title, "PR #12 has new commits")
        XCTAssertEqual(changed.body, "You reviewed an older version. Send anyway, or open the PR to see what changed.")
        XCTAssertEqual(changed.actionId, "changes:pr-12@3")
        XCTAssertEqual(changed.button, "Send anyway", "the button is the action's label, not a message")
    }

    func testAResultKindThisClientDoesNotKnowStillReadsAsTheServersWords() throws {
        let applied = try reply(#"{"protocol":1,"outcome":"applied","stamp":"Queued","result":{"kind":"queued","title":"PR #734 is queued to merge."}}"#)
        XCTAssertEqual(applied.result?.kind, .other)
        XCTAssertEqual(OutcomeStyle(applied), .success("PR #734 is queued to merge."), "applied, never 'not confirmed'")
        let refused = try reply(#"{"protocol":1,"outcome":"rejected","result":{"kind":"postponed","title":"Not now.","body":"Try after the release."}}"#)
        XCTAssertEqual(OutcomeStyle(refused), .failure("Not now. Try after the release."))
    }

    func testAnOlderDaemonWithoutAResultStillReadsThroughMessageAndStamp() throws {
        XCTAssertEqual(OutcomeStyle(try reply(#"{"protocol":1,"outcome":"applied","stamp":"Saved","message":"Saved"}"#)), .success("Saved"))
        XCTAssertEqual(OutcomeStyle(try reply(#"{"protocol":1,"outcome":"rejected","message":"It is working"}"#)), .failure("It is working"))
    }

    func testWhatNoAnswerMeansDependsOnWhetherTheMacCouldHaveGotIt() {
        let unreached = "Couldn’t reach your Mac. Nothing was sent; try again."
        let unsure = "Your Mac didn’t confirm it. It may have arrived: try again and it won’t be done twice."
        XCTAssertEqual(SendFailure(URLError(.notConnectedToInternet)).message, unreached)
        XCTAssertEqual(SendFailure(URLError(.cannotConnectToHost)).message, unreached)
        XCTAssertEqual(SendFailure(DirectRouteError.unreachable).message, unreached)
        XCTAssertEqual(SendFailure(URLError(.timedOut)).message, unsure)
        XCTAssertEqual(SendFailure(URLError(.networkConnectionLost)).message, unsure)
        XCTAssertEqual(SendFailure(OperationError.outcomeUnknown).message, unsure)
        XCTAssertEqual(SendFailure(APIError.unavailable).message, unsure, "a gateway may have passed it on")
        XCTAssertEqual(SendFailure(APIError.server(status: 500, kind: nil, detail: "panicked", reasons: [])).message,
                       "Your Mac couldn’t do it. Nothing changed; try again.")
        XCTAssertEqual(SendFailure(APIError.forbidden(kind: nil, detail: nil, reasons: [])).message,
                       "This device is no longer approved. Approve it again from the Mac.")
        XCTAssertEqual(SendFailure(APIError.conflict(kind: nil, detail: nil, reasons: [])).message,
                       "This changed while you were looking. It’s updated now; try again.")
    }

    func testAButtonIsPaintedInTheRoleTheServerSends() throws {
        let allow = try action(#"{"id":"ans:p@1","label":"Allow","style":"primary","role":"advance","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#)
        let close = try action(#"{"id":"close_unmerged:t-1@1","label":"Close without merging","style":"destructive","role":"risk","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true}"#)
        let changes = try action(Self.changes)
        XCTAssertEqual(allow.paintedRole, .advance)
        XCTAssertEqual(close.paintedRole, .risk)
        XCTAssertEqual(changes.paintedRole, .neutral, "a button the server left unpainted reads plain")
    }

    func testOnlyADestructiveActionConfirmsInRed() throws {
        let interrupt = try action(#"{"id":"interrupt:n-1@1","label":"Interrupt","style":"secondary","presentation":"button","input":{"kind":"none"},"attachments":false,"confirm":{"title":"Interrupt","message":"Stop what this node is doing now? It keeps its task and waits for you.","button":"Interrupt"},"enabled":true}"#)
        let close = try action(#"{"id":"close_unmerged:t-1@1","label":"Close without merging","style":"destructive","presentation":"button","input":{"kind":"none"},"attachments":false,"confirm":{"title":"Close PR #12 without merging?","message":"Alondra closes the PR on GitHub and cancels this task.","button":"Close PR"},"enabled":true}"#)
        var state = ActionFeature.State(dataEpoch: "e", client: .ios, lang: "en")
        state.action = interrupt
        XCTAssertFalse(state.confirmationIsDestructive, "interrupting is a question, not a danger")
        state.action = close
        XCTAssertTrue(state.confirmationIsDestructive)
    }

    @MainActor
    func testARefusalPutsTheCardBackWithTheServersWordsAndTheDelegateHearsThem() async throws {
        let store = TestStore(initialState: ActionFeature.State(dataEpoch: "e", client: .ios, lang: "en")) { ActionFeature() }
        store.exhaustivity = .off
        await store.send(.response(try reply(Self.refused))) {
            $0.error = "PR #12 must merge first: this one builds on it."
        }
        await store.receive(\.delegate.failed, "PR #12 must merge first: this one builds on it.")
    }

    @MainActor
    func testAStartedOutcomeTellsTheScreenWhatTheServerSaid() async throws {
        let store = TestStore(initialState: ActionFeature.State(dataEpoch: "e", client: .ios, lang: "en")) { ActionFeature() }
        store.exhaustivity = .off
        await store.send(.response(try reply(Self.merging)))
        await store.receive(\.delegate.completed, "PR #734 is merging.")
    }

    @MainActor
    func testAChangedOutcomeOpensTheQuestionAndGoingOnSendsTheOfferedAction() async throws {
        let sent = LockIsolated<[ActionRequest]>([])
        let store = TestStore(initialState: ActionFeature.State(dataEpoch: "e", client: .ios, lang: "en")) { ActionFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.viewsClient.perform = { request in
                sent.withValue { $0.append(request) }
                return try ContractCodec.decode(ActionResponse.self, from: Data(Self.merging.utf8))
            }
        }
        store.exhaustivity = .off
        await store.send(.present(try action(Self.changes)))
        await store.send(.response(try reply(Self.changed)))
        XCTAssertEqual(store.state.changed?.title, "PR #12 has new commits")
        await store.send(.sendChangedAction)
        await store.finish()
        XCTAssertEqual(sent.value.map(\.actionId), ["changes:pr-12@3"])
    }

    @MainActor
    func testAOneMethodMergeSendsItsMethodWithoutAnyPickOrConfirmation() async throws {
        let merge = try action(#"{"id":"merge:t-1@1","label":"Merge PR #12","style":"primary","presentation":"button","option_id":"squash","description":"Squash: the only way this repository allows","input":{"kind":"none"},"attachments":false,"enabled":true}"#)
        let sent = LockIsolated<[ActionRequest]>([])
        let store = TestStore(initialState: ActionFeature.State(dataEpoch: "e", client: .mac, lang: "en")) { ActionFeature() } withDependencies: {
            $0.uuid = .incrementing
            $0.viewsClient.perform = { request in
                sent.withValue { $0.append(request) }
                return try ContractCodec.decode(ActionResponse.self, from: Data(Self.merging.utf8))
            }
        }
        store.exhaustivity = .off
        await store.send(.present(merge))
        await store.send(.submit)
        XCTAssertNil(store.state.confirmation)
        await store.finish()
        XCTAssertEqual(sent.value.map(\.optionId), ["squash"])
    }
}
