import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// How a request is launched, as Alondra offers it: the project, the issue it starts from, and each
/// option the daemon sends, with its choices.
public struct MacRequestOptionsSheet: View {
    @Bindable var store: StoreOf<RequestOptionsFeature>

    public init(store: StoreOf<RequestOptionsFeature>) { self.store = store }

    public var body: some View {
        NavigationStack {
            Form {
                ForEach(store.state.visibleFields, id: \.id) { field in
                    control(field)
                }
            }
            .formStyle(.grouped)
            .navigationTitle("Options")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { store.send(.doneTapped) }
                }
            }
        }
        .sheet(item: $store.scope(state: \.issuePicker, action: \.issuePicker)) { picker in
            MacIssuePickerSheet(store: picker, cancel: { store.send(.issuePicker(.dismiss)) })
                .frame(minWidth: 440, minHeight: 360)
        }
    }

    @ViewBuilder private func control(_ field: Field) -> some View {
        switch field.kind {
        case .choice(let items):
            if ["model", "reasoning_effort"].contains(field.id) || items.contains(where: { $0.group != nil || $0.disabledReason != nil || $0.availableWhen != nil }) {
                NavigationLink {
                    RequestChoicePicker(store: store, fieldID: field.id)
                } label: {
                    LabeledContent { Text(verbatim: store.state.selectedLabel(field.id)) } label: { labelled(field, description: items.first { $0.id == store.state.selected(field.id) }?.description) }
                }
            } else {
                Picker(selection: Binding(get: { store.state.selected(field.id) }, set: { store.send(.choose(field.id, $0)) })) {
                    ForEach(store.state.choicesIncludingSelection(field.id), id: \.id) { item in Text(verbatim: item.label).tag(item.id).disabled(!store.state.isAvailable(item)) }
                } label: {
                    labelled(field, description: items.first { $0.id == store.state.selected(field.id) }?.description)
                }
                .pickerStyle(.menu)
            }
        case .toggle:
            Toggle(isOn: Binding(get: { store.state.isOn(field.id) }, set: { store.send(.toggle(field.id, $0)) })) {
                labelled(field, description: nil)
            }
        case .picker:
            LabeledContent {
                HStack(spacing: 8) {
                    if store.issue.isEmpty {
                        Button("Choose an issue") { store.send(.issueTapped) }
                    } else {
                        Text(verbatim: store.issue).foregroundStyle(Palette.sage)
                        Button("Change") { store.send(.issueTapped) }
                        Button("Remove", role: .destructive) { store.send(.removeIssue) }
                    }
                }
            } label: {
                labelled(field, description: nil)
            }
        default:
            EmptyView()
        }
    }

    private func labelled(_ field: Field, description: String?) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(verbatim: field.label)
            if let help = description ?? field.help {
                Text(verbatim: help).font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}
