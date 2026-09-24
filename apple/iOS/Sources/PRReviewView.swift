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
    @State private var attachments = Attachments()
    @State private var commenting: DiffLine?
    @State private var collapsed: Set<String> = []
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
                if let stackedOn = pr.stackedOn {
                    Label(stackedOn, systemImage: "square.stack.3d.up")
                        .font(.footnote.weight(.medium))
                        .foregroundStyle(.secondary)
                }
                ForEach(fleet.reports(forPullRequest: pr.id)) { report in
                    AttachedReport(report: report, showsBody: true)
                }
                ForEach(fleet.questions(forPullRequest: pr.id)) { question in
                    AttachedQuestion(question: question, showsDetail: true)
                }
                if let body = pr.body, !body.isEmpty {
                    PRDescription(markdown: body)
                }
                if let files {
                    Text("Tap a line to comment on it")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                        ForEach(files) { file in
                            Section {
                                if !collapsed.contains(file.id) {
                                    ForEach(file.lines) { line in
                                        PhoneDiffLine(line: line, coloured: colours.lines[line.id], comment: line.anchor.flatMap { comments[$0] }) { commenting = $0 }
                                    }
                                }
                            } header: {
                                DiffFileHeader(file: file, comments: file.commentCount(in: comments), collapsed: collapsed.contains(file.id)) {
                                    withAnimation(.snappy) { collapsed.formSymmetricDifference([file.id]) }
                                }
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
        .toolbar {
            if let files, !files.isEmpty {
                ToolbarItem(placement: .topBarTrailing) {
                    let allCollapsed = collapsed.count == files.count
                    Button(allCollapsed ? String(localized: "Expand All Files") : String(localized: "Collapse All Files"),
                           systemImage: allCollapsed ? "rectangle.expand.vertical" : "rectangle.compress.vertical") {
                        withAnimation(.snappy) {
                            collapsed = allCollapsed ? [] : Set(files.map(\.id))
                        }
                    }
                }
            }
        }
        .sheet(item: $commenting) { line in
            LineCommentSheet(line: line, initial: line.anchor.flatMap { comments[$0] } ?? "") { text in
                guard let anchor = line.anchor else { return }
                comments[anchor] = text.isEmpty ? nil : text
            }
            .presentationDetents([.medium, .large])
        }
        .task {
            do {
                let parsed = UnifiedDiff.parse(try await fleet.client.pullRequestDiff(pr.id))
                collapsed = Set(parsed.filter(\.startsCollapsed).map(\.id))
                files = parsed
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
            AttachmentStrip(attachments, client: fleet.client)
            HStack(spacing: 6) {
                AttachButton(attachments: attachments)
                TextField("A note for the node (optional)", text: $note, axis: .vertical)
                    .lineLimit(1...4)
                    .textFieldStyle(.roundedBorder)
            }
            Button {
                send()
            } label: {
                Label(comments.isEmpty ? String(localized: "Request Changes") : String(localized: "Request Changes (\(comments.count))"), systemImage: "arrow.uturn.backward")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.glassProminent)
            .disabled((review.isEmpty && attachments.isEmpty) || sending)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .background(.bar)
    }

    private func send() {
        let (id, text) = (pr.id, review)
        sending = true
        Task {
            let ok = await fleet.resolve(.pullRequest(pr)) { [attachments] client in
                try await client.requestChanges(onPullRequest: id, text: text, attachments: try await attachments.storedIDs(using: client))
            }
            sending = false
            if ok { dismiss() }
        }
    }
}

private struct PRDescription: View {
    static let foldedLength = 600

    let segments: [MarkdownSegment]
    let isLong: Bool
    @State private var expanded: Bool

    init(markdown: String) {
        segments = MarkdownParser.segments(from: markdown)
        isLong = markdown.count > Self.foldedLength
        _expanded = State(initialValue: markdown.count <= Self.foldedLength)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(segments) { segment in
                    MarkdownSegmentView(segment: segment)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .fixedSize(horizontal: false, vertical: true)
            .foregroundStyle(Palette.ink.opacity(0.9))
            .frame(maxWidth: .infinity, maxHeight: expanded ? nil : 180, alignment: .topLeading)
            .clipped()
            .mask {
                LinearGradient(stops: expanded
                    ? [.init(color: .black, location: 0), .init(color: .black, location: 1)]
                    : [.init(color: .black, location: 0.5), .init(color: .clear, location: 1)],
                    startPoint: .top, endPoint: .bottom)
            }
            if isLong {
                Button {
                    withAnimation(.snappy) { expanded.toggle() }
                } label: {
                    Label(expanded ? String(localized: "Show Less") : String(localized: "Show More"),
                          systemImage: expanded ? "chevron.up" : "chevron.down")
                        .font(.subheadline.weight(.medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Palette.sage)
            }
        }
        .padding(14)
        .background(Palette.hairline.opacity(0.5), in: .rect(cornerRadius: 14))
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
