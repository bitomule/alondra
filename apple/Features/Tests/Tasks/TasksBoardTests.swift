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
        XCTAssertEqual(request.state, "Preparing its tasks")
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

    func testCompletedWorkIsFoldedUnderItsCountWithTheNextPageKey() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated"))

        XCTAssertEqual(board.finished.count, 27)
        XCTAssertEqual(board.finished.requests.map(\.id), ["r-3", "r-4"])
        XCTAssertEqual(board.finished.more?.key, "tasks?section=finished&after=20")
        XCTAssertEqual(board.finished.more?.remaining, 7)
        XCTAssertEqual(board.finished.title, "Completed")
        XCTAssertTrue(try XCTUnwrap(board.finished.requests.last).tasks.isEmpty, "a withdrawn request has no tasks")
    }

    func testTheEntryToScheduledTasksAndClearCompletedComeFromTheirSections() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated"))

        XCTAssertEqual(board.schedules?.count, 3)
        XCTAssertEqual(board.schedules?.link, .view(key: "schedules"))
        XCTAssertEqual(board.clearCompleted?.id, "clear_completed:view@12")
    }

    func testClearCompletedIsReadFromTheCompletedHeaderOnly() throws {
        let board = TasksBoard(document: try Samples.document("tasks-populated") { object in
            object["sections"] = (object["sections"] as? [[String: Any]] ?? []).map { section in
                var section = section
                section["actions"] = nil
                return section
            }
        })
        XCTAssertNil(board.clearCompleted)

        let daemons = TasksBoard(document: try Fixtures.document("tasks-ios"))
        XCTAssertEqual(daemons.clearCompleted?.command, "clear_completed", "the daemon offers it in the Completed header")
        XCTAssertEqual(daemons.finished.title, "Completed")
        XCTAssertEqual(daemons.schedules?.count, 0, "the daemon sends no count when nothing is scheduled")
        XCTAssertEqual(daemons.active.requests.count, 8)
        XCTAssertEqual(daemons.active.count, 8, "the active work is counted in tasks")
    }

    func testAScheduledRunInProgressIsAnActiveRequestMarkedScheduled() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        XCTAssertEqual(board.active.blocks.map(\.id), ["block:for-you", "block:in-progress", "block:waiting"])
        let request = try XCTUnwrap(board.active.requests.first { $0.id == "t-scheduled-lumo" })
        let task = try XCTUnwrap(request.tasks.first)

        XCTAssertTrue(request.isSingleTask)
        XCTAssertEqual(task.title, "Lumo · weekly improvement")
        XCTAssertEqual(task.state, "Working")
        XCTAssertEqual(task.marks, ["Scheduled", "Lumo", "running since 09:48"], "an app that does not count the time reads since when it runs")
        XCTAssertEqual(task.tone, .accent, "the mark never takes the state's place")

        let spanish = TasksBoard(document: try Fixtures.document("tasks-ios-es"))
        let marks = spanish.active.requests.first { $0.id == "t-scheduled-lumo" }?.tasks.first?.marks
        XCTAssertEqual(marks, ["Programada", "Lumo", "en marcha desde las 09:48"])
        XCTAssertEqual(board.active.requests.first { $0.id == "t-badge-count" }?.tasks.first?.marks, ["Demo Garden", "running since 09:20"])
        XCTAssertEqual(board.active.requests.first { $0.id == "t-legacy-editor" }?.tasks.first?.marks, ["running since 17:15"], "a task saved before projects has none")
    }

    func testTheActiveWorkComesInBlocksWithTheirTitleAndCount() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))

        XCTAssertEqual(board.active.blocks.map(\.title), ["For you", "In progress", "Waiting"])
        XCTAssertEqual(board.active.blocks.map(\.count), [1, 4, 3], "a block counts its tasks, not the ones folded in a chain")
        XCTAssertEqual(board.active.blocks.map(\.isCompact), [false, false, true], "only waiting work reads as short lines")
        let forYou = try XCTUnwrap(board.active.blocks.first)
        XCTAssertEqual(forYou.requests.flatMap(\.tasks).filter { $0.depth == 0 }.map(\.state), ["Needs you"],
                       "only a task with a card waiting for you is For you")
    }

    func testAChainOfWaitingTasksFoldsUnderItsFirstTaskFromOneAndOpensOneLevelDeep() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        let head = try XCTUnwrap(board.active.requests.first { $0.id == "t-651c38484cd2f34e74d5107ba05f7f15" })

        XCTAssertEqual(head.lines(open: []).map(\.title), ["Restore the native review experience", "+1 waiting for this"])
        XCTAssertEqual(head.lines(open: []).last?.isChain, true)
        let open = head.lines(open: ["chain:t-651c38484cd2f34e74d5107ba05f7f15"])
        XCTAssertEqual(open.map(\.title), ["Restore the native review experience", "+1 waiting for this", "Decrement the badge on retraction"])
        XCTAssertEqual(open.last?.state, "waits for Restore the native review experience")
        XCTAssertEqual(open.last?.chain, "chain:t-651c38484cd2f34e74d5107ba05f7f15")
        XCTAssertTrue(board.active.requests.flatMap(\.tasks).allSatisfy { !$0.isWait && $0.origin == nil },
                      "no «waits for» rows and no «from:» lines")
    }

    func testTheTasksOfAnUntitledRequestWithSeveralTasksStandLooseWithoutAHeader() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        let loose = board.active.requests.filter { $0.id.hasPrefix("t-legacy") }

        XCTAssertEqual(loose.map(\.id), ["t-legacy-editor", "t-legacy-notes"])
        XCTAssertEqual(loose.map(\.isLoose), [true, true])
        XCTAssertEqual(loose.map { $0.tasks.map(\.title) }, [["Review the schedule editor"], ["Write the release notes"]])
        XCTAssertEqual(loose.map(\.title), ["", ""])
    }

    func testARequestStillBeingSplitIsNamedByItsFirstSentence() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        let request = try XCTUnwrap(board.active.requests.first { $0.id == "r-new" })

        XCTAssertTrue(request.isLoose, "it reads as one more row of its block, its time inside the row")
        XCTAssertEqual(request.tasks.map(\.title), ["Import lists from Notes"])
        XCTAssertEqual(request.tasks.first?.state, "Preparing its tasks")
        XCTAssertNotNil(request.tasks.first?.activityAt)
        XCTAssertNil(request.tasks.first?.link)
    }

    func testARequestAnsweredWithoutTasksIsNotListed() throws {
        let page = try Fixtures.document("tasks-ios-finished")
        let rows = page.sections.first { $0.id == "finished" }?.items.compactMap { item -> Row? in
            if case .row(let row) = item { row } else { nil }
        } ?? []
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"), pages: ["finished": TasksPage(rows: rows, more: nil)])

        XCTAssertFalse(rows.isEmpty)
        XCTAssertNil(board.finished.requests.first { $0.id == "r-answer" }, "its answer is on its card in the inbox")
    }

    func testTheListCarriesOnlyHowMuchIsFinishedAndTheEndedTasksOfLiveRequestsFoldThere() throws {
        let board = TasksBoard(document: try Fixtures.document("tasks-ios"))
        XCTAssertTrue(board.finished.requests.isEmpty)
        XCTAssertEqual(board.finished.more?.remaining, UInt32(board.finished.count))
        let live = board.active.requests.filter { $0.id.hasPrefix("t-hf-") }
        XCTAssertEqual(live.map(\.isLoose), [true, true], "a request whose tasks wait stands loose in Waiting")
        XCTAssertEqual(live.flatMap(\.tasks).filter { $0.depth == 0 }.map(\.title), ["Link the website in app.json", "Photos extension hint after the first blur"])
        XCTAssertEqual(live.flatMap(\.tasks).filter { $0.depth == 0 }.map(\.state), ["Didn't start", "Paused"])

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
