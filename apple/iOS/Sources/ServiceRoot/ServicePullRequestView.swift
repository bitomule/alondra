import AlondraAttachmentsFeature
import AlondraWorkspaceFeature
import AlondraPullRequestsFeature
import AlondraShared
import ComposableArchitecture
import SwiftUI

struct ServicePullRequestView: View {
    let store: StoreOf<ServicePullRequestFeature>
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        Form {
            if let pr = store.pr.pr {
                Section {
                    Text(pr.title).font(.headline)
                    Text(pr.summary).font(.caption.monospaced())
                    if let body = pr.body { Text(body).textSelection(.enabled) }
                    ForEach(store.pr.checks, id: \.self) { check in
                        LabeledContent(check.name, value: check.state)
                    }
                }
                Section("Actions") {
                    ForEach(pr.methods, id: \.self) { method in
                        Button("Merge: \(method)") { store.send(.pr(.chooseMergeMethod(method))) }
                    }
                    if pr.hasConflicts { Button("Resolve conflicts") { store.send(.pr(.resolveConflictsTapped)) } }
                    Button("Set aside") { store.send(.pr(.setAsideTapped)) }
                }.disabled(!store.pr.canAct)
            }
            ServicePROutcome(outcome: store.pr.mutationOutcome)
            Section("Diff") {
                if store.diff.isLoading { ProgressView("Loading diff…") }
                if let error = store.diff.loadError {
                    Text(error).foregroundStyle(.red)
                    Button("Retry") { store.send(.diff(.retryTapped)) }
                }
                ForEach(store.diff.files ?? []) { file in
                    DisclosureGroup(isExpanded: Binding(get: { !store.diff.collapsed.contains(file.id) }, set: { _ in store.send(.diff(.toggleFile(file.id))) })) {
                        ForEach(file.lines) { line in
                            VStack(alignment: .leading) {
                                Text(line.text).font(.caption.monospaced()).textSelection(.enabled)
                                if let anchor = line.anchor {
                                    TextField("Comment", text: Binding(get: { store.diff.comments[anchor] ?? "" }, set: {
                                        store.send(.diff(.editLine(anchor)))
                                        store.send(.diff(.saveLineComment(anchor, $0)))
                                    }), axis: .vertical).disabled(store.diff.isSending)
                                }
                            }
                        }
                    } label: { Text(file.id).font(.caption.monospaced()) }
                }
            }
            AttachmentPicker(store: store.scope(state: \.diff.attachmentPicker, action: \.diff.attachmentPicker))
                .disabled(store.diff.isSending || store.diff.mutationOutcome?.uncertainID != nil)
            Section("Review") {
                TextField("General feedback", text: Binding(get: { store.diff.note }, set: { store.send(.diff(.noteChanged($0))) }), axis: .vertical)
                    .disabled(store.diff.isSending)
                if store.diff.commentsNeedReview {
                    Text("The diff changed. Check each comment before confirming.")
                    Button("Comments match this diff") { store.send(.diff(.confirmCommentsForCurrentDiff)) }
                }
                Button("Ask a question") { store.send(.diff(.askTapped)) }.disabled(!store.diff.canReview)
                Button("Request changes") { store.send(.diff(.requestChangesTapped)) }.disabled(!store.diff.canReview)
                if let message = store.diff.validationError { Text(message).foregroundStyle(.red) }
            }
            ServicePROutcome(outcome: store.diff.mutationOutcome)
        }
        .navigationTitle("Pull request")
        .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } } }
        .confirmationDialog("Merge this pull request?", isPresented: Binding(get: { store.pr.selectedMergeMethod != nil }, set: { if !$0 { store.send(.pr(.cancelMerge)) } })) {
            Button("Merge", role: .destructive) { store.send(.pr(.confirmMerge)) }
        }
        .task { await store.send(.diff(.appeared)).finish() }
        .onDisappear { store.send(.diff(.disappeared)) }
    }
}

private struct ServicePROutcome: View {
    let outcome: PRMutationOutcome?
    var body: some View {
        if let outcome {
            Section {
                switch outcome {
                case .confirmed: Text("Completed")
                case .closed: Text("This pull request is closed")
                case .rejected(let message): Text(message).foregroundStyle(.red)
                case .unknown: Text("The result is unknown. Check Alondra before taking another action.").foregroundStyle(.orange)
                case .obsolete: Text("The connection changed. Reopen this pull request.")
                }
            }
        }
    }
}
