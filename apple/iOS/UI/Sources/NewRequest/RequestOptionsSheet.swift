import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// How a request is launched: project, issue, what happens to its pull requests, and how hard
/// the node works.
public struct RequestOptionsSheet: View {
    @Bindable var store: StoreOf<RequestOptionsFeature>

    public init(store: StoreOf<RequestOptionsFeature>) { self.store = store }

    public var body: some View {
        NavigationStack {
            Form {
                if store.state.field("project") != nil { projectSection }
                if store.state.field("issue") != nil { issueSection }
                ForEach(store.state.visibleFields.filter { !["project", "issue"].contains($0.id) }, id: \.id) { field in
                    fieldSection(field)
                }
            }
            .scrollContentBackground(.hidden)
            .background(DawnBackground(glow: Palette.ember))
            .navigationTitle("Options")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { store.send(.doneTapped) }
                }
            }
            .navigationDestination(item: $store.scope(state: \.issuePicker, action: \.issuePicker)) { picker in
                IssuePickerView(store: picker)
            }
        }
    }

    private var projectSection: some View {
        Section {
            NavigationLink {
                ProjectPickerView(store: store)
            } label: {
                LabeledContent("Project") {
                    if let label = store.projectLabel {
                        Text(verbatim: label)
                    } else {
                        Text("Alondra picks")
                    }
                }
            }
        }
    }

    private var issueSection: some View {
        Section("Issue") {
            if store.issue.isEmpty {
                Button("Choose an issue") { store.send(.issueTapped) }
            } else {
                HStack {
                    Label { Text(verbatim: store.issue) } icon: { Image(systemName: "smallcircle.filled.circle") }
                        .foregroundStyle(Palette.sage)
                    Spacer()
                    Button("Remove", role: .destructive) { store.send(.removeIssue) }
                        .buttonStyle(.borderless)
                }
            }
        }
    }

    @ViewBuilder private func fieldSection(_ field: Field) -> some View {
        Section {
            switch field.kind {
            case .choice(let items):
                if ["model", "reasoning_effort"].contains(field.id) || items.contains(where: { $0.group != nil || $0.disabledReason != nil || $0.availableWhen != nil }) {
                    NavigationLink {
                        RequestChoicePicker(store: store, fieldID: field.id)
                    } label: {
                        LabeledContent { Text(verbatim: store.state.selectedLabel(field.id)) } label: { Text(verbatim: field.label) }
                    }
                } else {
                    Picker(selection: Binding(get: { store.state.selected(field.id) }, set: { store.send(.choose(field.id, $0)) })) {
                        ForEach(store.state.choicesIncludingSelection(field.id), id: \.id) { item in
                            VStack(alignment: .leading, spacing: 2) { Text(verbatim: item.label); if let description = item.description { Text(verbatim: description).font(.caption).foregroundStyle(.secondary) } }
                                .tag(item.id).disabled(!store.state.isAvailable(item))
                        }
                    } label: { Text(verbatim: field.label) }
                    .pickerStyle(.navigationLink)
                }
            case .toggle:
                Toggle(isOn: Binding(get: { store.state.isOn(field.id) }, set: { store.send(.toggle(field.id, $0)) })) {
                    Text(verbatim: field.label)
                }
            default: EmptyView()
            }
        } footer: {
            if let help = field.help { Text(verbatim: help) }
            else if case .choice(let items) = field.kind, (["model", "reasoning_effort"].contains(field.id) || items.contains(where: { $0.group != nil || $0.disabledReason != nil || $0.availableWhen != nil })), let selected = store.state.choices(field.id).first(where: { $0.id == store.state.selected(field.id) }), let description = selected.description { Text(verbatim: description) }
        }
    }

}

/// The projects, in the order the daemon sent them, filtered as you type.
private struct ProjectPickerView: View {
    @Bindable var store: StoreOf<RequestOptionsFeature>
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let shown = store.visibleProjects
        List {
            ForEach(shown, id: \.id) { item in
                Button {
                    store.send(.choose("project", item.id))
                    dismiss()
                } label: {
                    HStack {
                        Text(verbatim: item.label).foregroundStyle(Palette.ink)
                        Spacer()
                        if item.id == store.state.selected("project") {
                            Image(systemName: "checkmark").foregroundStyle(Palette.ember)
                        }
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(DawnBackground(glow: Palette.ember))
        .overlay {
            if shown.isEmpty { ContentUnavailableView.search(text: store.projectQuery) }
        }
        .searchable(text: $store.projectQuery, prompt: Text("Filter projects"))
        .navigationTitle("Project")
    }
}
