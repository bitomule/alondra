import AlondraContract
import SwiftUI

/// The steps of a scenario in their lanes. The lane's name comes in the viewer's language from the
/// daemon; the colour only repeats what the word already says.
struct SheetLanesView: View {
    let lanes: [SheetLane]
    @Environment(\.dynamicTypeSize) private var typeSize
    @ScaledMetric(relativeTo: .caption) private var labelWidth: CGFloat = 96

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(lanes.enumerated()), id: \.offset) { _, lane in
                if typeSize.isAccessibilitySize {
                    VStack(alignment: .leading, spacing: 4) { label(lane); steps(lane) }
                } else {
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        label(lane).frame(width: labelWidth, alignment: .leading)
                        steps(lane)
                    }
                }
            }
        }
    }

    private func label(_ lane: SheetLane) -> some View {
        Text(lane.label).font(.caption.weight(.bold)).foregroundStyle(SheetStyle.color(lane.kind))
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityAddTraits(.isHeader)
    }

    private func steps(_ lane: SheetLane) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(lane.steps.enumerated()), id: \.offset) { _, step in SheetStepView(step: step) }
        }
        .padding(.leading, 10)
        .overlay(alignment: .leading) { Rectangle().fill(SheetStyle.color(lane.kind).opacity(0.5)).frame(width: 2) }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(lane.label))
    }
}

struct SheetStepView: View {
    let step: SheetStep

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(step.text).font(.subheadline).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
            if !step.table.isEmpty { SheetTableView(rows: step.table) }
            if let doc = step.doc {
                Text(doc).font(.mono(.footnote)).foregroundStyle(Palette.ink).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                    .padding(10).frame(maxWidth: .infinity, alignment: .leading)
                    .background(Palette.night.opacity(0.7), in: .rect(cornerRadius: 8))
            }
        }
    }
}

/// A table as a table: its first row is the header, and a table wider than the screen scrolls.
struct SheetTableView: View {
    let rows: [[String]]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 0) {
                ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                    GridRow {
                        ForEach(Array(row.enumerated()), id: \.offset) { _, cell in
                            Text(cell).font(.mono(.footnote).weight(index == 0 ? .bold : .regular)).foregroundStyle(Palette.ink)
                                .padding(.vertical, 5)
                        }
                    }
                    .accessibilityElement(children: .combine)
                    if index < rows.count - 1 { Divider().gridCellUnsizedAxes(.horizontal) }
                }
            }
            .padding(.horizontal, 10).padding(.vertical, 4)
        }
        .background(Palette.night.opacity(0.7), in: .rect(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Palette.hairline))
    }
}
