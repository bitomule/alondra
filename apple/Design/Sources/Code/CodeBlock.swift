import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

/// A fenced block of a card's text: its language, the code scrolling sideways, selectable, and a
/// button that copies all of it, so a command can go straight to a terminal.
public struct CodeBlock<Code: View>: View {
    let source: String
    let language: String?
    @ViewBuilder let code: () -> Code
    @State private var copied = false

    public init(source: String, language: String?, @ViewBuilder code: @escaping () -> Code) {
        self.source = source
        self.language = language
        self.code = code
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                if let language {
                    Text(verbatim: language.uppercased())
                        .font(.caption2.weight(.bold))
                        .kerning(0.8)
                        .foregroundStyle(.tertiary)
                }
                Spacer(minLength: 8)
                Button(action: copy) {
                    Group {
                        if copied {
                            Label("Copied", systemImage: "checkmark")
                        } else {
                            Label("Copy", systemImage: "doc.on.doc")
                        }
                    }
                    .font(.caption.weight(.semibold))
                }
                .buttonStyle(.borderless)
                .accessibilityHint(Text("Copies the whole block"))
            }
            ScrollView(.horizontal, showsIndicators: false) {
                code()
                    .fixedSize()
                    .textSelection(.enabled)
            }
        }
    }

    private func copy() {
        #if canImport(UIKit)
        UIPasteboard.general.string = source
        #elseif canImport(AppKit)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(source, forType: .string)
        #endif
        withAnimation(.snappy) { copied = true }
        Task {
            try? await Task.sleep(for: .seconds(2))
            withAnimation(.snappy) { copied = false }
        }
    }
}
