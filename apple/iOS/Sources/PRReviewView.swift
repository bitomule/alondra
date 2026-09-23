import AlondraShared
import SwiftUI

/// A PR's diff on the phone: tap a line to comment on it; the comments and a note go back to the
/// node as one change request.
struct PRReviewView: View {
    let pr: PullRequest
    @Environment(FleetStore.self) private var fleet
    @Environment(\.dismiss) private var dismiss
    @State private var files: [DiffFile]?
    @State private var failure: String?
    @State private var comments: [String: String] = [:]
    @State private var note = ""
    @State private var commenting: DiffLine?
    @State private var sending = false
    @State private var colours = DiffColours()
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(pr.title)
                    .font(.title3.weight(.bold))
                Text(pr.summary)
                    .font(.footnote.monospaced())
                    .foregroundStyle(.secondary)
                ForEach(fleet.questions(forPullRequest: pr.id)) { question in
                    AttachedQuestion(question: question, showsDetail: true)
                }
                if let files {
                    Text("Tap a line to comment on it")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                        ForEach(files) { file in
                            Section {
                                ForEach(file.lines) { line in
                                    PhoneDiffLine(line: line, coloured: colours.lines[line.id], comment: line.anchor.flatMap { comments[$0] }) { commenting = $0 }
                                }
                            } header: {
                                DiffFileHeader(file: file)
                            }
                        }
                    }
                    .task(id: scheme) { await colours.load(files, dark: scheme == .dark) }
                } else if let failure {
                    Label(failure, systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.secondary)
                } else {
                    ProgressView()
                        .frame(maxWidth: .infinity)
                }
            }
            .padding(16)
        }
        .navigationTitle("PR #\(pr.number)")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .safeAreaInset(edge: .bottom, spacing: 0) { bar }
        .sheet(item: $commenting) { line in
            LineCommentSheet(line: line, initial: line.anchor.flatMap { comments[$0] } ?? "") { text in
                guard let anchor = line.anchor else { return }
                comments[anchor] = text.isEmpty ? nil : text
            }
            .presentationDetents([.medium])
        }
        .task {
            do {
                files = UnifiedDiff.parse(try await fleet.client.pullRequestDiff(pr.id))
            } catch {
                failure = error.localizedDescription
            }
        }
    }

    private var review: String {
        ReviewExport.text(comments: ReviewExport.ordered(comments, in: files ?? []), general: note)
    }

    private var bar: some View {
        VStack(spacing: 8) {
            TextField("A note for the node (optional)", text: $note, axis: .vertical)
                .lineLimit(1...4)
                .textFieldStyle(.roundedBorder)
            Button {
                send()
            } label: {
                Label(comments.isEmpty ? String(localized: "Request Changes") : String(localized: "Request Changes (\(comments.count))"), systemImage: "arrow.uturn.backward")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.glassProminent)
            .disabled(review.isEmpty || sending)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(.bar)
    }

    private func send() {
        let (id, text) = (pr.id, review)
        sending = true
        Task {
            let ok = await fleet.resolve(.pullRequest(pr)) { try await $0.requestChanges(onPullRequest: id, text: text) }
            sending = false
            if ok { dismiss() }
        }
    }
}

private struct PhoneDiffLine: View {
    let line: DiffLine
    let coloured: AttributedString?
    let comment: String?
    let onSelect: (DiffLine) -> Void

    var body: some View {
        Button {
            onSelect(line)
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                DiffCodeLine(line: line, coloured: coloured)
                if let comment {
                    Label(comment, systemImage: "text.bubble.fill")
                        .font(.caption)
                        .padding(8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(Color.accentColor.opacity(0.12))
                }
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .disabled(line.anchor == nil)
    }
}
