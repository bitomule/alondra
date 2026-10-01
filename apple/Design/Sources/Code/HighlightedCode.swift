import SwiftUI

/// A code block's text: plain first, coloured once the highlighter has it.
public struct HighlightedCode: View {
    let code: String
    let language: String?

    @Environment(\.colorScheme) private var scheme
    @State private var coloured: (key: CodeColourKey, text: AttributedString)?

    public init(code: String, language: String?) {
        self.code = code
        self.language = language
    }

    public var body: some View {
        let key = CodeColourKey(code: code, fence: language, dark: scheme == .dark)
        Text(text(for: key))
            .task(id: key) {
                guard let key, CodeHighlighter.cached(key) == nil else { return }
                coloured = (key, await CodeHighlighter.colour(key))
            }
    }

    private func text(for key: CodeColourKey?) -> AttributedString {
        guard let key else { return AttributedString(code) }
        if let coloured, coloured.key == key { return coloured.text }
        return CodeHighlighter.cached(key) ?? AttributedString(code)
    }
}
