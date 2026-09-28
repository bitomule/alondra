import AlondraShared
import AlondraWorkspaceFeature
import AlondraPullRequestsFeature
import ComposableArchitecture
import SwiftUI

struct ServicePullRequestView: View {
    let store: StoreOf<ServicePullRequestFeature>
    @Environment(\.colorScheme) private var scheme
    private var commenting: DiffLine? {
        store.diff.files?.flatMap(\.lines).first { $0.anchor != nil && $0.anchor == store.diff.editingAnchor }
    }
    var body: some View {
        reviewContent
        .navigationTitle("PR #\(store.displayedPR?.number ?? 0)")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .safeAreaInset(edge: .bottom, spacing: 0) { bar }
        .toolbar {
            if let files = store.diff.files, !files.isEmpty {
                ToolbarItem(placement: .topBarTrailing) {
                    let allCollapsed = store.diff.collapsed.count == files.count
                    Button(allCollapsed ? String(localized: "Expand All Files") : String(localized: "Collapse All Files"), systemImage: allCollapsed ? "rectangle.expand.vertical" : "rectangle.compress.vertical") {
                        withAnimation(.snappy) { _ = store.send(.diff(.toggleAllFiles)) }
                    }
                }
            }
        }
        .sheet(item: Binding(get: { commenting }, set: { if $0 == nil { _ = store.send(.diff(.cancelLineComment)) } })) { line in
            LineCommentSheet(line: line, initial: line.anchor.flatMap { store.diff.comments[$0] } ?? "", text: Binding(get: { store.diff.editingText }, set: { store.send(.diff(.lineEditorChanged($0))) })) { text in
                if let anchor = line.anchor { _ = store.send(.diff(.saveLineComment(anchor, text))) }
            }.presentationDetents([.medium, .large])
        }
        .task { await store.send(.diff(.appeared)).finish() }
        .onDisappear { _ = store.send(.diff(.disappeared)) }
    }
    private var reviewContent: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let pr = store.displayedPR {
                    Text(pr.title).font(.title3.weight(.bold))
                    Text(pr.summary).font(.footnote.monospaced()).foregroundStyle(.secondary)
                    PRStatus(pr: pr)
                    ForEach(store.reports) { AttachedReport(report: $0, showsBody: true) }
                    ForEach(store.scope(state: \.questionFeatures, action: \.attachedQuestion).filter { store.visibleQuestionIDs.contains($0.id) }) { child in
                        ServiceAttachedQuestion(store: child.scope(state: \.question, action: \.question), showsDetail: true)
                    }
                    if let body = pr.body, !body.isEmpty { ServicePRDescription(markdown: body, expanded: store.descriptionExpanded ?? (body.count <= 600)) { store.send(.toggleDescription) } }
                }
                if let files = store.diff.files {
                    Text("Tap a line to comment on it").font(.caption).foregroundStyle(.secondary)
                    if store.diff.diffTruncated {
                        Label("This diff is too long to show in full. Open the PR on GitHub for the rest.", systemImage: "scissors")
                            .font(.caption).foregroundStyle(.orange)
                    }
                    LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                        ForEach(files) { file in
                            Section {
                                if !store.diff.collapsed.contains(file.id) {
                                    ForEach(file.lines) { line in
                                        ServicePhoneDiffLine(line: line, coloured: store.diff.highlightedLines[line.id], comment: line.anchor.flatMap { store.diff.comments[$0] }) {
                                            if let anchor = $0.anchor { _ = store.send(.diff(.editLine(anchor))) }
                                        }
                                    }
                                }
                            } header: {
                                DiffFileHeader(file: file, comments: file.commentCount(in: store.diff.comments), collapsed: store.diff.collapsed.contains(file.id)) {
                                    withAnimation(.snappy) { _ = store.send(.diff(.toggleFile(file.id))) }
                                }
                            }
                        }
                    }
                    .task(id: scheme) { store.send(.diff(.colorSchemeChanged(dark: scheme == .dark))) }
                } else if let failure = store.diff.loadError {
                    Label(failure, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary)
                    Button("Try Again") { _ = store.send(.diff(.retryTapped)) }
                } else { ProgressView().frame(maxWidth: .infinity) }
            }.padding(16)
        }
    }

    private var bar: some View {
        VStack(spacing: 8) {
            ServiceAttachmentStrip(store: store.scope(state: \.diff.attachmentPicker, action: \.diff.attachmentPicker))
            HStack(spacing: 6) {
                ServiceAttachButton(store: store.scope(state: \.diff.attachmentPicker, action: \.diff.attachmentPicker))
                TextField("A note for the node (optional)", text: Binding(get: { store.diff.note }, set: { _ = store.send(.diff(.noteChanged($0))) }), axis: .vertical)
                    .lineLimit(1...4).textFieldStyle(.roundedBorder)
            }.disabled(store.diff.isSending || store.diff.mutationOutcome?.uncertainID != nil)
            HStack(spacing: 8) {
                Button { _ = store.send(.diff(.askTapped)) } label: {
                    Label("Ask", systemImage: "questionmark.bubble").frame(maxWidth: .infinity)
                }.buttonStyle(.glass).disabled(!store.diff.hasDraft || !store.diff.canReview)
                Button { _ = store.send(.diff(.requestChangesTapped)) } label: {
                    Label(store.diff.comments.isEmpty ? String(localized: "Request Changes") : String(localized: "Request Changes (\(store.diff.comments.count))"), systemImage: "arrow.uturn.backward").frame(maxWidth: .infinity)
                }.buttonStyle(.glassProminent).disabled(!store.diff.hasDraft || !store.diff.canReview)
            }
            Group {
                ServicePRMainAction(store: store.scope(state: \.pr, action: \.pr), placement: .bar(prominent: !store.diff.hasDraft))
                if let pr = store.displayedPR {
                    PRFooter(pr: pr) { _ = store.send(.pr(.setAsideTapped)) }.padding(.horizontal, 4)
                }
            }.disabled(store.diff.isSending || !store.pr.canAct)
            if store.diff.commentsNeedReview {
                Text("The diff changed. Check each comment before confirming.").font(.caption)
                Button("Comments match this diff") { _ = store.send(.diff(.confirmCommentsForCurrentDiff)) }
            }
            if let error = store.diff.validationError, !store.diff.validationErrorIsSubmissionFailure { Text(error).font(.caption).foregroundStyle(Palette.rose) }
            ServicePROutcome(outcome: store.pr.mutationOutcome)
            ServicePROutcome(outcome: store.diff.mutationOutcome)
        }.padding(.horizontal, 16).padding(.vertical, 10).background(.bar)
    }
}
private struct ServicePROutcome: View {
    let outcome: PRMutationOutcome?
    var body: some View {
        if let outcome {
            switch outcome {
            case .confirmed: EmptyView()
            case .closed: Text("This pull request is closed").font(.caption)
            case .rejected: EmptyView() // Root presents the original failure toast.
            case .unknown: Text("The result is unknown. Check Alondra before taking another action.").font(.caption).foregroundStyle(Palette.dawn)
            case .obsolete: Text("The connection changed. Reopen this pull request.").font(.caption)
            }
        }
    }
}

struct ServicePRDescription: View {
    static let foldedLength = 600

    let segments: [MarkdownSegment]
    let isLong: Bool
    let expanded: Bool
    let toggle: () -> Void

    init(markdown: String, expanded: Bool, toggle: @escaping () -> Void) {
        segments = MarkdownParser.segments(from: markdown)
        isLong = markdown.count > Self.foldedLength
        self.expanded = expanded
        self.toggle = toggle
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
                    withAnimation(.snappy) { toggle() }
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

struct ServicePhoneDiffLine: View {
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
