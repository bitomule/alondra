import AlondraAttachmentsFeature
import AlondraWorkspaceFeature
import AlondraPullRequestsFeature
import AlondraQuestionsFeature
import AlondraShared
import ComposableArchitecture
import SwiftUI

struct ServicePullRequestView: View {
    let store: StoreOf<ServicePullRequestFeature>
    var taskTitle: String? = nil
    @FocusState private var focus: DetailField?
    private var workspace: DomainWorkspace? { store.pr.workspace }
    private var comments: [ReviewComment] {
        (store.diff.files ?? []).flatMap(\.lines).compactMap { line in
            guard let anchor = line.anchor, let text = store.diff.comments[anchor] else { return nil }
            return ReviewComment(anchor: anchor, quote: line.text, text: text)
        }
    }
    var body: some View { content(store.lastKnownCard) }
    private func content(_ card: DomainPullRequestCard) -> some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: card.root.style, node: workspace?.agentName(of: card.root),
                    date: card.presentation.flatMap { workspace?.createdAt(of: $0) },
                    title: store.observation?.title ?? card.root.title, task: taskTitle)
                nodeSays
                nodeAsks
                Text(card.summary)
                    .font(.callout.monospaced())
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                if let pr = store.observation { observed(pr) }
                if let url = card.webURL {
                    Link(destination: url) {
                        Label("Open on GitHub", systemImage: "arrow.up.right.square")
                    }
                }
                changes
            }
        } bar: {
            VStack(alignment: .leading, spacing: 12) {
                ServiceCommentsSummary(comments: comments) { store.send(.diff(.removeLineComment($0.anchor))) }
                ServiceComposer(
                    placeholder: String(localized: "What should change?"),
                    text: Binding(get: { store.diff.note }, set: { store.send(.diff(.noteChanged($0))) }),
                    focus: $focus,
                    sendTitle: nil,
                    canSend: false,
                    submit: { store.send(.diff(.requestChangesTapped)) },
                    attachmentsStore: store.scope(state: \.diff.attachmentPicker, action: \.diff.attachmentPicker)
                )
                actions(card)
                ServicePROutcome(outcome: store.pr.mutationOutcome)
                ServicePROutcome(outcome: store.diff.mutationOutcome)
                if let error = store.diff.validationError { Text(error).foregroundStyle(.red) }
            }
            .animation(.snappy(duration: 0.2), value: store.diff.comments.count)
        }
        .confirmationDialog("Merge PR #\(card.pullRequest.number)?", isPresented: Binding(get: { store.pr.mergeConfirmationPresented }, set: { store.send(.pr(.presentMergeConfirmation($0))) })) {
            ForEach(card.pullRequest.allowedMethods, id: \.self) { method in
                Button("Merge (\(method.label))") { store.send(.pr(.mergeMethodConfirmed(method))) }
            }
        } message: {
            Text(card.summary)
        }
        .task { await store.send(.diff(.appeared)).finish() }
        .onDisappear { store.send(.diff(.disappeared)) }
        .attachmentDownloadPresentation(store: store.scope(state: \.downloads, action: \.downloads))
    }

    @ViewBuilder
    private func observed(_ pr: PullRequest) -> some View {
        if pr.hasConflicts {
            Label("It has conflicts with its base branch", systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
        } else if pr.mergeabilityUnknown {
            Label("GitHub is checking whether it can be merged", systemImage: "clock")
                .foregroundStyle(.secondary)
        }
        if let stackedOn = pr.stackedOn {
            Label(stackedOn, systemImage: "square.stack.3d.up")
                .foregroundStyle(.secondary)
        }
        if let checks = pr.checks, !checks.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(checks, id: \.self) { check in
                    CheckRow(check: check)
                }
            }
        }
        if let body = pr.body, !body.isEmpty {
            MarkdownText(text: body)
        }
    }

    private var hasChanges: Bool { store.diff.hasDraft || !store.diff.attachmentPicker.entries.isEmpty }

    private func actions(_ card: DomainPullRequestCard) -> some View {
        CardActions {
            Button("Set Aside") { store.send(.pr(.setAsideTapped)) }
                .help("Take the card out of the inbox without merging")
        } trailing: {
            if !card.hasConflicts, let reason = store.observation?.mergeBlockedReason {
                Text(reason)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .multilineTextAlignment(.trailing)
            }
            Button("Ask") { store.send(.diff(.askTapped)) }
                .disabled(!hasChanges || !store.diff.canReview)
                .help("Ask the node about this PR; the card stays in the inbox")
            Group {
                if hasChanges {
                    Button(requestChangesTitle) { store.send(.diff(.requestChangesTapped)) }
                        .buttonStyle(.borderedProminent)
                        .disabled(!store.diff.canReview)
                } else {
                    Button(requestChangesTitle) { store.send(.diff(.requestChangesTapped)) }
                        .disabled(true)
                }
            }
            .help("Send the node what should change; the card comes back when it pushes again")
            if card.hasConflicts {
                Button("Resolve Conflicts") { store.send(.pr(.resolveConflictsTapped)) }
                    .buttonStyle(.borderedProminent)
                    .help("Its node brings the base into the branch, resolves the conflicts and pushes; the card comes back when it can be merged")
            } else if hasChanges {
                Button("Merge…") { store.send(.pr(.presentMergeConfirmation(true))) }
                    .disabled(store.observation?.mergeBlockedReason != nil)
            } else {
                Button("Merge…") { store.send(.pr(.presentMergeConfirmation(true))) }
                    .buttonStyle(.borderedProminent)
                    .disabled(store.observation?.mergeBlockedReason != nil)
            }
        }
        .animation(.snappy(duration: 0.18), value: hasChanges)
    }

    private var requestChangesTitle: String {
        store.diff.comments.isEmpty ? String(localized: "Request Changes") : String(localized: "Request Changes (\(store.diff.comments.count))")
    }

    @ViewBuilder
    private var nodeSays: some View {
        let reports = store.information
        if !reports.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("The node says")
                    .font(.title3.weight(.semibold))
                ForEach(reports) { report in
                    ServiceAttachedReport(report: report, attachments: workspace?.interaction(of: report)?.attachments ?? [],
                                          downloads: store.scope(state: \.downloads, action: \.downloads))
                }
            }
        }
    }

    @ViewBuilder
    private var nodeAsks: some View {
        let questions = store.questions
        if !questions.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("The node asks")
                    .font(.title3.weight(.semibold))
                ForEach(store.scope(state: \.questionFeatures, action: \.attachedQuestion).filter { store.visibleQuestionIDs.contains($0.id) }) { child in
                    ServiceAttachedQuestion(store: child.scope(state: \.question, action: \.question))
                }
            }
            .animation(.snappy, value: questions.map(\.id))
        }
    }

    private var changes: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Changes").font(.title3.weight(.semibold))
            if store.diff.files != nil {
                Text("Hover over a line to comment on it").font(.caption).foregroundStyle(.secondary)
                if store.diff.diffTruncated {
                    Label("This diff is too long to show in full. Open the PR on GitHub for the rest.", systemImage: "scissors")
                        .font(.caption).foregroundStyle(.orange)
                }
                ServicePRDiffReview(store: store.scope(state: \.diff, action: \.diff), focus: $focus)
                    .disabled(store.diff.isSending)
            } else if let error = store.diff.loadError {
                Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary)
                Button("Retry") { store.send(.diff(.retryTapped)) }
            } else { ProgressView().controlSize(.small) }
            if store.diff.commentsNeedReview {
                Text("The diff changed. Check each comment before confirming.")
                Button("Comments match this diff") { store.send(.diff(.confirmCommentsForCurrentDiff)) }
            }
        }
    }
}
private struct CheckRow: View {
    let check: PRCheck

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: check.passed ? "checkmark.circle.fill" : check.failed ? "xmark.circle.fill" : "clock")
                .foregroundStyle(check.passed ? .green : check.failed ? .red : .orange)
            if let link = check.url.flatMap(URL.init(string:)) {
                Link(check.name, destination: link)
            } else {
                Text(check.name)
            }
        }
        .font(.callout)
    }
}
private struct ServicePROutcome: View {
    let outcome: PRMutationOutcome?
    var body: some View {
        if let outcome {
            VStack(alignment: .leading) {
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
