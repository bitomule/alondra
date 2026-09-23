import AlondraShared
import SwiftUI

/// A PR a node opened: what it changes, its checks, and merge it or send it back.
struct PullRequestDetail: View {
    let pr: PullRequest
    let store: FleetStore
    @State private var method: String
    @State private var note = ""
    @State private var confirmingMerge = false
    @State private var files: [DiffFile]?
    @State private var diffError: String?
    @State private var comments: [String: String] = [:]
    @State private var commenting: DiffLine?

    init(pr: PullRequest, store: FleetStore) {
        self.pr = pr
        self.store = store
        _method = State(initialValue: pr.methods.first ?? "merge")
    }

    var body: some View {
        DetailScaffold {
            VStack(alignment: .leading, spacing: 22) {
                DetailHeader(style: InboxItem.pullRequest(pr).style, node: pr.nodeName, date: pr.openedAt, title: pr.title)
                Text(pr.summary)
                    .font(.callout.monospaced())
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
                if pr.hasConflicts {
                    Label("It has conflicts with its base branch", systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
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
            HStack(alignment: .center, spacing: 10) {
                TextField("What should change?", text: $note, axis: .vertical)
                    .lineLimit(1...4)
                    .textFieldStyle(.roundedBorder)
                Button(comments.isEmpty ? String(localized: "Request Changes") : String(localized: "Request Changes (\(comments.count))")) { requestChanges() }
                    .disabled(review.isEmpty)
                    .controlSize(.large)
                Picker("Method", selection: $method) {
                    ForEach(pr.methods, id: \.self) { m in
                        Text(PullRequest.methodLabel(m)).tag(m)
                    }
                }
                .labelsHidden()
                .fixedSize()
                .help("How to merge it; the method last used in this repo comes first")
                Button("Merge…") { confirmingMerge = true }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                Button("Set Aside") { setAside() }
                    .controlSize(.large)
                    .help("Take the card out of the inbox without merging")
            }
        }
        .sheet(item: $commenting) { line in
            LineCommentSheet(line: line, initial: line.anchor.flatMap { comments[$0] } ?? "") { text in
                guard let anchor = line.anchor else { return }
                comments[anchor] = text.isEmpty ? nil : text
            }
        }
        .confirmationDialog("Merge PR #\(pr.number)?", isPresented: $confirmingMerge) {
            Button("Merge (\(PullRequest.methodLabel(method)))") { merge() }
        } message: {
            Text(pr.summary)
        }
    }

    private func merge() {
        let (id, method) = (pr.id, method)
        Task { _ = await store.resolve(.pullRequest(pr)) { try await $0.mergePullRequest(id, method: method) } }
    }

    @ViewBuilder
    private var changes: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Changes")
                .font(.title3.weight(.semibold))
            if let files {
                Text("Click a line to comment on it")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                DiffView(files: files, comments: comments) { commenting = $0 }
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
        ReviewExport.text(comments: orderedComments(comments, in: files ?? []), general: note)
    }

    private func loadDiff() async {
        do {
            files = UnifiedDiff.parse(try await store.client.pullRequestDiff(pr.id))
        } catch {
            diffError = error.localizedDescription
        }
    }

    private func requestChanges() {
        let (id, text) = (pr.id, review)
        Task {
            if await store.resolve(.pullRequest(pr), { try await $0.requestChanges(onPullRequest: id, text: text) }) {
                note = ""
                comments = [:]
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
