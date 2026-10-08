import AlondraContract
import AlondraFeatures
import Foundation
import XCTest

final class TasksBoardTests: XCTestCase {
    func testRequestsKeepTheirTaskTreeInOrderWithTheRailsEachRowNeeds() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated"))

        XCTAssertEqual(board.active.requests.map(\.id), ["r-5", "r-1", "r-2"])
        let first = try XCTUnwrap(board.active.requests.first { $0.id == "r-1" })
        XCTAssertEqual(first.tasks.map(\.id), ["t-1", "t-2", "t-3"], "what a node does now is not a task")
        XCTAssertEqual(first.tasks.map(\.depth), [0, 0, 1])
        XCTAssertEqual(first.tasks.map(\.hasChildren), [false, true, false])
        XCTAssertEqual(first.tasks.map(\.isLastSibling), [false, true, true])
        XCTAssertEqual(first.tasks[2].rails, [false], "its parent is the last task on its level, so no rail runs past it")
        XCTAssertEqual(first.tasks.map(\.waiting), [4, 1, 0])
        XCTAssertFalse(first.isSingleTask)
        XCTAssertTrue(try XCTUnwrap(board.active.requests.last).isSingleTask)
    }

    func testATaskSaysWhatItsNodeDoesNowAndWhen() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated"))
        let tasks = try XCTUnwrap(board.active.requests.first { $0.id == "r-1" }).tasks

        XCTAssertEqual(tasks.map(\.activity), ["Asked whether to keep the old review sheet", "xcodebuild test -scheme Alondra", nil])
        XCTAssertEqual(tasks.map(\.activityAt), ["2025-12-31T22:05:00.000Z", "2025-12-31T23:52:00.000Z", nil])
    }

    func testARequestTheOrchestratorIsStillSplittingHasNoTasksYet() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated"))
        let request = try XCTUnwrap(board.active.requests.first)

        XCTAssertEqual(request.id, "r-5")
        XCTAssertEqual(request.state, "The orchestrator is splitting it")
        XCTAssertTrue(request.tasks.isEmpty)
    }

    func testARequestCarriesItsProjectStateAndWhenItWasLastTouched() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated"))
        let request = try XCTUnwrap(board.active.requests.first { $0.id == "r-1" })

        XCTAssertEqual(request.project, "Alondra")
        XCTAssertEqual(request.state, "Active")
        XCTAssertEqual(request.touchedAt, Date(timeIntervalSince1970: 1_767_216_600))
        XCTAssertEqual(request.link, .view(key: "task:t-2"), "a request opens its most recently changed task")
        XCTAssertEqual(request.tasks.first?.tone, .warning)
        XCTAssertEqual(request.tasks.first?.icon, .action)
    }

    func testAFinishedRequestOpensItsLastTaskAndOneWithoutTasksOpensNothing() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated"))

        XCTAssertEqual(board.finished.requests.map(\.link), [.view(key: "task:t-5"), nil])
        XCTAssertEqual(board.finished.requests.map(\.tasks.isEmpty), [false, true])
    }

    func testFinishedAndArchivedWorkIsFoldedUnderItsCountWithTheNextPageKey() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated"))

        XCTAssertEqual(board.finished.count, 27)
        XCTAssertEqual(board.finished.requests.map(\.id), ["r-3", "r-4"])
        XCTAssertEqual(board.finished.more?.key, "tasks?section=finished&after=20")
        XCTAssertEqual(board.finished.more?.remaining, 7)
        XCTAssertEqual(board.archived.count, 0)
        XCTAssertNil(board.archived.more)
        XCTAssertTrue(try XCTUnwrap(board.finished.requests.last).tasks.isEmpty, "a withdrawn request has no tasks")
    }

    func testTheEntryToScheduledTasksAndTheArchiveActionComeFromTheirSections() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated"))

        XCTAssertEqual(board.schedules?.count, 3)
        XCTAssertEqual(board.schedules?.link, .view(key: "schedules"))
        XCTAssertEqual(board.archiveCompleted?.id, "archive_done:view@12")
    }

    func testWithoutTerminalTasksThereIsNothingToArchive() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated") { object in
            object["sections"] = (object["sections"] as? [[String: Any]] ?? []).filter { $0["id"] as? String != "actions" }
        })
        XCTAssertNil(board.archiveCompleted)

        let daemons = TasksBoard(document: try Fixtures.document("tasks-ios"))
        XCTAssertNotNil(daemons.archiveCompleted, "the BlurCamera request has finished tasks")
        XCTAssertEqual(daemons.schedules?.count, 0, "the daemon sends no count when nothing is scheduled")
        XCTAssertEqual(daemons.active.requests.count, 7)
    }

    func testAScheduledRunInProgressIsAnActiveRequestMarkedScheduled() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        let request = try XCTUnwrap(board.active.requests.first { $0.id == "r-scheduled" })
        let task = try XCTUnwrap(request.tasks.first)

        XCTAssertTrue(request.isSingleTask)
        XCTAssertEqual(task.title, "Lumo · weekly improvement")
        XCTAssertEqual(task.state, "Working")
        XCTAssertEqual(task.marks, ["Scheduled"])
        XCTAssertEqual(task.tone, .accent, "the mark never takes the state's place")

        let spanish = TasksBoard(document: try Fixtures.document("tasks-ios-es"))
        let marks = spanish.active.requests.first { $0.id == "r-scheduled" }?.tasks.first?.marks
        XCTAssertEqual(marks, ["Programada"])
        XCTAssertEqual(board.active.requests.first { $0.id == "r-badge" }?.tasks.first?.marks, [])
    }

    func testATaskThatWaitsOnAnotherRequestsTaskIsNestedThereAndLeavesAShortRowInItsOwn() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        let parent = try XCTUnwrap(board.active.requests.first { $0.title == "Polish the Alondra beta" })
        let nested = try XCTUnwrap(parent.tasks.last)

        XCTAssertEqual(parent.tasks.map(\.depth), [0, 1])
        XCTAssertEqual(nested.id, "t-badge-decrement")
        XCTAssertEqual(nested.origin, "from: Badge accuracy")
        XCTAssertEqual(parent.taskCount, 1, "a task of another request is not one of its tasks")
        XCTAssertFalse(parent.isSingleTask, "the nested task makes it a group, so its title shows")

        let own = try XCTUnwrap(board.active.requests.first { $0.title == "Badge accuracy" })
        XCTAssertEqual(own.tasks.map(\.isWait), [false, true])
        let waits = try XCTUnwrap(own.tasks.last)
        XCTAssertEqual(waits.title, "waits for Restore the native review experience")
        XCTAssertEqual(waits.dependent, "Decrement the badge on retraction")
        XCTAssertNil(waits.state)
        XCTAssertEqual(waits.link, .view(key: "task:t-badge-decrement"))
        XCTAssertEqual(own.taskCount, 1)
    }

    func testTheTasksOfAnUntitledRequestWithSeveralTasksStandLooseWithoutAHeader() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        let loose = board.active.requests.filter(\.isLoose)

        XCTAssertEqual(loose.map(\.id), ["t-legacy-editor", "t-legacy-notes"])
        XCTAssertEqual(loose.map { $0.tasks.map(\.title) }, [["Review the schedule editor"], ["Write the release notes"]])
        XCTAssertEqual(loose.map(\.title), ["", ""])
    }

    func testARequestStillBeingSplitIsNamedNewAndNeverByItsPrompt() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        let request = try XCTUnwrap(board.active.requests.first { $0.id == "r-new" })

        XCTAssertEqual(request.title, "New request")
        XCTAssertTrue(request.tasks.isEmpty)
    }

    func testARequestThatOnlyAnsweredIsFinishedAsAnswered() throws {
        let page = try Fixtures.document("tasks-ios-finished")
        let rows = page.sections.first { $0.id == "finished" }?.items.compactMap { item -> Row? in
            if case .row(let row) = item { row } else { nil }
        } ?? []
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"), pages: ["finished": TasksPage(rows: rows, more: nil)])

        XCTAssertEqual(board.finished.requests.first { $0.id == "r-answer" }?.state, "Answered")
    }

    func testTheListCarriesOnlyHowMuchIsFinishedAndTheEndedTasksOfLiveRequestsFoldThere() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        XCTAssertTrue(board.finished.requests.isEmpty)
        XCTAssertEqual(board.finished.more?.remaining, UInt32(board.finished.count))
        let live = try XCTUnwrap(board.active.requests.first { $0.id == "r-blurcamera" })
        XCTAssertEqual(live.tasks.map(\.title), ["Link the website in app.json", "Photos extension hint after the first blur"])
        XCTAssertEqual(live.tasks.map(\.state), ["Won't start: Claude Code doesn't trust ~/Projects/indie-apps", "No activity"])

        let page = try Fixtures.document("tasks-ios-finished")
        let ended = page.sections.first { $0.id == "finished" }?.items.compactMap { item -> String? in
            if case .row(let row) = item { row.id } else { nil }
        }
        XCTAssertEqual(ended?.contains("r-blurcamera:ended"), true)
    }

    func testAnEmptyBoardSaysSo() throws {
        let board = TasksBoard(document: try Samples.document("tasks-empty"))

        XCTAssertTrue(board.isEmpty)
        XCTAssertEqual(board.empty?.text, "No tasks yet")
    }

    func testLoadedPagesFollowTheSectionsOwnRowsAndReplaceTheShowMoreKey() throws {
        let document = try Samples.document("tasks-populated")
        let next = try XCTUnwrap(
            try Samples.document("tasks-populated").sections.first { $0.id == "active" }?.items.compactMap { item -> Row? in
                if case .row(let row) = item { row } else { nil }
            }.last
        )
        let more = try ContractCodec.decode(More.self, from: Data(#"{"key":"tasks?section=finished&after=40","remaining":2}"#.utf8))
        let page = TasksPage(rows: [next], more: more)

        let board = TasksBoard(document: document, pages: ["finished": page])

        XCTAssertEqual(board.finished.requests.map(\.id), ["r-3", "r-4", "r-2"])
        XCTAssertEqual(board.finished.more?.remaining, 2)
        XCTAssertEqual(board.active.requests.count, 3, "another section's rows are not touched")
    }
}
