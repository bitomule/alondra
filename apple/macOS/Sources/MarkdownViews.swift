import AlondraShared
import SwiftUI

struct MarkdownText: View {
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(MarkdownParser.segments(from: text)) { segment in
                switch segment {
                case .block(let block):
                    BlockView(block: block)
                case .table(let rows):
                    TableGrid(rows: rows)
                }
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
            .padding(.leading, CGFloat(block.depth) * 20)
    }

    @ViewBuilder
    private var content: some View {
        switch block.kind {
        case .heading(let level):
            Text(block.content)
                .font(headingFont(level))
                .padding(.top, level <= 2 ? 12 : 6)
                .padding(.bottom, 2)
                .accessibilityAddTraits(.isHeader)
        case .paragraph:
            Text(block.content)
                .lineSpacing(3)
                .padding(.vertical, 2)
        case .listItem(let marker):
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                ListMarkerView(marker: marker)
                    .frame(minWidth: 16, alignment: .trailing)
                Text(block.content)
                    .lineSpacing(3)
            }
        case .code(let language):
            ScrollView(.horizontal) {
                Text(block.plainText)
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
                Text(block.content)
                    .foregroundStyle(.secondary)
                    .lineSpacing(3)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .tableRow:
            TableGrid(rows: [block])
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

struct TableGrid<Cell: View, Below: View>: View {
    let rows: [MarkdownBlock]
    @ViewBuilder let cell: (MarkdownBlock, Int, TableCell) -> Cell
    @ViewBuilder let below: (MarkdownBlock) -> Below

    var body: some View {
        ScrollView(.horizontal) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                ForEach(rows) { block in
                    if let row = block.tableRow {
                        GridRow {
                            ForEach(row.cells.indices, id: \.self) { column in
                                cell(block, column, TableCell(text: row.cells[column], alignment: row.alignment(of: column), isHeader: row.isHeader))
                            }
                        }
                        if row.isHeader {
                            Divider().gridCellUnsizedAxes(.horizontal)
                        }
                        below(block)
                    }
                }
            }
            .font(.callout)
            .padding(.vertical, 4)
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        .textSelection(.enabled)
        .padding(.horizontal, -9)
        .padding(.leading, CGFloat(rows.first?.depth ?? 0) * 20)
    }
}

extension TableGrid where Cell == TableCell, Below == EmptyView {
    init(rows: [MarkdownBlock]) {
        self.init(rows: rows, cell: { _, _, cell in cell }, below: { _ in EmptyView() })
    }
}

struct TableCell: View {
    let text: AttributedString
    let alignment: MarkdownBlock.ColumnAlignment
    let isHeader: Bool

    var body: some View {
        Text(text)
            .fontWeight(isHeader ? .semibold : nil)
            .multilineTextAlignment(textAlignment)
            .frame(maxWidth: 320, alignment: frameAlignment)
            .padding(.horizontal, 9)
            .padding(.vertical, 3)
            .gridColumnAlignment(horizontalAlignment)
    }

    private var textAlignment: TextAlignment {
        switch alignment {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }

    private var horizontalAlignment: HorizontalAlignment {
        switch alignment {
        case .leading: .leading
        case .center: .center
        case .trailing: .trailing
        }
    }

    private var frameAlignment: Alignment {
        Alignment(horizontal: horizontalAlignment, vertical: .top)
    }
}

struct ListMarkerView: View {
    let marker: MarkdownBlock.ListMarker

    var body: some View {
        switch marker {
        case .bullet:
            Text(verbatim: "•").foregroundStyle(.secondary)
        case .number(let n):
            Text(verbatim: "\(n).").monospacedDigit().foregroundStyle(.secondary)
        case .task(let done):
            Image(systemName: done ? "checkmark.square.fill" : "square")
                .foregroundStyle(done ? Color.accentColor : .secondary)
                .accessibilityLabel(done ? String(localized: "Done") : String(localized: "Pending"))
        }
    }
}
