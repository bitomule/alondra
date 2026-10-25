import AlondraDesign
import Foundation
import Testing

@Suite struct UnifiedDiffTests {
    static let files: [DiffFile] = {
        let environment = ProcessInfo.processInfo.environment
        let url = environment["TEST_SRCDIR"].map {
            URL(fileURLWithPath: $0).appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
                .appendingPathComponent("fixtures/diffs/pull-request.diff")
        } ?? URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../../../fixtures/diffs/pull-request.diff").standardizedFileURL
        return UnifiedDiff.parse((try? String(contentsOf: url, encoding: .utf8)) ?? "")
    }()

    func file(_ path: String) throws -> DiffFile {
        try #require(Self.files.first { $0.path == path })
    }

    @Test func everyFileKeepsItsStatus() {
        #expect(Self.files.map(\.path) == [
            "Sources/ScheduleEditor.swift", "Sources/CompactRow.swift", "Sources/LongForm.swift",
            "Sources/Launch/Launch.swift", "Sources/Preferences.swift", "Assets/icon.png",
        ])
        #expect(Self.files.map(\.status) == [.modified, .added, .deleted, .renamed, .renamed, .modified])
        #expect(Self.files.map(\.oldPath) == [nil, nil, nil, "Old/Launch.swift", "Sources/Settings.swift", nil])
        #expect(Self.files.map(\.isBinary) == [false, false, false, false, false, true])
    }

    @Test func linesKeepTheirOldAndNewNumbers() throws {
        let editor = try file("Sources/ScheduleEditor.swift")
        #expect(editor.additions == 3)
        #expect(editor.deletions == 3)
        let numbered = editor.lines.map { "\($0.old.map(String.init) ?? "_") \($0.new.map(String.init) ?? "_")" }
        #expect(numbered == ["_ _", "10 10", "11 _", "_ 11", "_ 12", "12 13", "13 14", "14 _", "_ _", "40 _", "_ 40", "41 41"])
    }

    @Test func aRemovedLineThatLooksLikeAHeaderStaysCode() throws {
        let removed = try file("Sources/ScheduleEditor.swift").lines.filter { $0.kind == .removed }.map(\.text)
        #expect(removed.contains("--- old comment"))
    }

    @Test func theHunkHeaderKeepsItsContext() throws {
        let hunks = try file("Sources/ScheduleEditor.swift").lines.filter { $0.kind == .hunk }.map(\.text)
        #expect(hunks == ["@@ -10,5 +10,5 @@ struct ScheduleEditor: View {", "@@ -40,2 +40,2 @@ extension ScheduleEditor {"])
    }

    @Test func binaryAndUnchangedFilesStartFolded() throws {
        #expect(try file("Assets/icon.png").startsCollapsed)
        #expect(try file("Sources/Launch/Launch.swift").startsCollapsed)
        #expect(try !file("Sources/LongForm.swift").startsCollapsed, "a file with only a note after its lines still shows them")
        #expect(try !file("Sources/ScheduleEditor.swift").startsCollapsed)
    }

    @Test func aReplacedLineMarksOnlyTheWordsThatChanged() throws {
        let editor = try file("Sources/ScheduleEditor.swift")
        func changed(_ text: String) -> String? {
            guard let line = editor.lines.first(where: { $0.text == text && $0.kind != .context }), let range = line.changed else { return nil }
            return String(Array(line.text)[range])
        }
        #expect(changed("        Form { longRows }") == "longRows")
        #expect(changed("        Form { compactRow }") == "compactRow")
        #expect(changed("    let title = \"Schedule\"") == "Schedule")
        #expect(changed("    let title = \"Repeat\"") == "Repeat")
        #expect(changed("            .sheet(isPresented: $showsLaunch) { LaunchOptions() }") == nil, "an added line with no removed partner")
    }

    @Test func linesThatShareNothingAreNotMarked() {
        #expect(InlineChanges.changed("    return a", "    print(b)") == nil)
        #expect(InlineChanges.changed("same", "same") == nil)
        let long = String(repeating: "x ", count: InlineChanges.longestComparedLine)
        #expect(InlineChanges.changed(long + "a", long + "b") == nil)
    }
}
