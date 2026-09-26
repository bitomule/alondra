import AlondraAttachmentsFeature
import AlondraContracts
import AlondraDomain
import AlondraNewWorkFeature
import AlondraShared
import ComposableArchitecture
import SwiftUI

struct ServiceNewRequestView: View {
    let store: StoreOf<NewRequestFeature>
    var body: some View {
        Form {
            Section("Request") {
                TextField("What should Alondra do?", text: Binding(get: { store.text }, set: { store.send(.textChanged($0)) }), axis: .vertical)
                    .lineLimit(6...16)
                Picker("Project", selection: Binding(get: { store.selection.project }, set: { store.send(.projectSelected($0)) })) {
                    Text("Projects folder").tag(nil as String?)
                    ForEach(store.projects, id: \.self) { Text($0).tag(Optional($0)) }
                }
            }.disabled(store.isSubmitting || store.submissionUncertain)
            Section("Launch options") {
                if store.isLoadingOptions { ProgressView("Loading options…") }
                if let error = store.optionsError {
                    Text(error).foregroundStyle(.red)
                    Button("Reload options") { store.send(.appeared) }
                }
                ForEach(store.selection.specs, id: \.id) { spec in
                    ServiceNewWorkOption(store: store, spec: spec)
                }
            }.disabled(store.isSubmitting || store.submissionUncertain)
            Section("GitHub issue") {
                if let issue = store.selection.issue {
                    LabeledContent("Selected", value: issue)
                    Button("Remove issue") { store.send(.issueSelected(nil)) }
                }
                TextField("Search issues or paste an issue URL", text: Binding(get: { store.issueQuery }, set: { store.send(.issueQueryChanged($0)) }))
                Button(store.isLoadingIssues ? "Loading issues…" : "Load issues") { store.send(.issuesOpened) }.disabled(store.isLoadingIssues)
                if store.issuesLoaded { Button("Refresh issues") { store.send(.issuesRefreshRequested) }.disabled(store.isLoadingIssues) }
                if let error = store.issuesError { Text(error).foregroundStyle(.red) }
                if let pasted = store.pastedIssue { Button("Use \(pasted)") { store.send(.issueSelected(pasted)) } }
                ForEach(store.visibleIssues) { issue in
                    Button { store.send(.issueSelected(issue.reference)) } label: {
                        VStack(alignment: .leading) {
                            Text(issue.title)
                            Text(issue.reference).font(.caption.monospaced())
                        }
                    }
                }
            }.disabled(store.isSubmitting || store.submissionUncertain)
            AttachmentPicker(store: store.scope(state: \.attachmentPicker, action: \.attachmentPicker))
                .disabled(store.isSubmitting || store.submissionUncertain)
            Section {
                Button(store.isSubmitting ? "Sending…" : "Create request") { store.send(.submitTapped) }
                    .disabled(!store.submitEnabled)
                if let error = store.submissionError { Text(error).foregroundStyle(store.submissionUncertain ? .orange : .red) }
                if store.successfulSubmissionCount > 0 && store.text.isEmpty { Text("Request created. Follow its progress in Tasks.") }
            }
        }
        .navigationTitle("New Request")
        .task { await store.send(.appeared).finish() }
    }
}

private struct ServiceNewWorkOption: View {
    let store: StoreOf<NewRequestFeature>
    let spec: OptionSpec
    var body: some View {
        if spec.kind == .toggle {
            Toggle(spec.title, isOn: Binding(get: { store.selection.current(spec) == .toggle(true) }, set: { store.send(.optionSelected(spec.id, .toggle($0))) }))
        } else {
            Picker(spec.title, selection: Binding(get: { store.selection.current(spec) }, set: { store.send(.optionSelected(spec.id, $0)) })) {
                ForEach(spec.values, id: \.id) { value in Text(spec.title(of: .value(value.id))).tag(OptionChoice.value(value.id)) }
            }
        }
    }
}
