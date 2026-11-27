import AlondraContract
import SwiftUI

/// One spec as a sheet, never as Gherkin: its title and what changed, its explanation and the
/// decisions it rests on, its diagrams and mockups, and each scenario as a card with its steps in
/// lanes. Scenarios a change leaves as they are fold into one line.
public struct SpecSheetView: View {
    let sheet: SpecSheet
    @State private var showsUnchanged = false
    @Environment(\.sheetHandlers) private var handlers
    @Environment(\.sheetSaving) private var saving
    @Environment(\.dynamicTypeSize) private var typeSize

    public init(sheet: SpecSheet) { self.sheet = sheet }

    public var body: some View {
        SheetCard(raised: true) {
            VStack(alignment: .leading, spacing: 14) {
                header
                if !sheet.explanation.isEmpty {
                    Text(sheet.explanation).font(.subheadline).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
                }
                if let decisions = sheet.decisions { SheetReferencesView(references: decisions) }
                ForEach(sheet.parts) { SheetPartView(part: $0) }
                if let background = sheet.background { SheetBackgroundView(background: background) }
                ForEach(sheet.scenarios) { SheetScenarioView(scenario: $0) }
                if let unchanged = sheet.unchanged { folded(unchanged) }
                if let empty = sheet.empty { Text(empty).font(.subheadline).foregroundStyle(.secondary) }
                if !sheet.add.isEmpty { SheetAddRow(references: sheet.add) }
                if !sheet.actions.isEmpty { actions }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(SheetSpeech.spec(sheet)))
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                Text(sheet.title).font(.display(.title3, weight: .bold)).foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true).accessibilityAddTraits(.isHeader)
                ChipRow(stacked: typeSize.isAccessibilitySize || sheet.spec.count + (sheet.mark?.label.count ?? 0) + (savingState == nil ? 0 : 12) > 28) {
                    Text(sheet.spec).font(.mono(.caption)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    if let mark = sheet.mark { SheetMarkView(mark: mark) }
                    if let state = savingState { SheetSavingView(saving: state) }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            SheetAffordances(anchor: sheet.anchor, edit: sheet.edit, subject: sheet.title)
        }
    }

    private var savingState: SheetSaving? {
        sheet.edit.flatMap { link in if case let .view(key) = link { saving[key] } else { nil } }
    }

    private func folded(_ unchanged: SheetUnchanged) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Button { withAnimation(.snappy) { showsUnchanged.toggle() } } label: {
                HStack(spacing: 8) {
                    Image(systemName: showsUnchanged ? "chevron.down" : "chevron.right").font(.footnote.weight(.bold)).accessibilityHidden(true)
                    Text(unchanged.label).font(.subheadline).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 0)
                }
                .frame(minHeight: 44).contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityAddTraits(.isButton)
            .accessibilityValue(showsUnchanged ? Text("Unfolded", tableName: "AlondraDesign", bundle: #bundle) : Text("Folded", tableName: "AlondraDesign", bundle: #bundle))
            if showsUnchanged { ForEach(unchanged.scenarios) { SheetScenarioView(scenario: $0) } }
        }
    }

    private var actions: some View {
        FlowRow {
            ForEach(Array(sheet.actions.enumerated()), id: \.offset) { _, action in
                Button(action.label) { handlers.onAction(action) }
                    .buttonStyle(.bordered).tint(Palette.dawn).disabled(!action.enabled)
            }
        }
    }
}

struct SheetReferencesView: View {
    let references: SheetReferences
    @Environment(\.sheetHandlers) private var handlers

    var body: some View {
        FlowRow {
            Text(references.label).font(.footnote.weight(.semibold)).foregroundStyle(.secondary)
            ForEach(Array(references.items.enumerated()), id: \.offset) { _, item in
                if let link = item.link {
                    Button { handlers.onLink(link) } label: {
                        Label(item.title, systemImage: "arrow.turn.down.right").font(.footnote.weight(.semibold))
                            .padding(.horizontal, 10).frame(minHeight: 44).background(Palette.sky.opacity(0.12), in: .capsule)
                    }
                    .buttonStyle(.plain).foregroundStyle(Palette.sky)
                } else {
                    Text(item.title).font(.footnote.weight(.semibold)).foregroundStyle(Palette.ink)
                }
            }
        }
    }
}

struct SheetBackgroundView: View {
    let background: SheetBackground

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(background.label).font(.caption.weight(.bold)).foregroundStyle(.secondary).accessibilityAddTraits(.isHeader)
            SheetLanesView(lanes: background.lanes)
        }
        .padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.night.opacity(0.55), in: .rect(cornerRadius: 12))
    }
}
