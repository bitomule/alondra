import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import XCTest

final class ScheduleEditorTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_767_225_600)

    @MainActor
    private func loaded(
        _ sample: String = "schedule-edit",
        key: String = "schedule:a1",
        sent: SentActions = SentActions(),
        dismissed: LockIsolated<Int> = LockIsolated(0),
        delegates: LockIsolated<[ScreenDelegate]> = LockIsolated([]),
        respond: @escaping @Sendable (ActionRequest) throws -> ActionResponse = { _ in try Responses.applied("Saved") },
        load: @escaping @Sendable (ViewKey) -> AsyncThrowingStream<ViewFetch, any Error>
    ) async throws -> TestStoreOf<ScheduleEditorFeature> {
        let probe = Reduce<ScheduleEditorFeature.State, ScheduleEditorFeature.Action> { _, action in
            if case .delegate(let delegate) = action { delegates.withValue { $0.append(delegate) } }
            return .none
        }
        let store = TestStore(initialState: ScheduleEditorFeature.State(key: ViewKey(rawValue: key))) {
            CombineReducers {
                probe
                ScheduleEditorFeature()
            }
        } withDependencies: {
            $0.viewsClient.observe = load
            $0.viewsClient.perform = { request in sent.record(request); return try respond(request) }
            $0.uuid = .incrementing
            $0.date.now = now
            $0.dismiss = DismissEffect { dismissed.withValue { $0 += 1 } }
        }
        store.exhaustivity = .off
        await store.send(.task)
        await store.finish()
        await store.skipReceivedActions()
        return store
    }

    @MainActor
    private func loaded(
        _ sample: String = "schedule-edit",
        key: String = "schedule:a1",
        sent: SentActions = SentActions(),
        dismissed: LockIsolated<Int> = LockIsolated(0),
        delegates: LockIsolated<[ScreenDelegate]> = LockIsolated([]),
        respond: @escaping @Sendable (ActionRequest) throws -> ActionResponse = { _ in try Responses.applied("Saved") }
    ) async throws -> TestStoreOf<ScheduleEditorFeature> {
        try await loaded(sample, key: key, sent: sent, dismissed: dismissed, delegates: delegates, respond: respond,
                         load: ViewsClient.showing(try Samples.document(sample)))
    }

    @MainActor
    func testTheFormStartsFromTheValuesTheDaemonSent() async throws {
        let store = try await loaded()

        XCTAssertEqual(store.state.text("name"), "Morning digest")
        XCTAssertEqual(store.state.value("weekday"), .integer(4))
        XCTAssertEqual(store.state.text("time"), "07:30")
        XCTAssertEqual(store.state.value("active"), .bool(true))
        XCTAssertEqual(store.state.title, "Morning digest")
        XCTAssertFalse(store.state.isNew)
        XCTAssertEqual(store.state.notices.map(\.text), ["Delivered"], "how the last run went")
    }

    @MainActor
    func testOnlyTheFieldsTheRepeatModeNeedsAreVisible() async throws {
        let store = try await loaded()
        func visible() -> [String] {
            store.state.form?.fields.filter(store.state.isVisible).map(\.id) ?? []
        }

        XCTAssertTrue(visible().contains("weekday"))
        XCTAssertTrue(visible().contains("time"))
        XCTAssertFalse(visible().contains("interval"))
        XCTAssertFalse(visible().contains("start_at"))

        await store.send(.set("repeat", .string("every_days")))
        XCTAssertTrue(visible().contains("interval"))
        XCTAssertTrue(visible().contains("start_at"))
        XCTAssertFalse(visible().contains("weekday"))
        XCTAssertFalse(visible().contains("time"))
    }

    @MainActor
    func testChoosingARepeatThatStartsOnADateFillsInToday() async throws {
        let store = try await loaded()
        XCTAssertEqual(store.state.value("start_at"), .null)

        await store.send(.set("repeat", .string("once")))

        XCTAssertEqual(store.state.value("start_at"), .integer(1_767_225_600_000))
    }

    @MainActor
    func testADateAlreadyChosenIsKeptWhenTheRepeatChanges() async throws {
        let store = try await loaded("schedule-new-custom", key: "schedule:new")
        XCTAssertEqual(store.state.value("start_at"), .integer(1_767_254_400_000))

        await store.send(.set("repeat", .string("every_months")))

        XCTAssertEqual(store.state.value("start_at"), .integer(1_767_254_400_000))
    }

    @MainActor
    func testSavingNeedsANameAndAMission() async throws {
        let sent = SentActions()
        let store = try await loaded("schedule-new-custom", key: "schedule:new", sent: sent)
        XCTAssertFalse(store.state.canSave)

        await store.send(.save)
        XCTAssertTrue(sent.requests.isEmpty)

        await store.send(.set("name", .string("  ")))
        await store.send(.set("mission", .string("Check the logs")))
        XCTAssertFalse(store.state.canSave, "a name of spaces is no name")

        await store.send(.set("name", .string("Logs")))
        XCTAssertTrue(store.state.canSave)
    }

    @MainActor
    func testSavingSendsEveryFieldAndClosesTheEditorWithAToast() async throws {
        let sent = SentActions()
        let dismissed = LockIsolated(0)
        let delegates = LockIsolated<[ScreenDelegate]>([])
        let store = try await loaded(sent: sent, dismissed: dismissed, delegates: delegates)

        await store.send(.set("name", .string("Evening digest")))
        await store.send(.save)
        await store.receive(\.delegate.completed, "Saved")

        let request = try XCTUnwrap(sent.requests.first)
        XCTAssertEqual(request.actionId, "sched_save:a1@3")
        let fields = try XCTUnwrap(request.input.fields)
        XCTAssertEqual(fields["name"], .string("Evening digest"))
        XCTAssertEqual(fields["repeat"], .string("weekly"))
        XCTAssertEqual(fields["weekday"], .integer(4))
        XCTAssertEqual(fields["time"], .string("07:30"))
        XCTAssertEqual(fields["disallowed_tools"], .string("Bash(rm *)"))
        XCTAssertEqual(fields["pr"], .string("review"))
        await store.finish()
        XCTAssertEqual(dismissed.value, 1)
        XCTAssertEqual(delegates.value, [.completed("Saved")])
    }

    @MainActor
    func testARefusalNamesTheFieldAndKeepsTheEditorOpen() async throws {
        let dismissed = LockIsolated(0)
        let store = try await loaded(dismissed: dismissed, respond: { _ in
            try Responses.rejected("Choose an interval from 1 to 365.", field: "interval")
        })

        await store.send(.save)
        await store.receive(\.actions.response)

        XCTAssertEqual(store.state.errors, ["interval": "Choose an interval from 1 to 365."])
        XCTAssertNil(store.state.formError)
        await store.finish()
        XCTAssertEqual(dismissed.value, 0)
    }

    @MainActor
    func testARefusalWithoutAFieldIsShownOnTheForm() async throws {
        let store = try await loaded(respond: { _ in try Responses.rejected("That schedule changed on your Mac") })

        await store.send(.save)
        await store.receive(\.actions.response)

        XCTAssertEqual(store.state.formError, "That schedule changed on your Mac")
        XCTAssertTrue(store.state.errors.isEmpty)
    }

    @MainActor
    func testEditingAFieldClearsItsErrorAndTheFormError() async throws {
        let store = try await loaded(respond: { _ in try Responses.rejected("Enter a name of up to 120 characters.", field: "name") })
        await store.send(.save)
        await store.receive(\.actions.response)
        XCTAssertNotNil(store.state.errors["name"])

        await store.send(.set("name", .string("Digest")))

        XCTAssertNil(store.state.errors["name"])
    }

    @MainActor
    func testAFieldErrorDoesNotAlsoBecomeAToast() async throws {
        let delegates = LockIsolated<[ScreenDelegate]>([])
        let store = try await loaded(delegates: delegates, respond: { _ in
            try Responses.rejected("Choose a valid time.", field: "time")
        })

        await store.send(.save)
        await store.receive(\.actions.response)
        await store.finish()

        XCTAssertEqual(store.state.errors, ["time": "Choose a valid time."])
        XCTAssertEqual(delegates.value, [], "the message is on the field, not also in a toast")
    }

    @MainActor
    func testSavingShowsItIsSavingUntilTheDaemonAnswersEitherWay() async throws {
        for reply in [try Responses.applied("Saved"), try Responses.rejected("Choose a valid time.", field: "time")] {
            let gate = Gate()
            let store = try await loaded()
            store.dependencies.viewsClient.perform = { _ in
                await gate.wait()
                return reply
            }
            await store.send(.set("name", .string("Evening digest")))
            XCTAssertFalse(store.state.isSaving)

            await store.send(.save)
            await store.receive(\.actions.sent)
            XCTAssertTrue(store.state.isSaving, "the save waits for the daemon and says so")
            XCTAssertFalse(store.state.canSave, "a second tap cannot send it twice")

            await gate.open()
            await store.receive(\.actions.response)
            XCTAssertFalse(store.state.isSaving, "\(reply.outcome)")
            await store.finish()
        }
    }

    @MainActor
    func testASaveThatCannotReachTheDaemonStopsSaving() async throws {
        let gate = Gate()
        let store = try await loaded()
        store.dependencies.viewsClient.perform = { _ in
            await gate.wait()
            throw URLError(.notConnectedToInternet)
        }
        await store.send(.save)
        await store.receive(\.actions.sent)
        XCTAssertTrue(store.state.isSaving)
        await gate.open()
        await store.receive(\.actions.failed)
        XCTAssertFalse(store.state.isSaving)
    }

    @MainActor
    func testASaveThatCouldNotBeSentIsAToastAndKeepsWhatWasTyped() async throws {
        let store = try await loaded(respond: { _ in throw URLError(.notConnectedToInternet) })

        await store.send(.set("name", .string("Evening digest")))
        await store.send(.save)
        await store.receive(\.delegate.failed)

        XCTAssertEqual(store.state.text("name"), "Evening digest")
        XCTAssertFalse(store.state.actions.inFlight)
    }

    @MainActor
    func testPickingAProjectOfANewScheduleLoadsItsOptionsAndKeepsWhatWasTyped() async throws {
        let first = try Samples.document("schedule-new-custom")
        let reloaded = try Samples.document("schedule-new-custom") { object in
            object["key"] = "schedule:new?project=Alondra"
            object["version"] = "vproject"
            var sections = object["sections"] as? [[String: Any]] ?? []
            var items = sections[0]["items"] as? [[String: Any]] ?? []
            var form = items[0]
            var fields = form["fields"] as? [[String: Any]] ?? []
            for index in fields.indices {
                switch fields[index]["id"] as? String {
                case "project": fields[index]["value"] = "Alondra"
                case "effort": fields[index]["value"] = "hard"
                case "name": fields[index]["value"] = ""
                default: break
                }
            }
            form["fields"] = fields
            items[0] = form
            sections[0]["items"] = items
            object["sections"] = sections
        }
        let asked = LockIsolated<[ViewKey]>([])
        let store = try await loaded("schedule-new-custom", key: "schedule:new", load: { key in
            asked.withValue { $0.append(key) }
            return ViewsClient.showing(key.rawValue == "schedule:new" ? first : reloaded)(key)
        })
        await store.send(.set("name", .string("Logs")))
        await store.send(.set("effort", .string("light")))

        await store.send(.set("project", .string("Alondra")))
        await store.receive(\.screen.loaded)
        await store.finish()

        XCTAssertEqual(asked.value.map(\.rawValue), ["schedule:new", "schedule:new?project=Alondra"])
        XCTAssertEqual(store.state.text("name"), "Logs", "what the person typed survives the reload")
        XCTAssertEqual(store.state.value("project"), .string("Alondra"))
        XCTAssertEqual(store.state.value("effort"), .string("hard"), "the launch options come from the chosen project")
    }

    @MainActor
    func testPickingAProjectOfAnExistingScheduleOnlyChangesTheValue() async throws {
        let asked = LockIsolated<[ViewKey]>([])
        let store = try await loaded(load: { key in
            asked.withValue { $0.append(key) }
            return ViewsClient.showing(try! Samples.document("schedule-edit"))(key)
        })

        await store.send(.set("project", .string("Alondra")))
        await store.finish()

        XCTAssertEqual(asked.value.map(\.rawValue), ["schedule:a1"])
        XCTAssertEqual(store.state.value("project"), .string("Alondra"))
    }

    @MainActor
    func testAFreshCopyOfTheScheduleDoesNotOverwriteWhatIsBeingEdited() async throws {
        let store = try await loaded()
        await store.send(.set("name", .string("Mine")))

        let fresh = try Samples.document("schedule-edit") { $0["version"] = "vfresh" }
        await store.send(.screen(.loaded(fresh)))

        XCTAssertEqual(store.state.text("name"), "Mine")
        XCTAssertEqual(store.state.screen.document?.version, "vfresh")
    }
}
