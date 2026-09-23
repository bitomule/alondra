import AlondraShared
import SwiftUI

struct MarkdownBlockView: View {
    let block: MarkdownBlock

    var body: some View {
        switch block.kind {
        case .heading(let level):
            Text(MarkdownBlock.inline(block.text))
                .font(headingFont(level))
                .foregroundStyle(Palette.ink)
                .padding(.top, level <= 2 ? 10 : 4)
                .accessibilityAddTraits(.isHeader)
        case .paragraph:
            Text(MarkdownBlock.inline(flowing))
                .font(.body)
                .lineSpacing(3)
        case .listItem(.bullet, let depth):
            listRow(indent: depth) {
                Circle().fill(Palette.dawn).frame(width: 6, height: 6).padding(.top, 8)
            }
        case .listItem(.number(let marker), let depth):
            listRow(indent: depth) {
                Text("\(marker).")
                    .font(.body.monospacedDigit().weight(.semibold))
                    .foregroundStyle(Palette.dawn)
            }
        case .listItem(.task(let done), let depth):
            listRow(indent: depth) {
                Image(systemName: done ? "checkmark.square.fill" : "square")
                    .foregroundStyle(done ? Palette.sage : .secondary)
            }
        case .quote:
            HStack(alignment: .top, spacing: 12) {
                Capsule().fill(Palette.slate.opacity(0.5)).frame(width: 3)
                Text(MarkdownBlock.inline(flowing))
                    .font(.body.italic())
                    .foregroundStyle(.secondary)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .code(let language):
            VStack(alignment: .leading, spacing: 6) {
                if let language {
                    Text(language.uppercased())
                        .font(.caption2.weight(.bold))
                        .kerning(0.8)
                        .foregroundStyle(.tertiary)
                }
                ScrollView(.horizontal, showsIndicators: false) {
                    Text(block.text)
                        .font(.mono(.callout))
                        .fixedSize()
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.night.opacity(0.7), in: .rect(cornerRadius: 12))
        case .table(let header, let rows):
            TableBlock(rows: [header] + rows)
        case .rule:
            Rectangle().fill(Palette.hairline).frame(height: 1).padding(.vertical, 6)
        }
    }

    private var flowing: String {
        block.text.replacingOccurrences(of: "\n", with: " ")
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .display(.title)
        case 2: .display(.title2)
        case 3: .display(.title3)
        default: .headline
        }
    }

    private func listRow<Marker: View>(indent: Int, @ViewBuilder marker: () -> Marker) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            marker()
                .frame(minWidth: 18, alignment: .leading)
            Text(MarkdownBlock.inline(flowing))
                .font(.body)
                .lineSpacing(3)
        }
        .padding(.leading, CGFloat(indent) * 18)
    }
}

private struct TableBlock: View {
    let rows: [[String]]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 8) {
                ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
                            Text(MarkdownBlock.inline(cell))
                                .font(index == 0 ? .subheadline.weight(.semibold) : .subheadline)
                                .frame(maxWidth: 260, alignment: .leading)
                        }
                    }
                    if index == 0 {
                        Divider().gridCellUnsizedAxes(.horizontal)
                    }
                }
            }
            .padding(14)
        }
        .background(Palette.night.opacity(0.5), in: .rect(cornerRadius: 12))
    }
}
