import AlondraContract
import SwiftUI

/// A scenario as a card that folds: its title, what kind of check it is and what changed, then its
/// steps in lanes, its examples, its own diagrams and mockups and, in a delivery, how it was validated.
/// Where the lanes and the parts have room they sit side by side.
struct SheetScenarioView: View {
    let scenario: SheetScenario
    @State private var expanded: Bool
    @Environment(\.sheetHandlers) private var handlers
    @Environment(\.sheetSaving) private var saving
    @Environment(\.dynamicTypeSize) private var typeSize

    init(scenario: SheetScenario) {
        self.scenario = scenario
        _expanded = State(initialValue: !scenario.collapsed)
    }

    var body: some View {
        SheetCard {
            VStack(alignment: .leading, spacing: 12) {
                header
                if expanded { content }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(SheetSpeech.scenario(scenario)))
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 0) {
            if typeSize.isAccessibilitySize {
                HStack(spacing: 0) {
                    foldButton
                    Spacer(minLength: 0)
                    SheetAffordances(anchor: scenario.anchor, edit: scenario.edit, subject: scenario.title).fixedSize()
                }
                title
            } else {
                HStack(alignment: .top, spacing: 0) {
                    foldButton
                    title.layoutPriority(1)
                    SheetAffordances(anchor: scenario.anchor, edit: scenario.edit, subject: scenario.title).fixedSize()
                }
            }
            ChipRow(stacked: stacksChips) {
                Label(scenario.kind, systemImage: scenario.kind.isJudgment ? "eye" : "checkmark.seal")
                    .font(.caption.weight(.semibold)).foregroundStyle(Palette.slate).fixedSize(horizontal: false, vertical: true)
                if let mark = scenario.mark { SheetMarkView(mark: mark) }
                if let state = savingState { SheetSavingView(saving: state) }
            }
            .padding(.leading, typeSize.isAccessibilitySize ? 0 : 32)
            .padding(.top, 4)
        }
    }

    private var foldButton: some View {
        Button { expanded.toggle() } label: {
            Image(systemName: expanded ? "chevron.down" : "chevron.right").font(.footnote.weight(.bold))
                .foregroundStyle(.secondary).frame(minWidth: 32, minHeight: 44)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(expanded ? Text("Fold", tableName: "AlondraDesign", bundle: #bundle) : Text("Unfold", tableName: "AlondraDesign", bundle: #bundle))
    }

    private var title: some View {
        Text(scenario.title).font(.headline).foregroundStyle(Palette.ink).multilineTextAlignment(.leading)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, typeSize.isAccessibilitySize ? 6 : 10)
            .contentShape(.rect)
            .onTapGesture(perform: titleTapped)
            .accessibilityAddTraits(.isButton)
    }

    private var stacksChips: Bool {
        typeSize.isAccessibilitySize || scenario.kind.count + (scenario.mark?.label.count ?? 0) + (savingState == nil ? 0 : 12) > 16
    }

    private var savingState: SheetSaving? {
        scenario.edit.flatMap { link in if case let .view(key) = link { saving[key] } else { nil } }
    }

    private func titleTapped() {
        if let edit = scenario.edit { handlers.onLink(edit) } else { expanded.toggle() }
    }

    @ViewBuilder private var content: some View {
        let body = VStack(alignment: .leading, spacing: 12) {
            SheetLanesView(lanes: scenario.lanes)
            if let examples = scenario.examples {
                VStack(alignment: .leading, spacing: 6) {
                    Text(examples.label).font(.caption.weight(.bold)).foregroundStyle(.secondary)
                    SheetTableView(rows: examples.rows)
                }
            }
        }
        if scenario.parts.isEmpty {
            body
        } else {
            SideBySideLayout {
                body
                parts
            }
        }
        if let result = scenario.result { SheetResultView(result: result) }
        if !scenario.add.isEmpty { SheetAddRow(references: scenario.add) }
    }

    private var parts: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(scenario.parts) { SheetPartView(part: $0) }
        }
    }
}

extension String {
    /// The kind a scenario reads as: «judgment on captures», «juicio sobre capturas».
    var isJudgment: Bool { lowercased().contains("judg") || lowercased().contains("juicio") }
}

/// The links that add a diagram or a mockup, or a scenario, to what the sheet shows.
struct SheetAddRow: View {
    let references: [SheetReference]
    @Environment(\.sheetHandlers) private var handlers

    var body: some View {
        FlowRow {
            ForEach(Array(references.enumerated()), id: \.offset) { _, reference in
                Button { reference.link.map(handlers.onLink) } label: {
                    Label(reference.title, systemImage: "plus").font(.footnote.weight(.semibold))
                        .padding(.horizontal, 12).frame(minHeight: 44)
                        .background(Palette.dawn.opacity(0.12), in: .capsule)
                }
                .buttonStyle(.plain).foregroundStyle(Palette.dawn)
                .disabled(reference.link == nil)
            }
        }
    }
}
