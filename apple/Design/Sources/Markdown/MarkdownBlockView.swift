import SwiftUI

public struct MarkdownSegmentView: View {
    public let segment: MarkdownSegment

    public init(segment: MarkdownSegment) { self.segment = segment }

    public var body: some View {
        switch segment {
        case .block(let block):
            MarkdownBlockView(block: block)
        case .table(let rows):
            TableBlock(rows: rows)
        }
    }
}

public struct MarkdownBlockView: View {
    public let block: MarkdownBlock

    public init(block: MarkdownBlock) { self.block = block }

    public var body: some View {
        content
            .padding(.leading, CGFloat(block.depth) * 18)
    }

    @ViewBuilder
    private var content: some View {
        switch block.kind {
        case .heading(let level):
            Text(block.content)
                .font(headingFont(level))
                .foregroundStyle(Palette.ink)
                .padding(.top, level <= 2 ? 10 : 4)
                .accessibilityAddTraits(.isHeader)
        case .paragraph:
            Text(block.content)
                .font(.body)
                .lineSpacing(3)
        case .listItem(.bullet):
            listRow {
                Circle().fill(Palette.dawn).frame(width: 6, height: 6).padding(.top, 8)
            }
        case .listItem(.number(let marker)):
            listRow {
                Text(verbatim: "\(marker).")
                    .font(.body.monospacedDigit().weight(.semibold))
                    .foregroundStyle(Palette.dawn)
            }
        case .listItem(.task(let done)):
            listRow {
                Image(systemName: done ? "checkmark.square.fill" : "square")
                    .foregroundStyle(done ? Palette.sage : .secondary)
            }
        case .quote:
            HStack(alignment: .top, spacing: 12) {
                Capsule().fill(Palette.slate.opacity(0.5)).frame(width: 3)
                Text(block.content)
                    .font(.body.italic())
                    .foregroundStyle(.secondary)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .code(let language):
            CodeBlock(source: block.plainText, language: language) {
                HighlightedCode(code: block.plainText, language: language)
                    .font(.mono(.callout))
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.night.opacity(0.7), in: .rect(cornerRadius: 12))
        case .tableRow:
            TableBlock(rows: [block])
        case .rule:
            Rectangle().fill(Palette.hairline).frame(height: 1).padding(.vertical, 6)
        }
    }

    private func headingFont(_ level: Int) -> Font {
        switch level {
        case 1: .display(.title)
        case 2: .display(.title2)
        case 3: .display(.title3)
        default: .headline
        }
    }

    private func listRow<Marker: View>(@ViewBuilder marker: () -> Marker) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            marker()
                .frame(minWidth: 18, alignment: .leading)
            Text(block.content)
                .font(.body)
                .lineSpacing(3)
        }
    }
}

public struct TableBlock<Cell: View, Below: View, Card: View>: View {
    public let rows: [MarkdownBlock]
    @ViewBuilder let cell: (MarkdownBlock, TableCell) -> Cell
    @ViewBuilder let below: (MarkdownBlock) -> Below
    @ViewBuilder let card: (MarkdownBlock, TableRowCardView) -> Card

    public init(
        rows: [MarkdownBlock],
        @ViewBuilder cell: @escaping (MarkdownBlock, TableCell) -> Cell,
        @ViewBuilder below: @escaping (MarkdownBlock) -> Below,
        @ViewBuilder card: @escaping (MarkdownBlock, TableRowCardView) -> Card
    ) {
        self.rows = rows
        self.cell = cell
        self.below = below
        self.card = card
    }

    public var body: some View {
        ViewThatFits(in: .horizontal) {
            grid
            cards
        }
        .padding(.leading, CGFloat(rows.first?.depth ?? 0) * 18)
    }

    private var grid: some View {
        Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
            ForEach(rows) { block in
                if let row = block.tableRow {
                    GridRow {
                        ForEach(row.cells.indices, id: \.self) { column in
                            cell(block, TableCell(text: row.cells[column], alignment: row.alignment(of: column), isHeader: row.isHeader))
                        }
                    }
                    if row.isHeader {
                        Divider().gridCellUnsizedAxes(.horizontal)
                    }
                    below(block)
                }
            }
        }
        .padding(6)
        .background(Palette.night.opacity(0.5), in: .rect(cornerRadius: 12))
    }

    private var cards: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(rows) { block in
                if let row = block.tableRow, !row.isHeader || rows.count == 1 {
                    card(block, TableRowCardView(card: TableRowCard(row)))
                }
            }
        }
    }
}

extension TableBlock where Cell == TableCell, Below == EmptyView, Card == TableRowCardView {
    public init(rows: [MarkdownBlock]) {
        self.init(rows: rows, cell: { _, cell in cell }, below: { _ in EmptyView() }, card: { _, card in card })
    }
}

public struct TableCell: View {
    public let text: AttributedString
    public let alignment: MarkdownBlock.ColumnAlignment
    public let isHeader: Bool

    public init(text: AttributedString, alignment: MarkdownBlock.ColumnAlignment, isHeader: Bool) {
        self.text = text
        self.alignment = alignment
        self.isHeader = isHeader
    }

    public var body: some View {
        Text(text)
            .font(isHeader ? .subheadline.weight(.semibold) : .subheadline)
            .multilineTextAlignment(textAlignment)
            .frame(maxWidth: 260, alignment: Alignment(horizontal: horizontalAlignment, vertical: .top))
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
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
}
