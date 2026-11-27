import AlondraContract
import SwiftUI

/// A lasting decision as a sheet: its context, the decision, its consequences and how it is
/// checked, and which decisions it replaces.
public struct DecisionSheetView: View {
    let sheet: DecisionSheet
    @Environment(\.sheetSaving) private var saving
    @Environment(\.dynamicTypeSize) private var typeSize

    public init(sheet: DecisionSheet) { self.sheet = sheet }

    public var body: some View {
        SheetCard(raised: true) {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .top, spacing: 0) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(sheet.title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink)
                            .fixedSize(horizontal: false, vertical: true).accessibilityAddTraits(.isHeader)
                        ChipRow(stacked: typeSize.isAccessibilitySize || sheet.decision.count + sheet.status.count + (sheet.mark?.label.count ?? 0) + (savingState == nil ? 0 : 12) > 20) {
                            Text(sheet.decision).font(.mono(.caption)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                            Label(sheet.status, systemImage: "seal").font(.caption.weight(.semibold)).foregroundStyle(Palette.slate)
                            if let mark = sheet.mark { SheetMarkView(mark: mark) }
                            if let state = savingState { SheetSavingView(saving: state) }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    SheetAffordances(anchor: sheet.anchor, edit: sheet.edit, subject: sheet.title)
                }
                ForEach(Array(sheet.sections.enumerated()), id: \.offset) { _, section in
                    VStack(alignment: .leading, spacing: 4) {
                        Text(section.label).font(.caption.weight(.bold)).foregroundStyle(.secondary).accessibilityAddTraits(.isHeader)
                        Text(section.text).font(.subheadline).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
                    }
                }
                if let supersedes = sheet.supersedes { SheetReferencesView(references: supersedes) }
                if let supersededBy = sheet.supersededBy { SheetReferencesView(references: supersededBy) }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(SheetSpeech.decision(sheet)))
    }

    private var savingState: SheetSaving? {
        sheet.edit.flatMap { link in if case let .view(key) = link { saving[key] } else { nil } }
    }
}
