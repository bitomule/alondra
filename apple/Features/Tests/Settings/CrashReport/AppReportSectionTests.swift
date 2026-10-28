import AlondraContract
import Foundation
import Testing
import AlondraFeatures

/// `apps.jsonl` keeps the code's text of each log line and only the values of a safe shape: the
/// entries below are built as `OSLogStore` gives them, with the private values it does not hide.
@Suite struct AppReportSectionTests {
    let app = AppReportSection.App(client: "ios", version: "0.3.5 (12)", system: "26.1.0",
                                   connection: .unreachable(.deviceRevoked(macName: "Ana’s MacBook Pro")))
    let at = Date(timeIntervalSince1970: 1_792_850_000)

    func entry(_ category: String, _ components: [AppLogEntry.Component], after seconds: TimeInterval = 0) -> AppLogEntry {
        AppLogEntry(date: at.addingTimeInterval(seconds), category: category, level: "notice", components: components)
    }

    func rows(_ text: String) throws -> [[String: Any]] {
        try text.split(separator: "\n").map { try #require(JSONSerialization.jsonObject(with: Data($0.utf8)) as? [String: Any]) }
    }

    @Test func aLineKeepsTheCodesTextAndOnlyTheValuesOfASafeShape() throws {
        let entries = [
            entry("auth", [.init(text: "Refresh failed for ", argument: .string("ana@acme.com")),
                           .init(text: " after ", argument: .number("3")), .init(text: " attempts: ", argument: .string("invalid_grant")),
                           .init(text: "")]),
            entry("auth", [.init(text: "Session restored on ", argument: .string("2026-10-24T05:10:11.637+02:00")),
                           .init(text: " from ", argument: .string("/Users/ana.lopez/Library/Keys")),
                           .init(text: " task ", argument: .string("t-fix-acme-invoice-rounding")),
                           .init(text: " pr ", argument: .string("https://github.com/acme/billing/pull/42")),
                           .init(text: " blob ", argument: .data), .init(text: " state ", argument: .string("Session restored"))], after: 1),
        ]
        let text = AppReportSection.text(app: app, entries: entries, measurements: [])
        for leaked in ["ana@acme.com", "ana.lopez", "acme", "invalid_grant", "Ana’s", "MacBook", "t-fix", "+02:00"] {
            #expect(!text.contains(leaked), "\(leaked) leaked into:\n\(text)")
        }
        let rows = try rows(text)
        #expect(rows[0]["kind"] as? String == "app")
        #expect(rows[0]["version"] as? String == "0.3.5 (12)")
        #expect(rows[0]["connection"] as? String == "unreachable deviceRevoked", "a Mac's name never goes, only the code's word")
        #expect(rows[1]["message"] as? String == "Refresh failed for app-value-1 after 3 attempts: app-value-2")
        #expect(rows[1]["at"] as? String == "2026-10-24T13:53:20.000Z")
        #expect(rows[2]["message"] as? String
            == "Session restored on 2026-10-24T03:10:11.637Z from app-path-1 task app-task-1 pr app-repo-1/pull/42 blob <data> state Session restored",
            "words of the code's own text stay")
    }

    @Test func measurementsGoWholeAndTheOldestLinesAreCutToFit() throws {
        let measurement = #"{"at":"2026-10-24T03:10:11.637Z","route":"irohRelay","ms":212,"ok":true}"#
        let entries = (0..<400).map { entry("feed", [.init(text: "Change ", argument: .number("\($0)")), .init(text: " applied")], after: Double($0)) }
        let text = AppReportSection.text(app: app, entries: entries, measurements: [measurement, "not json"], budget: 8 * 1024)
        #expect(text.utf8.count <= 8 * 1024)
        let rows = try rows(text)
        #expect((rows.first { $0["kind"] as? String == "measurement" }?["measurement"] as? [String: Any])?["route"] as? String == "irohRelay")
        let logs = rows.filter { $0["kind"] as? String == "log" }.compactMap { $0["message"] as? String }
        #expect(logs.last == "Change 399 applied", "the newest lines are kept")
        #expect(!logs.contains("Change 0 applied"))
        let omitted = try #require(rows.last)
        #expect(omitted["kind"] as? String == "omitted")
        #expect(omitted["oldest_log_lines"] as? Int == 400 - logs.count)
        #expect(omitted["measurements_not_json"] as? Int == 1)
    }
}
