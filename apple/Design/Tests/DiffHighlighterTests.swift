import AlondraDesign
import Foundation
import Testing

@Suite struct DiffHighlighterTests {
    @Test func theLanguageComesFromTheFileName() {
        #expect(DiffHighlighter.language(for: "crates/core/src/states.rs") == "rs")
        #expect(DiffHighlighter.language(for: "apple/BUILD.bazel") == "python")
        #expect(DiffHighlighter.language(for: "Makefile") == "makefile")
        #expect(DiffHighlighter.language(for: "apple/Design/Resources/AlondraDesign.xcstrings") == "json")
        #expect(DiffHighlighter.language(for: "LICENSE") == nil)
    }

    @Test func colouredTextSplitsBackIntoItsLines() {
        let lines = DiffHighlighter.split(AttributedString("let a = 1\n    let b = 2\n"))
        #expect(lines.map { String($0.characters) } == ["let a = 1", "    let b = 2", ""])
    }

    @Test func theIndentHighlightJSTrimsIsPutBack() {
        let raw = "    return x"
        let restored = DiffHighlighter.restoringIndent(AttributedString("return x"), of: raw)
        #expect(restored.map { String($0.characters) } == raw)
        #expect(DiffHighlighter.restoringIndent(AttributedString("something else"), of: raw) == nil, "a line that does not match stays plain")
    }
}
