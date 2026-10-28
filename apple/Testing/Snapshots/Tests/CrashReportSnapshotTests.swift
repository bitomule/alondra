import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import XCTest

/// Settings › Send a crash report on the iPhone: «What happened?», the preview with the one
/// decision, a report that cannot leave the Mac, and a Mac that does not answer.
@MainActor
final class CrashReportSnapshotTests: XCTestCase {
    static let report = CrashReport(
        id: "r-20261027-0000abcd", compressedBytes: 6_144, repo: "bitomule/alondra-issues", passed: true,
        files: [.init(name: "manifest.json", bytes: nil), .init(name: "description.txt", bytes: 31),
                .init(name: "versions.json", bytes: 412), .init(name: "machine.json", bytes: 1_204),
                .init(name: "daemon.log", bytes: 48_310), .init(name: "lifecycle.jsonl", bytes: 9_822),
                .init(name: "tasks.jsonl", bytes: 3_115), .init(name: "nodes.jsonl", bytes: nil, empty: "no nodes in the window"),
                .init(name: "apps.jsonl", bytes: 7_480)],
        leftOut: [.omitted("daemon_log_lines_without_template", count: 12)])

    private func screen(_ state: CrashReportFeature.State) -> some View {
        let store = Store(initialState: state) { EmptyReducer<CrashReportFeature.State, CrashReportFeature.Action>() }
        return NavigationStack { CrashReportScreen(store: store) }
    }

    private var preview: CrashReportFeature.State {
        CrashReportFeature.State(whatHappened: "The node stopped after the Mac slept", stage: .ready(Self.report))
    }

    func test_crashReport_whatHappened() {
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 560) { screen(.init()) } }
    }

    func test_crashReport_whatHappened_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 560) { screen(.init()) } }
    }

    func test_crashReport_preview() {
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 1_480) { screen(preview) } }
    }

    func test_crashReport_preview_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 1_480) { screen(preview) } }
    }

    func test_crashReport_withoutRepository() {
        var report = Self.report
        report.repo = nil
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 1_320) { screen(.init(stage: .ready(report))) } }
    }

    func test_crashReport_somethingPrivateFound() {
        var report = Self.report
        report.passed = false
        report.hits = [.init(section: "apps.jsonl", line: 14, what: "an email address")]
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 1_320) { screen(.init(stage: .ready(report))) } }
    }

    func test_crashReport_macNotAnswering() {
        assertScreen([.light, .dark], inWindow: true) {
            ScreenFrame(height: 360) { screen(.init(stage: .failed(.notAnswering(onThisMac: false), report: nil))) }
        }
    }

    func test_crashReport_macNotAnswering_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) {
            ScreenFrame(height: 360) { screen(.init(stage: .failed(.notAnswering(onThisMac: false), report: nil))) }
        }
    }

    func test_crashReport_sent() {
        let sent = CrashReportSent(state: .sent, url: URL(string: "https://github.com/bitomule/alondra-issues/issues/12"),
                                   message: "Sent: https://github.com/bitomule/alondra-issues/issues/12. Its issue in bitomule/alondra-issues shows the version, macOS and the kind of failure, and links the report: anyone who sees it can read the report.")
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 420) { screen(.init(stage: .sent(sent))) } }
    }
}
