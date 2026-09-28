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
        VStack(alignment: .leading, spacing: 14) {
            form(presentation: presentation)
            if let error = store.submissionError { Text(error).foregroundStyle(store.submissionUncertain ? .orange : .red) }
            if store.submissionUncertain {
                HStack {
                    Button("Edit Again") { store.send(.editUncertainDraft) }
                    Button("Discard Draft", role: .destructive) { store.send(.discardUncertainDraft) }
                }
            }
            HStack {
                ServiceAttachFilesButton(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                    .disabled(locked)
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button { store.send(.submitTapped) } label: {
                    if store.isSubmissionInFlight { ProgressView().controlSize(.small) } else { Text("Send") }
                }.buttonStyle(.borderedProminent).keyboardShortcut(.return, modifiers: .command)
                    .help(Text(verbatim: "\(String(localized: "Send")) (⌘↩)")).disabled(!store.submitEnabled)
            }
        }.padding(22)
            .onAppear { focused = true }
            .task { await store.send(.appeared).finish() }
            .onDisappear { store.send(.disappeared) }
            .onChange(of: store.successfulSubmissionCount) { _, _ in dismiss() }
    }

    private var locked: Bool { store.isSubmissionInFlight || store.submissionUncertain }

    private func form(presentation: StoreOf<MacNewRequestPresentation>) -> some View {
        @Bindable var presentation = presentation
        return VStack(alignment: .leading, spacing: 14) {
            Text("What do you want done?").font(.title3.weight(.semibold))
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
                if let error = store.optionsError {
                    GridRow {
                        Color.clear.gridCellUnsizedAxes([.horizontal, .vertical])
                        Label(error, systemImage: "exclamationmark.triangle")
                            .font(.caption).foregroundStyle(.orange).lineLimit(2)
                    }
                }
            }.opacity(store.isLoadingOptions ? 0.4 : 1).animation(.default, value: store.isLoadingOptions)
        }.disabled(locked)
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

    private func pick(_ project: String?) {
        store.send(.projectSelected(project))
        presentation.projectPicker = false
    }

    private var shown: [String] { ProjectList.filter(store.projects, query: presentation.projectQuery) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Filter projects", text: $presentation.projectQuery)
                .textFieldStyle(.roundedBorder)
                .onSubmit { if let first = shown.first { pick(first) } }
            List {
                if presentation.projectQuery.isEmpty {
                    row(nil, title: String(localized: "The orchestrator decides"))
                }
                ForEach(shown, id: \.self) { row($0, title: $0) }
            }
            .listStyle(.plain)
            .overlay {
                if shown.isEmpty && !presentation.projectQuery.isEmpty {
                    Text("No project matches")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(12)
        .frame(width: 320, height: 340)
    }

    private func row(_ project: String?, title: String) -> some View {
        Button {
            pick(project)
        } label: {
            HStack {
                Text(verbatim: title)
                Spacer()
                if project == store.selection.project {
                    Image(systemName: "checkmark")
                        .foregroundStyle(.tint)
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}

private struct ServiceIssuePicker: View {
    let store: StoreOf<NewRequestFeature>
    @Bindable var presentation: StoreOf<MacNewRequestPresentation>

    private func pick(_ reference: String) {
        store.send(.issueSelected(reference))
        presentation.issuePicker = false
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                TextField("Filter, or paste an issue link", text: Binding(get: { store.issueQuery }, set: { store.send(.issueQueryChanged($0)) }))
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { if let first = store.pastedIssue ?? store.visibleIssues.first?.reference { pick(first) } }
                Button {
                    store.send(.issuesRefreshRequested)
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .help(Text("Search GitHub again"))
                .accessibilityLabel(Text("Search GitHub again"))
                .disabled(store.isLoadingIssues)
            }
            if let error = store.issuesError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .lineLimit(2)
            }
            List {
                if let pasted = store.pastedIssue {
                    Button {
                        pick(pasted)
                    } label: {
                        Label("Use \(pasted)", systemImage: "link")
                    }
                    .buttonStyle(.plain)
                }
                ForEach(store.visibleIssues) { issue in
                    Button {
                        pick(issue.reference)
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(verbatim: issue.title)
                                .lineLimit(2)
                            Text(verbatim: ([issue.reference] + issue.labels).joined(separator: " · "))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                }
            }
            .listStyle(.plain)
            .overlay {
                if store.isLoadingIssues && store.issues.isEmpty {
                    ProgressView()
                } else if store.issuesLoaded && store.visibleIssues.isEmpty && store.pastedIssue == nil {
                    Text("No open issues match")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(12)
        .frame(width: 440, height: 360)
        .task { store.send(.issuesOpened) }
    }
}
