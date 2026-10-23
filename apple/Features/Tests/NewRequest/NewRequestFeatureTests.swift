import AlondraAttachments
import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraTransport
import ComposableArchitecture
import Foundation
import XCTest

final class NewRequestFeatureTests: XCTestCase {
    private let first = "00000000-0000-0000-0000-000000000000"
    private let second = "00000000-0000-0000-0000-000000000001"

    /// The daemon's New view, at `version`, with its queue showing `rows` (id and text).
    private func view(version: String? = nil, queue rows: [(String, String)]? = nil) throws -> ViewDocument {
        let base = try Fixtures.document("new-request-ios", version: version)
        guard let rows else { return base }
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: ContractCodec.encode(base)) as? [String: Any])
        replaceItems(of: "sent", in: &object, with: rows.map { id, title in
            ["type": "row", "id": id, "icon": "request", "title": title,
             "badges": [["text": "Queued", "tone": "neutral"]], "depth": 0, "struck": false]
        })
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    @MainActor
    private func composer(
        sent: SentActions = SentActions(),
        clock: TestClock<Duration> = TestClock(),
        loaded: Bool = true,
        attachments: [AttachmentFeature.Entry] = [],
        uuid: UUIDGenerator = .incrementing,
        respond: @escaping @Sendable (ActionRequest) async throws -> ActionResponse = { _ in try Responses.applied("Request sent") }
    ) async throws -> TestStoreOf<NewRequestFeature> {
        var state = NewRequestFeature.State()
        state.attachments.entries = attachments
        let store = TestStore(initialState: state) { NewRequestFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
            $0.viewsClient.perform = { request in sent.record(request); return try await respond(request) }
            $0.continuousClock = clock
            $0.uuid = uuid
        }
        store.exhaustivity = .off
        if loaded { await store.send(.screen(.loaded(try view()))) }
        return store
    }

    @MainActor
    func testTheDraftStartsFromTheValuesTheDaemonSent() async throws {
        let store = try await composer()

        XCTAssertEqual(store.state.values["text"], .string(""))
        XCTAssertEqual(store.state.values["pr"], .string("review"))
        XCTAssertEqual(store.state.values["effort"], .string("auto"))
        XCTAssertNil(store.state.project, "no project means the orchestrator picks")
        XCTAssertEqual(store.state.optionsSummary, ["Review each one", "Auto"])
        XCTAssertEqual(store.state.recent.map(\.title), ["Import lists from Notes"])
        XCTAssertFalse(store.state.canSend)
    }

    @MainActor
    func testWritingNeedsNothingFromTheDaemon() async throws {
        let store = try await composer(loaded: false)

        await store.send(.textChanged("Polish the beta"))

        XCTAssertNil(store.state.form, "the options have not arrived")
        XCTAssertTrue(store.state.canSend)
        await store.send(.textChanged("   "))
        XCTAssertFalse(store.state.canSend, "spaces are not a request")
    }

    @MainActor
    func testSendingClearsTheComposerAndQueuesTheRequestBeforeTheDaemonAnswers() async throws {
        let gate = Gate()
        let store = try await composer { _ in await gate.wait(); return try Responses.applied() }

        await store.send(.textChanged("  Polish the beta  "))
        await store.send(.send)

        XCTAssertEqual(store.state.text, "", "the composer is ready for the next request at once")
        XCTAssertEqual(store.state.unshown.map(\.text), ["Polish the beta"])
        XCTAssertEqual(store.state.unshown.first?.phase, .sending)
        XCTAssertEqual(store.state.unshown.first?.id, first)
        await gate.open()
    }

    @MainActor
    func testRequestsGoOneAfterAnotherWithoutWaitingForEachOther() async throws {
        let gate = Gate()
        let sent = SentActions()
        let store = try await composer(sent: sent) { _ in await gate.wait(); return try Responses.applied() }

        await store.send(.textChanged("First"))
        await store.send(.send)
        await store.send(.textChanged("Second"))
        await store.send(.send)
        while sent.requests.count < 2 { await Task.yield() }

        XCTAssertEqual(store.state.unshown.map(\.text), ["Second", "First"], "newest on top")
        XCTAssertEqual(sent.requests.map(\.commandId), [first, second])
        await gate.open()
    }

    @MainActor
    func testTheRequestCarriesWhatWasWrittenAndChosenUnderItsQueueID() async throws {
        let sent = SentActions()
        let store = try await composer(sent: sent)

        await store.send(.textChanged("Polish the beta"))
        await store.send(.set("effort", .string("hard")))
        await store.send(.send)
        await store.receive(\.answered)

        let request = try XCTUnwrap(sent.requests.first)
        XCTAssertEqual(request.commandId, first, "the daemon names the request by this ID, so its row replaces ours")
        XCTAssertEqual(request.actionId, "new_request:view@6")
        XCTAssertEqual(request.client, .ios)
        XCTAssertEqual(request.input.fields?["text"], .string("Polish the beta"))
        XCTAssertEqual(request.input.fields?["effort"], .string("hard"))
        XCTAssertEqual(store.state.values["effort"], .string("hard"), "the options stay for the next request")
        XCTAssertEqual(store.state.unshown.first?.phase, .applied(shown: "vbcce706b7df9fa50"))
    }

    @MainActor
    func testARefusedRequestStaysInTheQueueWithItsReasonAndItsText() async throws {
        let store = try await composer { _ in try Responses.rejected("Alondra can’t take requests right now") }

        await store.send(.textChanged("Polish the beta"))
        await store.send(.send)
        await store.receive(\.answered)
        await store.send(.screen(.loaded(try view(version: "v-later"))))

        XCTAssertEqual(store.state.unshown.map(\.text), ["Polish the beta"])
        XCTAssertEqual(store.state.unshown.first?.phase, .failed("Alondra can’t take requests right now"))
        XCTAssertEqual(store.state.text, "", "nothing is put back over what is being written")
    }

    @MainActor
    func testARequestThatCanNeverBeDeliveredStaysInTheQueueSayingSo() async throws {
        let store = try await composer { _ in throw ActionDeliveryError.dataReset }

        await store.send(.textChanged("Polish the beta"))
        await store.send(.send)
        await store.receive(\.undelivered)

        XCTAssertEqual(store.state.unshown.first?.isFailed, true)
    }

    @MainActor
    func testTryingAgainSendsTheSameRequestUnderTheSameID() async throws {
        let sent = SentActions()
        let refuse = LockIsolated(true)
        let store = try await composer(sent: sent) { _ in
            refuse.value ? try Responses.rejected("Not now") : try Responses.applied()
        }
        await store.send(.textChanged("Polish the beta"))
        await store.send(.set("effort", .string("hard")))
        await store.send(.send)
        await store.receive(\.answered)
        refuse.setValue(false)

        await store.send(.retry(first))
        await store.receive(\.answered)

        XCTAssertEqual(sent.requests.map(\.commandId), [first, first], "the daemon never takes it twice")
        XCTAssertEqual(sent.requests.last?.input, sent.requests.first?.input, "nothing written or chosen is lost")
        XCTAssertEqual(store.state.unshown.first?.phase, .applied(shown: "vbcce706b7df9fa50"))
    }

    @MainActor
    func testDiscardingAFailedRequestTakesItOutOfTheQueue() async throws {
        let store = try await composer { _ in try Responses.rejected("Not now") }
        await store.send(.textChanged("Polish the beta"))
        await store.send(.send)
        await store.receive(\.answered)

        await store.send(.discard(first))

        XCTAssertTrue(store.state.unshown.isEmpty)
        XCTAssertTrue(store.state.outbox.isEmpty)
    }

    @MainActor
    func testARequestTheDaemonDoesNotConfirmInTimeSaysSoAndKeepsGoing() async throws {
        let clock = TestClock()
        let gate = Gate()
        let store = try await composer(clock: clock) { _ in await gate.wait(); return try Responses.applied() }
        await store.send(.textChanged("Polish the beta"))
        await store.send(.send)

        await clock.advance(by: .seconds(29))
        XCTAssertEqual(store.state.unshown.first?.phase, .sending)
        await clock.advance(by: .seconds(1))
        await store.receive(\.expired)
        XCTAssertEqual(store.state.unshown.first?.phase, .unconfirmed)

        await gate.open()
        await store.receive(\.answered)
        XCTAssertEqual(store.state.unshown.first?.phase, .applied(shown: "vbcce706b7df9fa50"))
    }

    @MainActor
    func testTheDaemonsRowTakesThePlaceOfOursWithoutShowingItTwice() async throws {
        let store = try await composer()
        await store.send(.textChanged("Polish the beta"))
        await store.send(.send)
        await store.receive(\.answered)

        await store.send(.screen(.loaded(try view(version: "v-taken", queue: [(first, "Polish the beta"), ("r-old", "Import lists")]))))

        XCTAssertTrue(store.state.unshown.isEmpty)
        XCTAssertEqual(store.state.recent.map(\.id), [first, "r-old"])
        XCTAssertTrue(store.state.outbox.isEmpty)
    }

    /// 16 Oct, iPhone on 4G: one request showed twice under Pending Requests, because the daemon
    /// names it by the operation that carried it, never by the bare id it was sent with.
    @MainActor
    func testTheDaemonsRowNamedByItsOperationTakesThePlaceOfOurs() async throws {
        let sent = UUID(uuidString: "27d86775-cd69-4956-92d0-4a20cd01c813")!
        let operation = "op_8e8dd8f1687d492b5a16fb35d2ca17cd_1_85_27d86775-cd69-4956-92d0-4a20cd01c813"
        let gate = Gate()
        let store = try await composer(uuid: .constant(sent)) { _ in await gate.wait(); return try Responses.applied() }
        await store.send(.textChanged("Bug for the Redraft node"))
        await store.send(.send)

        await store.send(.screen(.loaded(try view(version: "v-taken", queue: [(operation, "Bug for the Redraft node")]))))
        XCTAssertTrue(store.state.unshown.isEmpty, "the daemon's row is ours: it is shown once")
        XCTAssertEqual(store.state.recent.map(\.id), [operation])

        await gate.open()
        await store.receive(\.answered)
        XCTAssertTrue(store.state.unshown.isEmpty, "a late answer does not bring our copy back")
    }

    @MainActor
    func testAClosedRequestLeavesPendingRequests() async throws {
        let sent = UUID(uuidString: "27d86775-cd69-4956-92d0-4a20cd01c813")!
        let operation = "op_8e8dd8f1687d492b5a16fb35d2ca17cd_1_85_27d86775-cd69-4956-92d0-4a20cd01c813"
        let gate = Gate()
        let store = try await composer(uuid: .constant(sent)) { _ in await gate.wait(); return try Responses.applied() }
        await store.send(.textChanged("Bug for the Redraft node"))
        await store.send(.send)

        await store.send(.screen(.loaded(try view(version: "v-taken", queue: [(operation, "Bug for the Redraft node")]))))
        await store.send(.screen(.loaded(try view(version: "v-closed", queue: []))))
        await gate.open()
        await store.receive(\.answered)

        XCTAssertTrue(store.state.unshown.isEmpty, "closed while its answer was on its way: nothing is pending")
        XCTAssertTrue(store.state.recent.isEmpty)
    }

    @MainActor
    func testTheDaemonsRowNeverDuplicatesARequestStillOnItsWay() async throws {
        let gate = Gate()
        let store = try await composer { _ in await gate.wait(); return try Responses.applied() }
        await store.send(.textChanged("Polish the beta"))
        await store.send(.send)

        await store.send(.screen(.loaded(try view(version: "v-taken", queue: [(first, "Polish the beta")]))))

        XCTAssertTrue(store.state.unshown.isEmpty, "the daemon already shows it")
        XCTAssertEqual(store.state.recent.map(\.id), [first])
        await gate.open()
        await store.receive(\.answered)
        XCTAssertTrue(store.state.unshown.isEmpty, "a late answer does not bring our copy back")
    }

    @MainActor
    func testAViewDrawnBeforeTheDaemonTookItNeverTakesItAway() async throws {
        let gate = Gate()
        let store = try await composer { _ in await gate.wait(); return try Responses.applied() }
        await store.send(.textChanged("Polish the beta"))
        await store.send(.send)

        await store.send(.screen(.loaded(try view(version: "v-before", queue: []))))
        XCTAssertEqual(store.state.unshown.map(\.id), [first], "on its way: no view takes it out")

        await gate.open()
        await store.receive(\.answered)
        await store.send(.screen(.loaded(try view(version: "v-before", queue: []))))
        XCTAssertEqual(store.state.unshown.map(\.id), [first], "the view shown when the daemon took it is older than it")

        await store.send(.screen(.loaded(try view(version: "v-after", queue: []))))
        XCTAssertTrue(store.state.unshown.isEmpty, "a newer view without it: the orchestrator already took it out of the queue")
    }

    @MainActor
    func testARequestSentBeforeTheOptionsArriveGoesWhenTheyDo() async throws {
        let sent = SentActions()
        let store = try await composer(sent: sent, loaded: false)

        await store.send(.textChanged("Polish the beta"))
        await store.send(.send)
        XCTAssertEqual(store.state.text, "")
        XCTAssertEqual(store.state.unshown.first?.phase, .waiting(.options))
        XCTAssertTrue(sent.requests.isEmpty)

        await store.send(.screen(.loaded(try view())))
        await store.receive(\.answered)

        XCTAssertEqual(sent.requests.map(\.commandId), [first])
        XCTAssertEqual(sent.requests.first?.input.fields?["text"], .string("Polish the beta"))
    }

    @MainActor
    func testARequestWhoseAttachmentsAreStillUploadingGoesOnceTheyAre() async throws {
        let sent = SentActions()
        let upload = Gate()
        let receipt = try JSONDecoder().decode(ViewUploadedAttachment.self, from: Data(
            #"{"id":"att-1","name":"shot.png","content_digest":"d","content_reference":"r","bytes":3}"#.utf8))
        let draft = AttachmentFeature.Entry(draft: AttachmentDraft(name: "shot.png", data: Data([1, 2, 3])))
        let store = try await composer(sent: sent, attachments: [draft])
        store.dependencies.attachmentUpload = AttachmentUploadClient(upload: { _ in await upload.wait(); return receipt })

        await store.send(.textChanged("Look at this"))
        await store.send(.send)

        XCTAssertEqual(store.state.text, "")
        XCTAssertTrue(store.state.attachments.entries.isEmpty, "the attachments left with the request")
        XCTAssertEqual(store.state.unshown.first?.phase, .waiting(.attachments))
        XCTAssertTrue(sent.requests.isEmpty)

        await upload.open()
        await store.receive(\.answered)
        XCTAssertEqual(sent.requests.first?.input.attachments, ["att-1"])
    }

    @MainActor
    func testChoosingAProjectLoadsTheViewForThatProject() async throws {
        let keys = LockIsolated<[ViewKey]>([])
        let store = try await composer()
        store.dependencies.viewsClient.observe = { key in
            keys.withValue { $0.append(key) }
            return AsyncThrowingStream { _ in }
        }

        await store.send(.set("project", .string("Demo Garden")))
        await store.receive(\.screen.task)

        XCTAssertEqual(keys.value.last, .newRequest(project: "Demo Garden"))
        XCTAssertEqual(store.state.screen.key, .newRequest(project: "Demo Garden"))
        XCTAssertEqual(store.state.project, "Demo Garden")

        await store.send(.set("project", .string("")))
        XCTAssertEqual(store.state.screen.key, .newRequest())
        XCTAssertNil(store.state.project)
    }

    @MainActor
    func testOptionChangesFlowBackToTheDraft() async throws {
        let store = try await composer()

        await store.send(.optionsTapped)
        XCTAssertEqual(store.state.options?.fields.map(\.id), ["project", "pr", "effort", "account", "issue"])

        await store.send(.options(.presented(.choose("pr", "auto_merge"))))
        await store.skipReceivedActions()

        XCTAssertEqual(store.state.values["pr"], .string("auto_merge"))
        XCTAssertEqual(store.state.optionsSummary, ["Auto-merge", "Auto"])
    }

    @MainActor
    func testAPickedIssueBecomesPartOfTheRequestAndCanBeRemoved() async throws {
        let sent = SentActions()
        let store = try await composer(sent: sent)

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
        await store.receive(\.answered)
        XCTAssertEqual(sent.requests.first?.input.fields?["issue"], .string("me/App#4"))

        await store.send(.options(.presented(.removeIssue)))
        await store.skipReceivedActions()
        XCTAssertEqual(store.state.values["issue"], .string(""))
        XCTAssertEqual(store.state.optionsSummary, ["Review each one", "Auto"])
    }

    func testProjectsAreFilteredByEveryWordTyped() throws {
        let projects = try ContractCodec.decode([ChoiceItem].self, from: Data(
            #"[{"id":"","label":"Alondra picks the project"},{"id":"Demo Garden","label":"Demo Garden"},{"id":"Lumo","label":"Lumo"}]"#.utf8))

        XCTAssertEqual(ProjectChoices.filter(projects, query: "").count, 3)
        XCTAssertEqual(ProjectChoices.filter(projects, query: "gar dem").map(\.id), ["Demo Garden"])
        XCTAssertEqual(ProjectChoices.filter(projects, query: "orchestrator").map(\.id), [], "the default is not a project to search for")
        XCTAssertTrue(ProjectChoices.filter(projects, query: "zzz").isEmpty)
    }
}
