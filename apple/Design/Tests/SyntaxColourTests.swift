import AlondraDesign
import Foundation
import Testing

@Suite struct SyntaxColourTests {
    @Test func theIndentHighlightJSTrimsIsPutBack() {
        let raw = "    return x"
        let restored = SyntaxColour.restoringIndent(AttributedString("return x"), of: raw)
        #expect(restored.map { String($0.characters) } == raw)
        #expect(SyntaxColour.restoringIndent(AttributedString("something else"), of: raw) == nil, "a line that does not match stays plain")
    }
}
