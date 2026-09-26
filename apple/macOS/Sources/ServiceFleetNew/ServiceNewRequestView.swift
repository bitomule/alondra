import AlondraAttachmentsFeature
import AlondraContracts
import AlondraDomain
import AlondraNewWorkFeature
import AlondraShared
import ComposableArchitecture
import SwiftUI

@Reducer
struct MacNewRequestPresentation {
    @ObservableState
    struct State: Equatable {
        var projectPicker = false
        var issuePicker = false
        var projectQuery = ""
    }
    enum Action: BindableAction { case binding(BindingAction<State>) }
    var body: some ReducerOf<Self> {
        BindingReducer()
        Reduce { state, action in
            if case .binding(\.projectPicker) = action, state.projectPicker { state.projectQuery = "" }
            return .none
        }
    }
}

struct ServiceNewRequestView: View {
    let store: StoreOf<NewRequestFeature>
    @State private var presentation = Store(initialState: MacNewRequestPresentation.State()) { MacNewRequestPresentation() }
    @Environment(\.dismiss) private var dismiss
    @FocusState private var focused: Bool
    var body: some View {
        @Bindable var presentation = presentation
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text("What do you want done?").font(.title3.weight(.semibold))
                Text("I’ll pass it to the orchestrator: it prepares the mission and launches the node.").foregroundStyle(.secondary)
            }
            TextEditor(text: Binding(get: { store.text }, set: { store.send(.textChanged($0)) }))
                .font(.body).scrollContentBackground(.hidden).padding(8)
                .frame(minWidth: 520, minHeight: 180)
                .background(.background, in: .rect(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color.primary.opacity(0.12)))
                .overlay(alignment: .topLeading) {
                    if store.text.isEmpty {
                        Text("For example: “Find out why Redraft is slow to open and propose a fix”")
                            .foregroundStyle(.tertiary).padding(.horizontal, 13).padding(.vertical, 8).allowsHitTesting(false)
                    }
                }.focused($focused)
                .modifier(ServiceDropsFiles(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker)))
            ServiceAttachmentStrip(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 8) {
                GridRow {
                    Text("Project").foregroundStyle(.secondary).gridColumnAlignment(.trailing)
                    Button { presentation.projectPicker = true } label: {
                        HStack(spacing: 4) {
                            Text(store.selection.project ?? String(localized: "The orchestrator decides"))
                            Image(systemName: "chevron.up.chevron.down").font(.caption2).foregroundStyle(.secondary)
                        }
                    }.popover(isPresented: $presentation.projectPicker, arrowEdge: .bottom) {
                        ServiceProjectPicker(store: store, presentation: presentation)
                    }
                }
                GridRow {
                    Text("Issue").foregroundStyle(.secondary)
                    if let issue = store.selection.issue {
                        HStack(spacing: 6) {
                            Label(issue, systemImage: "smallcircle.filled.circle").foregroundStyle(.green)
                            Button { store.send(.issueSelected(nil)) } label: { Image(systemName: "xmark.circle.fill") }
                                .buttonStyle(.borderless).help("Remove the issue").accessibilityLabel("Remove the issue")
                        }
                    } else {
                        Button("From an issue…") { presentation.issuePicker = true; store.send(.issuesOpened) }
                            .popover(isPresented: $presentation.issuePicker, arrowEdge: .bottom) {
                                ServiceIssuePicker(store: store, presentation: presentation)
                            }
                    }
                }
                ForEach(store.selection.specs, id: \.id) { spec in
                    GridRow {
                        Text(verbatim: spec.title).foregroundStyle(.secondary)
                        ServiceNewWorkOption(store: store, spec: spec)
                    }
                }
            }.opacity(store.isLoadingOptions ? 0.4 : 1).animation(.default, value: store.isLoadingOptions)
            if let error = store.optionsError {
                HStack { Text(error).font(.callout).foregroundStyle(.red); Button("Retry") { store.send(.appeared) } }
            }
            if let error = store.submissionError { Text(error).foregroundStyle(store.submissionUncertain ? .orange : .red) }
            HStack {
                ServiceAttachFilesButton(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button { store.send(.submitTapped) } label: {
                    if store.isSubmitting { ProgressView().controlSize(.small) } else { Text("Send") }
                }.buttonStyle(.borderedProminent).keyboardShortcut(.return, modifiers: .command)
                    .help(Text(verbatim: "\(String(localized: "Send")) (⌘↩)")).disabled(!store.submitEnabled)
            }
        }.padding(22)
            .disabled(store.isSubmitting || store.submissionUncertain)
            .onAppear { focused = true }
            .task { await store.send(.appeared).finish() }
            .onChange(of: store.successfulSubmissionCount) { _, _ in dismiss() }
    }
}

private struct ServiceNewWorkOption: View {
    let store: StoreOf<NewRequestFeature>
    let spec: OptionSpec
    var body: some View {
        Menu {
            ForEach(spec.choices, id: \.self) { choice in
                Button { store.send(.optionSelected(spec.id, choice)) } label: {
                    if choice == store.selection.current(spec) { Image(systemName: "checkmark") }
                    Text(verbatim: spec.title(of: choice))
                    if let description = spec.description(of: choice) { Text(verbatim: description) }
                }
            }
        } label: { Text(verbatim: spec.title(of: store.selection.current(spec))) }
            .menuStyle(.button).fixedSize()
    }
}

private struct ServiceProjectPicker: View {
    let store: StoreOf<NewRequestFeature>
    @Bindable var presentation: StoreOf<MacNewRequestPresentation>
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Filter projects", text: $presentation.projectQuery).textFieldStyle(.roundedBorder)
            List {
                if presentation.projectQuery.isEmpty {
                    Button("The orchestrator decides") { store.send(.projectSelected(nil)); presentation.projectPicker = false }
                }
                ForEach(ProjectList.filter(store.projects, query: presentation.projectQuery), id: \.self) { project in
                    Button { store.send(.projectSelected(project)); presentation.projectPicker = false } label: {
                        HStack {
                            Text(verbatim: project)
                            Spacer()
                            if project == store.selection.project { Image(systemName: "checkmark").foregroundStyle(.tint) }
                        }.contentShape(.rect)
                    }.buttonStyle(.plain)
                }
            }.listStyle(.plain)
        }.padding(12).frame(width: 320, height: 340)
    }
}

private struct ServiceIssuePicker: View {
    let store: StoreOf<NewRequestFeature>
    @Bindable var presentation: StoreOf<MacNewRequestPresentation>
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Search issues or paste an issue URL", text: Binding(get: { store.issueQuery }, set: { store.send(.issueQueryChanged($0)) })).textFieldStyle(.roundedBorder)
            if store.isLoadingIssues { ProgressView() }
            if let error = store.issuesError { Text(error).foregroundStyle(.red) }
            if let pasted = store.pastedIssue {
                Button("Use \(pasted)") { store.send(.issueSelected(pasted)); presentation.issuePicker = false }
            }
            List(store.visibleIssues) { issue in
                Button { store.send(.issueSelected(issue.reference)); presentation.issuePicker = false } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(issue.title).fontWeight(.medium)
                        Text(issue.reference).font(.caption).foregroundStyle(.secondary)
                    }
                }.buttonStyle(.plain)
            }.listStyle(.plain)
            Button("Refresh") { store.send(.issuesRefreshRequested) }.disabled(store.isLoadingIssues)
        }.padding(12).frame(width: 440, height: 380)
    }
}
