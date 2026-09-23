import AlondraShared
import SwiftUI

func inlineMarkdown(_ text: String) -> AttributedString {
    let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
    return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
}

struct MarkdownText: View {
    let text: String

    var body: some View {
        let blocks = MarkdownParser.blocks(from: text)
        VStack(alignment: .leading, spacing: 8) {
            ForEach(blocks) { block in
                BlockView(block: block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct BlockView: View {
    let block: MarkdownBlock

    var body: some View {
        content
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private var content: some View {
        switch block.kind {
        case .heading(let level):
            Text(inlineMarkdown(block.text))
                .font(headingFont(level))
                .padding(.top, level <= 2 ? 12 : 6)
                .padding(.bottom, 2)
                .accessibilityAddTraits(.isHeader)
        case .paragraph:
            Text(inlineMarkdown(block.text))
                .lineSpacing(3)
                .padding(.vertical, 2)
        case .listItem(let marker, let depth):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                ListMarkerView(marker: marker)
                    .frame(minWidth: 16, alignment: .trailing)
                Text(inlineMarkdown(block.text))
                    .lineSpacing(3)
            }
            .padding(.leading, CGFloat(depth) * 20)
        case .code(let language):
            ScrollView(.horizontal) {
                Text(block.text)
                    .font(.system(.callout, design: .monospaced))
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(12)
            }
            .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.primary.opacity(0.05), in: .rect(cornerRadius: 8))
            .overlay(alignment: .topTrailing) {
                if let language {
                    Text(language)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.tertiary)
                        .padding(6)
                }
            }
            .padding(.vertical, 4)
        case .quote:
            HStack(alignment: .top, spacing: 10) {
                Capsule().fill(.quaternary).frame(width: 3)
                Text(inlineMarkdown(block.text))
                    .foregroundStyle(.secondary)
                    .lineSpacing(3)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .table(let header, let rows):
            ScrollView(.horizontal) {
                Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 6) {
                    GridRow {
                        ForEach(Array(header.enumerated()), id: \.offset) { _, cell in
                            Text(inlineMarkdown(cell)).fontWeight(.semibold)
                        }
                    }
                    Divider()
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        GridRow {
                            ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
                                Text(inlineMarkdown(cell))
                            }
                        }
                    }
                }
                .font(.callout)
                .padding(.vertical, 4)
            }
            .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        case .rule:
            Divider().padding(.vertical, 8)
        }
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .title2.weight(.bold)
        case 2: .title3.weight(.semibold)
        case 3: .headline
        default: .subheadline.weight(.semibold)
        }
    }
}

struct ListMarkerView: View {
    let marker: MarkdownBlock.ListMarker

    var body: some View {
        switch marker {
        case .bullet:
            Text("•").foregroundStyle(.secondary)
        case .number(let n):
            Text("\(n).").monospacedDigit().foregroundStyle(.secondary)
        case .task(let done):
            Image(systemName: done ? "checkmark.square.fill" : "square")
                .foregroundStyle(done ? Color.accentColor : .secondary)
                .accessibilityLabel(done ? "Hecho" : "Pendiente")
        }
    }
}
