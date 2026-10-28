import AlondraContract
import AlondraFeatures
import AlondraTransport
import ComposableArchitecture
import XCTest

@MainActor
final class CrashReportFeatureTests: XCTestCase {
    nonisolated static let reply = #"""
    {"schema":"alondra.report/1","id":"r-20261027-0000abcd","dir":"/Users/ana/.alondra/reports/r-20261027-0000abcd",
     "compressed_bytes":6144,"repo":"bitomule/alondra-issues","failure":"other",
     "manifest":{"window":{"hours":72},"description":{"bytes":31,"sha256":"y"},
       "sections":[{"name":"daemon.log","bytes":4096,"sha256":"a"},{"name":"nodes.jsonl","empty":"no nodes in the window"},
                   {"name":"apps.jsonl","bytes":900,"sha256":"b"}],
       "omitted":{"daemon_log_lines_without_template":12,"lifecycle_events_not_decoded":0},
       "trimmed":[{"section":"daemon.log","lines":40}]},
     "check":{"passed":true,"hits":[]},"summary":"…"}
    """#

    nonisolated static var report: CrashReport { CrashReport(daemonReply: Data(reply.utf8))! }

    func testTheDaemonsAnswerReadsAsTheFilesThatWouldLeaveTheMac() throws {
        let report = Self.report
        XCTAssertEqual(report.id, "r-20261027-0000abcd")
        XCTAssertEqual(report.files.map(\.name), ["manifest.json", "description.txt", "daemon.log", "nodes.jsonl", "apps.jsonl"])
        XCTAssertEqual(report.files.filter(\.openable).map(\.name), ["manifest.json", "description.txt", "daemon.log", "apps.jsonl"])
        XCTAssertEqual(report.files[3].empty, "no nodes in the window")
        XCTAssertEqual(report.leftOut, [.omitted("daemon_log_lines_without_template", count: 12), .trimmed(section: "daemon.log", lines: 40)])
        XCTAssertTrue(report.sendable)
        let unsafeRepo = Self.reply.replacingOccurrences(of: "bitomule/alondra-issues", with: "not a repo")
        XCTAssertNil(CrashReport(daemonReply: Data(unsafeRepo.utf8))?.repo)
        XCTAssertNil(CrashReport(daemonReply: Data(#"{"id":"../x"}"#.utf8)))
    }

    private func store(_ client: CrashReportClient) -> TestStoreOf<CrashReportFeature> {
        TestStore(initialState: CrashReportFeature.State()) { CrashReportFeature() } withDependencies: { $0.crashReports = client }
    }

    func testWhatTheUserWroteAndTheAppsSectionGoToTheMacAndNothingIsSentUntilYes() async throws {
        let built = LockIsolated<[String?]>([])
        let sent = LockIsolated<[String]>([])
        let client = CrashReportClient(
            build: { description, failure, section in
                built.withValue { $0 = [description, failure, section] }
                return Self.report
            },
            file: { id, name in "\(id)/\(name)" },
            send: { id, failure in
                sent.withValue { $0 = [id, failure] }
                return CrashReportSent(state: .sent, url: URL(string: "https://github.com/bitomule/alondra-issues/issues/1"), message: "Sent")
            },
            appSection: { #"{"kind":"app"}"# + "\n" })
        let store = store(client)
        await store.send(.describe("  It stopped after the Mac slept  ")) { $0.whatHappened = "  It stopped after the Mac slept  " }
        await store.send(.build) { $0.stage = .building }
        await store.receive(\.built) { $0.stage = .ready(Self.report) }
        XCTAssertEqual(built.value, ["It stopped after the Mac slept", "other", #"{"kind":"app"}"# + "\n"], "Settings sends «other»")
        await store.send(.describe("changed"))
        await store.send(.open("nodes.jsonl"))
        await store.send(.open("daemon.log")) { $0.viewing = .init(name: "daemon.log") }
        await store.receive(\.fileLoaded) { $0.viewing?.text = "r-20261027-0000abcd/daemon.log" }
        await store.send(.closeFile) { $0.viewing = nil }
        XCTAssertEqual(sent.value, [], "nothing leaves before the user says yes")
        await store.send(.send) { $0.stage = .sending(Self.report) }
        await store.receive(\.sent) {
            $0.stage = .sent(CrashReportSent(state: .sent, url: URL(string: "https://github.com/bitomule/alondra-issues/issues/1"), message: "Sent"))
        }
        XCTAssertEqual(sent.value, ["r-20261027-0000abcd", "other"])
    }

    func testNoKeepsItOnTheMacAndAReportWithoutARepositoryOrWithSomethingPrivateIsNeverSent() async {
        var noRepo = Self.report
        noRepo.repo = nil
        var blocked = Self.report
        blocked.passed = false
        let client = CrashReportClient(build: { _, _, _ in Self.report }, file: { _, _ in "" },
                                       send: { _, _ in XCTFail("never sent"); throw CancellationError() })
        for report in [noRepo, blocked] {
            let store = TestStore(initialState: CrashReportFeature.State(stage: .ready(report))) { CrashReportFeature() } withDependencies: {
                $0.crashReports = client
            }
            await store.send(.send)
        }
        let store = TestStore(initialState: CrashReportFeature.State(stage: .ready(Self.report))) { CrashReportFeature() } withDependencies: {
            $0.crashReports = client
        }
        await store.send(.keep) { $0.stage = .kept("r-20261027-0000abcd") }
    }

    func testAMacThatDoesNotAnswerIsToldWhereToRunTheReport() async {
        for onThisMac in [true, false] {
            let client = CrashReportClient(build: { _, _, _ in throw OperationError.outcomeUnknown }, file: { _, _ in "" },
                                           send: { _, _ in throw CancellationError() },
                                           problem: { _ in .macNotAnswering }, onThisMac: onThisMac)
            let store = store(client)
            await store.send(.build) { $0.stage = .building }
            await store.receive(\.failed) { $0.stage = .failed(.notAnswering(onThisMac: onThisMac), report: nil) }
            await store.send(.build) { $0.stage = .building }
            await store.receive(\.failed) { $0.stage = .failed(.notAnswering(onThisMac: onThisMac), report: nil) }
        }
        XCTAssertEqual(CrashReportFeature.message(.notAnswering(onThisMac: true)),
                       "Alondra’s service is not answering: open Terminal and run alondra report.")
        XCTAssertEqual(CrashReportFeature.message(.notAnswering(onThisMac: false)), "Your Mac is not answering: run alondra report on it.")
    }

    func testTheMacsOwnReasonIsShownWhenItRefusesAndTheReportStaysThere() async {
        let client = CrashReportClient(build: { _, _, _ in Self.report }, file: { _, _ in "" },
                                       send: { _, _ in throw APIError.conflict(kind: nil, detail: "GitHub said no", reasons: []) },
                                       problem: { _ in .macNotAnswering })
        let store = TestStore(initialState: CrashReportFeature.State(stage: .ready(Self.report))) { CrashReportFeature() } withDependencies: {
            $0.crashReports = client
        }
        await store.send(.send) { $0.stage = .sending(Self.report) }
        await store.receive(\.failed) { $0.stage = .failed(.refused("GitHub said no"), report: "r-20261027-0000abcd") }
        await store.send(.build)
        await store.send(.startOver) { $0 = CrashReportFeature.State() }
    }
}
