import AlondraShared
import SwiftUI

/// A PR a node opened: what it changes, its checks, and merge it or send it back.
struct PullRequestDetail: View {
    let pr: PullRequest
    let store: FleetStore
    let drafts: Drafts
    @Bindable var draft: ReviewDraft
    @State private var confirmingMerge = false
    @State private var files: [DiffFile]?
    @State private var diffError: String?
    @FocusState private var focus: DetailField?

    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: InboxItem.pullRequest(pr).style, node: pr.nodeName, date: pr.openedAt, title: pr.title)
                nodeSays
                nodeAsks
                Text(pr.summary)
                    .font(.callout.monospaced())
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
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
                if let url = URL(string: pr.url) {
                    Link(destination: url) {
                        Label("Open on GitHub", systemImage: "arrow.up.right.square")
                    }
                }
                changes
            }
        } bar: {
            VStack(alignment: .leading, spacing: 12) {
                CommentsSummary(draft: draft)
                Composer(
                    placeholder: String(localized: "What should change?"),
                    text: $draft.general,
                    focus: $focus,
                    sendTitle: nil,
                    canSend: false,
                    send: requestChanges,
                    attachments: draft.attachments,
                    client: store.client
                )
                actions
            }
            .animation(.snappy(duration: 0.2), value: draft.comments.count)
        }
        .confirmationDialog("Merge PR #\(pr.number)?", isPresented: $confirmingMerge) {
            ForEach(pr.methods, id: \.self) { method in
                Button("Merge (\(PullRequest.methodLabel(method)))") { merge(method) }
            }
        } message: {
            Text(pr.summary)
        }
    }

    private var hasChanges: Bool { !review.isEmpty || !draft.attachments.isEmpty }

    private var actions: some View {
        CardActions {
            Button("Set Aside") { setAside() }
                .help("Take the card out of the inbox without merging")
        } trailing: {
            if !pr.hasConflicts, let reason = pr.mergeBlockedReason {
                Text(reason)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .multilineTextAlignment(.trailing)
            }
            Button("Ask") { ask() }
                .disabled(!hasChanges)
                .help("Ask the node about this PR; the card stays in the inbox")
            Group {
                if hasChanges {
                    Button(requestChangesTitle) { requestChanges() }
                        .buttonStyle(.borderedProminent)
                } else {
                    Button(requestChangesTitle) { requestChanges() }
                        .disabled(true)
                }
            }
            .help("Send the node what should change; the card comes back when it pushes again")
            if pr.hasConflicts {
                Button("Resolve Conflicts") { resolveConflicts() }
                    .buttonStyle(.borderedProminent)
                    .help("Its node brings the base into the branch, resolves the conflicts and pushes; the card comes back when it can be merged")
            } else if hasChanges {
                Button("Merge…") { confirmingMerge = true }
                    .disabled(pr.mergeBlockedReason != nil)
            } else {
                Button("Merge…") { confirmingMerge = true }
                    .buttonStyle(.borderedProminent)
                    .disabled(pr.mergeBlockedReason != nil)
            }
        }
        .animation(.snappy(duration: 0.18), value: hasChanges)
    }

    private var requestChangesTitle: String {
        draft.comments.isEmpty ? String(localized: "Request Changes") : String(localized: "Request Changes (\(draft.comments.count))")
    }

    @ViewBuilder
    private var nodeSays: some View {
        let reports = store.reports(forPullRequest: pr.id)
        if !reports.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("The node says")
                    .font(.title3.weight(.semibold))
                ForEach(reports) { report in
                    AttachedReport(report: report, pullRequestURL: pr.url)
                }
            }
        }
    }

    @ViewBuilder
    private var nodeAsks: some View {
        let questions = store.questions(forPullRequest: pr.id)
        if !questions.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                Text("The node asks")
                    .font(.title3.weight(.semibold))
                ForEach(questions) { question in
                    AttachedQuestion(question: question, pullRequestURL: pr.url, store: store, drafts: drafts, draft: drafts.draft(for: InboxItem.question(question).id))
                }
            }
            .animation(.snappy, value: questions.map(\.id))
        }
    }

    private func merge(_ method: String) {
        let id = pr.id
        Task { _ = await store.resolve(.pullRequest(pr)) { try await $0.mergePullRequest(id, method: method) } }
    }

    private func resolveConflicts() {
        let id = pr.id
        Task { _ = await store.resolve(.pullRequest(pr)) { try await $0.resolveConflicts(onPullRequest: id) } }
    }

    @ViewBuilder
    private var changes: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Changes")
                .font(.title3.weight(.semibold))
            if let files {
                Text("Hover over a line to comment on it")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                PRDiffReview(files: files, draft: draft, focus: $focus)
            } else if let diffError {
                Label(diffError, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.secondary)
            } else {
                ProgressView()
                    .controlSize(.small)
            }
        }
        .task(id: pr.id) { await loadDiff() }
    }

    private var review: String {
        ReviewExport.text(comments: draft.comments.map { ($0.anchor, $0.text) }, general: draft.general)
    }

    private func loadDiff() async {
        do {
            files = UnifiedDiff.parse(try await store.client.pullRequestDiff(pr.id))
        } catch {
            diffError = error.localizedDescription
        }
    }

    private func ask() {
        let (id, text, attachments) = (pr.id, review, draft.attachments)
        Task {
            if await store.resolve(.pullRequest(pr), { client in
                try await client.ask(onPullRequest: id, text: text, attachments: try await attachments.storedIDs(using: client))
            }) {
                drafts.discard(InboxItem.pullRequest(pr).id)
            }
        }
    }

    private func requestChanges() {
        let (id, text, attachments) = (pr.id, review, draft.attachments)
        Task {
            if await store.resolve(.pullRequest(pr), { client in
                try await client.requestChanges(onPullRequest: id, text: text, attachments: try await attachments.storedIDs(using: client))
            }) {
                drafts.discard(InboxItem.pullRequest(pr).id)
            }
        }
    }

    private func setAside() {
        let id = pr.id
        Task { _ = await store.resolve(.pullRequest(pr)) { try await $0.dismissPullRequest(id) } }
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
