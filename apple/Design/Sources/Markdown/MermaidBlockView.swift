import AlondraMermaid
import Foundation
import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

/// A ` ```mermaid ` block drawn as a diagram. Until it is laid out, and whenever it cannot be drawn,
/// it shows the code block it has always been, the latter with a notice saying why.
public struct MermaidBlockView: View {
    public static func handles(language: String?) -> Bool {
        language?.lowercased() == "mermaid"
    }

    let source: String
    @ScaledMetric(relativeTo: .subheadline) private var textSize: CGFloat = 14
    @State private var finished: (key: MermaidRenderCache.Key, rendering: MermaidRendering)?
    @State private var showsCode = false
    @State private var width: CGFloat?

    public init(source: String) { self.source = source }

    public var body: some View {
        let key = MermaidRenderCache.Key(source: source, size: textSize.rounded())
        let rendering = finished?.key == key ? finished?.rendering : MermaidRenderCache.shared.cached(key)
        Group {
            switch rendering {
            case .diagram(let layout, let summary) where !showsCode:
                diagram(layout, summary: summary, size: key.size)
            case .diagram:
                code(header: AnyView(toggle))
            case .notice(let notice):
                code(header: AnyView(NoticeLine(phrase: notice.phrase)))
            case nil:
                code(header: AnyView(EmptyView()))
            }
        }
        .task(id: key) {
            if finished?.key == key { return }
            let rendering = await MermaidRenderCache.shared.rendering(key)
            finished = (key, rendering)
        }
    }

    private func diagram(_ layout: MermaidLayout, summary: MermaidSummary, size: CGFloat) -> some View {
        let words = MermaidWords.app
        let speech = MermaidSpeech(summary, words: words)
        let scale = MermaidScale.fitting(layout.size.width, in: width, textSize: size)
        let overflows = width.map { layout.size.width * scale > $0 } ?? false
        return VStack(alignment: .leading, spacing: 6) {
            HStack {
                Spacer(minLength: 8)
                toggle
            }
            ScrollView(.horizontal) {
                MermaidCanvas(layout: layout, theme: .alondra(size: size))
                    .scaleEffect(scale, anchor: .topLeading)
                    .frame(width: layout.size.width * scale, height: layout.size.height * scale, alignment: .topLeading)
            }
            .scrollDisabled(!overflows)
            .onGeometryChange(for: CGFloat.self) { $0.size.width } action: { width = $0 }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(verbatim: speech.label))
            .accessibilityValue(Text(verbatim: speech.value))
            .accessibilityAction(named: Text(verbatim: words(.showCode))) { showsCode = true }
            .accessibilityAction(named: Text(verbatim: words(.copyCode))) { copy() }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.card, in: .rect(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Palette.hairline))
        .contextMenu {
            Button { showsCode = true } label: { Label(words(.showCode), systemImage: "chevron.left.forwardslash.chevron.right") }
            Button(action: copy) { Label(words(.copyCode), systemImage: "doc.on.doc") }
        }
    }

    private var toggle: some View {
        Button { showsCode.toggle() } label: {
            Group {
                if showsCode {
                    Label(MermaidWords.app(.diagram), systemImage: "point.3.connected.trianglepath.dotted")
                } else {
                    Label(MermaidWords.app(.code), systemImage: "chevron.left.forwardslash.chevron.right")
                }
            }
            .font(.caption.weight(.semibold))
        }
        .buttonStyle(.borderless)
    }

    private func code(header: AnyView) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            CodeBlock(source: source, language: "mermaid") {
                HighlightedCode(code: source, language: "mermaid")
                    .font(.mono(.callout))
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.night.opacity(0.7), in: .rect(cornerRadius: 12))
    }

    private func copy() {
        #if canImport(UIKit)
        UIPasteboard.general.string = source
        #elseif canImport(AppKit)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(source, forType: .string)
        #endif
    }
}

private struct NoticeLine: View {
    let phrase: MermaidPhrase

    var body: some View {
        Label(MermaidWords.app(phrase), systemImage: "exclamationmark.triangle")
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}
