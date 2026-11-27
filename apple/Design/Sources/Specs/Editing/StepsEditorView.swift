import SwiftUI

/// The steps of a scenario as rows: a lane, the words and, when the step has one, a table. Rows
/// are added, removed and reordered with buttons a screen reader can reach. Text that is not made
/// of steps stays editable as text.
public struct StepsEditorView: View {
    @Binding var text: String
    let placeholder: String
    @State private var draft: StepsDraft?
    @State private var edited: String

    public init(text: Binding<String>, placeholder: String = "") {
        _text = text
        self.placeholder = placeholder
        _draft = State(initialValue: StepsDraft(parsing: text.wrappedValue))
        _edited = State(initialValue: text.wrappedValue)
    }

    public var body: some View {
        Group {
            if draft != nil { rows } else { fallback }
        }
        .onChange(of: text) { _, new in
            guard new != draft?.text else { return }
            draft = StepsDraft(parsing: new)
            edited = new
        }
    }

    private var rows: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(draft?.rows ?? []) { row in
                StepRowEditor(row: binding(row.id), canMoveUp: draft?.canMove(row.id, by: -1) ?? false, canMoveDown: draft?.canMove(row.id, by: 1) ?? false,
                              move: { offset in change { $0.move(row.id, by: offset) } }, remove: { change { $0.remove(row.id) } })
            }
            Menu {
                ForEach(StepsDraft.Keyword.allCases, id: \.self) { keyword in
                    Button(keyword.label) { change { $0.add(keyword) } }
                }
            } label: {
                Label { Text("Add a step", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "plus") }
                    .font(.subheadline.weight(.semibold)).frame(minHeight: 44)
            }
            .tint(Palette.dawn)
            examples
        }
    }

    @ViewBuilder private var examples: some View {
        if let draft, !draft.examples.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                Text("Examples", tableName: "AlondraDesign", bundle: #bundle).font(.caption.weight(.bold)).foregroundStyle(.secondary)
                TableCellsEditor(table: Binding(get: { self.draft?.examples ?? [] }, set: { new in change { $0.examples = new } }))
                Button(role: .destructive) { change { $0.examples = [] } } label: {
                    Label { Text("Remove the examples", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "trash") }.frame(minHeight: 44)
                }
            }
        } else {
            Button { change { $0.examples = [["name"], ["value"]] } } label: {
                Label { Text("Add examples", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "tablecells") }
                    .font(.subheadline.weight(.semibold)).frame(minHeight: 44)
            }
            .tint(Palette.dawn)
        }
    }

    private var fallback: some View {
        VStack(alignment: .leading, spacing: 6) {
            MultilineTextField(placeholder, text: Binding(get: { edited }, set: { edited = $0; text = $0 }), lines: 4...10)
                .padding(10).background(Color.primary.opacity(0.06), in: .rect(cornerRadius: 10))
            Label { Text("These lines are not steps yet, so they are edited as text", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "text.alignleft") }
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private func binding(_ id: Int) -> Binding<StepsDraft.Row> {
        Binding(
            get: { draft?.rows.first { $0.id == id } ?? StepsDraft.Row(id: id, keyword: .given) },
            set: { new in change { if let index = $0.rows.firstIndex(where: { $0.id == id }) { $0.rows[index] = new } } }
        )
    }

    private func change(_ edit: (inout StepsDraft) -> Void) {
        guard var next = draft else { return }
        edit(&next)
        draft = next
        text = next.text
    }
}

extension StepsDraft.Keyword {
    var label: LocalizedStringKey {
        switch self {
        case .given: LocalizedStringKey("Given")
        case .when: LocalizedStringKey("When")
        case .then: LocalizedStringKey("Then")
        case .and: LocalizedStringKey("And")
        case .but: LocalizedStringKey("But")
        }
    }
}

struct StepRowEditor: View {
    @Binding var row: StepsDraft.Row
    let canMoveUp: Bool
    let canMoveDown: Bool
    let move: (Int) -> Void
    let remove: () -> Void
    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 4) {
                Picker(selection: $row.keyword) {
                    ForEach(StepsDraft.Keyword.allCases, id: \.self) { Text($0.label, tableName: "AlondraDesign", bundle: #bundle).tag($0) }
                } label: { Text("Lane", tableName: "AlondraDesign", bundle: #bundle) }
                .pickerStyle(.menu).labelsHidden().tint(Palette.sky).fixedSize().frame(minHeight: 44)
                Spacer(minLength: 0)
                Button { move(-1) } label: { Label { Text("Move up", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "chevron.up") }.labelStyle(.iconOnly).frame(minWidth: 44, minHeight: 44) }
                    .disabled(!canMoveUp)
                Button { move(1) } label: { Label { Text("Move down", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "chevron.down") }.labelStyle(.iconOnly).frame(minWidth: 44, minHeight: 44) }
                    .disabled(!canMoveDown)
                Button(role: .destructive, action: remove) { Label { Text("Remove the step", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "trash") }.labelStyle(.iconOnly).frame(minWidth: 44, minHeight: 44) }
            }
            .buttonStyle(.borderless)
            MultilineTextField(String(localized: "What happens", table: "AlondraDesign", bundle: #bundle), text: $row.text, lines: 1...4)
                .padding(10).background(Color.primary.opacity(0.06), in: .rect(cornerRadius: 10))
            if row.table.isEmpty {
                Button { row.table = [["name"], ["value"]] } label: {
                    Label { Text("Add a table", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "tablecells") }.font(.footnote.weight(.semibold)).frame(minHeight: 44)
                }
                .buttonStyle(.borderless).tint(Palette.dawn)
            } else {
                TableCellsEditor(table: $row.table)
                Button(role: .destructive) { row.table = [] } label: {
                    Label { Text("Remove the table", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "trash") }.font(.footnote.weight(.semibold)).frame(minHeight: 44)
                }
                .buttonStyle(.borderless)
            }
        }
        .padding(12)
        .background(Palette.card, in: .rect(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Palette.hairline))
        .accessibilityElement(children: .contain)
    }
}

/// A table whose cells are fields: a row or a column is added or removed with its own button.
struct TableCellsEditor: View {
    @Binding var table: [[String]]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ScrollView(.horizontal, showsIndicators: false) {
                Grid(alignment: .leading, horizontalSpacing: 6, verticalSpacing: 6) {
                    ForEach(table.indices, id: \.self) { rowIndex in
                        GridRow {
                            ForEach(table[rowIndex].indices, id: \.self) { column in
                                TextField(text: cell(rowIndex, column)) { Text("Cell", tableName: "AlondraDesign", bundle: #bundle) }
                                    .textFieldStyle(.plain)
                                    .font(.mono(.footnote).weight(rowIndex == 0 ? .bold : .regular))
                                    .frame(minWidth: 96).padding(8).background(Color.primary.opacity(0.06), in: .rect(cornerRadius: 8))
                            }
                            if rowIndex > 0 {
                                Button { table.remove(at: rowIndex) } label: { Label { Text("Remove the row", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "minus.circle") }.labelStyle(.iconOnly).frame(minWidth: 44, minHeight: 44) }
                                    .buttonStyle(.borderless).foregroundStyle(Palette.rose)
                            }
                        }
                    }
                }
            }
            HStack {
                Button { table.append(Array(repeating: "", count: table.first?.count ?? 1)) } label: {
                    Label { Text("Add a row", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "plus") }.font(.footnote.weight(.semibold)).frame(minHeight: 44)
                }
                Button { table = table.map { $0 + [""] } } label: {
                    Label { Text("Add a column", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "plus.rectangle.on.rectangle") }.font(.footnote.weight(.semibold)).frame(minHeight: 44)
                }
                if (table.first?.count ?? 0) > 1 {
                    Button { table = table.map { Array($0.dropLast()) } } label: {
                        Label { Text("Remove the last column", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "minus") }.font(.footnote.weight(.semibold)).frame(minHeight: 44)
                    }
                }
            }
            .buttonStyle(.borderless).tint(Palette.dawn)
        }
    }

    private func cell(_ row: Int, _ column: Int) -> Binding<String> {
        Binding(
            get: { table.indices.contains(row) && table[row].indices.contains(column) ? table[row][column] : "" },
            set: { if table.indices.contains(row), table[row].indices.contains(column) { table[row][column] = $0 } }
        )
    }
}
