import AlondraContract
import AlondraDesign
import ComposableArchitecture
import SwiftUI

struct AccountOrderSheet: View {
    @Bindable var store: StoreOf<ServerSettingsFeature>
    var body: some View {
        NavigationStack {
            if let draft = store.orderDraft {
                List {
                    Section {
                        ForEach(draft.rows) { row in orderRow(row, in: draft).disabled(draft.saving || draft.conflict) }
                            .onMove { store.send(.orderRowsMoved($0, to: $1)) }
                    } header: {
                        Text("New work follows this order")
                    } footer: {
                        Text("Changes apply when you save. Running work continues.")
                    }
                    if draft.conflict {
                        Section {
                            Text("The account order changed on the Mac. Reload it before saving.").foregroundStyle(Palette.ember)
                            Button("Reload order") { store.send(.orderBegan) }
                        }
                    }
                    if let error = store.error { Text(verbatim: error).foregroundStyle(Palette.rose) }
                    if draft.saving { ProgressView("Saving order…") }
                    if draft.saving, store.actions.unsure != nil {
                        Button("Check pending save") { store.send(.actions(.retryNow)) }
                    }
                }
                #if os(iOS)
                .environment(\.editMode, .constant(.active))
                #endif
                .navigationTitle("Reorder accounts")
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Cancel") { store.send(.orderDraftPresented(nil)) }.disabled(draft.saving)
                    }
                    ToolbarItem(placement: .confirmationAction) {
                        Button("Save") { store.send(.orderSaved) }.disabled(!draft.hasChanges || draft.conflict || draft.saving || store.actions.inFlight)
                            .keyboardShortcut(.defaultAction)
                    }
                }
                .interactiveDismissDisabled(draft.saving)

            }
        }
        #if os(macOS)
        .frame(minWidth: 480, minHeight: 480)
        #endif
    }

    private func orderRow(_ row: AlondraContract.Row, in draft: AccountOrderDraft) -> some View {
        let position = draft.rows.firstIndex { $0.id == row.id } ?? 0
        return HStack {
            VStack(alignment: .leading, spacing: 3) {
                Text(verbatim: row.title).foregroundStyle(Palette.ink)
                if let account = row.account { Text(verbatim: account.key.provider.displayName).font(.caption).foregroundStyle(.secondary) }
            }
            Spacer()
            Menu {
                Button("Move up") { store.send(.orderMoved(row.id, to: position - 1)) }.disabled(position == 0)
                Button("Move down") { store.send(.orderMoved(row.id, to: position + 1)) }.disabled(position + 1 == draft.rows.count)
                Menu("Move to position") {
                    ForEach(draft.rows) { target in
                        let destination = draft.rows.firstIndex { $0.id == target.id } ?? 0
                        Button { store.send(.orderMoved(row.id, to: destination)) } label: { Text(verbatim: "\(destination + 1). \(target.title)") }
                            .disabled(destination == position)
                    }
                }
            } label: { Label("Move", systemImage: "arrow.up.arrow.down").labelStyle(.iconOnly) }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text(verbatim: "\(position + 1). \(row.title), \(row.account?.key.provider.displayName ?? "")"))
        .accessibilityAction(named: Text("Move up")) { if position > 0 { store.send(.orderMoved(row.id, to: position - 1)) } }
        .accessibilityAction(named: Text("Move down")) { if position + 1 < draft.rows.count { store.send(.orderMoved(row.id, to: position + 1)) } }
        .accessibilityIdentifier("account.order.\(row.id)")
    }
}
