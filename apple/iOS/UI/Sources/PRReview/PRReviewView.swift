import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// A pull request on the phone: what it changes and where it stands, the diff to comment on, and
/// the bar to merge, hand conflicts back, ask, or request changes.
public struct PRReviewView: View {
    @Bindable var store: StoreOf<PRReviewFeature>
    @Environment(\.openURL) private var openURL

    public init(store: StoreOf<PRReviewFeature>) { self.store = store }

    public var body: some View {
        Group {
            if let review = store.review {
                content(review)
            } else {
                placeholder
            }
        }
        .background(DawnBackground(glow: Palette.dawn))
        .navigationTitle(store.review.map { Text(verbatim: $0.title) } ?? Text("Pull Request"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .toolbar { foldToolbar }
        .task { await store.send(.task).finish() }
        .sheet(isPresented: Binding(get: { store.isComposing }, set: { if !$0 { store.send(.dismissSheet) } })) { sheet }
        .sheet(item: Binding(get: { store.attachment }, set: { if $0 == nil { store.send(.closeAttachment) } })) { preview in
            AttachmentPreviewView(preview: preview)
        }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
        .alert("Could not open attachment", isPresented: Binding(
            get: { store.failure != nil },
            set: { if !$0 { store.send(.dismissFailure) } }
        )) {
            Button("OK") { store.send(.dismissFailure) }
        } message: {
            Text(store.failure ?? "")
        }
    }

    @ViewBuilder private var placeholder: some View {
        switch store.screen.phase {
        case .missing:
            ContentUnavailableView("This pull request is no longer available", systemImage: "questionmark.circle")
        case .failed(let message):
            ContentUnavailableView {
                Label(message, systemImage: "exclamationmark.triangle")
            } actions: {
                Button("Try Again") { store.send(.screen(.retry)) }
            }
        case .loading, .loaded:
            ProgressView().tint(Palette.dawn).frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func content(_ review: PRReview) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(review.title)
                    .font(.title3.weight(.bold)).foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
                ForEach(review.notices, id: \.text) { NoticeView(notice: $0) }
                if !review.facts.isEmpty { PRFacts(facts: review.facts) }
                PRStatusView(review: review)
                ForEach(review.attached, id: \.id) { card in
                    AttachedCardView(card: card,
                                     perform: { store.send(.perform($0)) },
                                     open: { store.send(.follow($0)) },
                                     showAttachment: { store.send(.showAttachment($0)) })
                }
                if let description = review.description, !description.text.isEmpty {
                    PRDescription(markdown: description)
                }
                diff
            }
            .padding(16)
        }
        .scrollDismissesKeyboard(.interactively)
        .safeAreaInset(edge: .bottom, spacing: 0) { PRActionBar(store: store, review: review, openURL: openURL) }
    }

    @ViewBuilder private var diff: some View {
        switch store.diff {
        case .unavailable:
            EmptyView()
        case .loading:
            ProgressView().frame(maxWidth: .infinity).padding()
        case .failed(let message):
            VStack(spacing: 8) {
                Label(message, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary)
                Button("Try Again") { store.send(.retryDiff) }
            }
            .frame(maxWidth: .infinity)
        case .loaded(let files):
            VStack(alignment: .leading, spacing: 12) {
                Text("Diff").font(.title3.weight(.bold)).foregroundStyle(Palette.ink)
                Text("Tap a line to comment on it").font(.caption).foregroundStyle(.secondary)
                PRDiffView(files: files, collapsed: store.collapsed, comments: store.comments,
                           toggle: { store.send(.toggleFile($0)) },
                           comment: { store.send(.comment(anchor: $0, $1)) })
                    .id(store.diffDigest)
            }
        }
    }

    @ToolbarContentBuilder private var foldToolbar: some ToolbarContent {
        if case .loaded(let files) = store.diff, !files.isEmpty {
            ToolbarItem(placement: .topBarTrailing) {
                let allCollapsed = store.collapsed.count == files.count
                Button(allCollapsed ? "Expand All Files" : "Collapse All Files",
                       systemImage: allCollapsed ? "rectangle.expand.vertical" : "rectangle.compress.vertical") {
                    withAnimation(.snappy) { _ = store.send(.toggleAllFiles) }
                }
            }
        }
    }

    private var sheet: some View {
        ActionInputView(store: store.scope(state: \.actions, action: \.actions), showCommentEditor: false)
            .padding()
            .presentationDetents([.medium, .large])
            .safeAreaInset(edge: .bottom) {
                Button("Send") { store.send(.actions(.submit)) }
                    .buttonStyle(.borderedProminent).tint(Palette.dawn)
                    .disabled(store.actions.inFlight)
                    .padding()
            }
    }
}

private struct PRFacts: View {
    let facts: [Fact]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(facts.enumerated()), id: \.offset) { _, fact in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(fact.label).font(.caption).foregroundStyle(.secondary)
                    Spacer(minLength: 8)
                    Text(fact.value).font(.mono(.footnote)).foregroundStyle(Palette.ink)
                        .multilineTextAlignment(.trailing)
                }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 16, style: .continuous))
    }
}

/// What stands between the PR and its merge.
private struct PRStatusView: View {
    let review: PRReview

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if review.resolveConflicts != nil {
                Label("It has conflicts with its base branch", systemImage: "exclamationmark.triangle.fill")
                    .font(.footnote.weight(.medium)).foregroundStyle(Palette.dawn)
            }
            if let merge = review.merge, !merge.enabled, let reason = merge.disabledReason {
                Label(reason, systemImage: "square.stack.3d.up")
                    .font(.footnote.weight(.medium)).foregroundStyle(.secondary)
            }
            ForEach(Array(review.checks.enumerated()), id: \.offset) { _, check in
                Label(check.name, systemImage: symbol(check.state))
                    .font(.footnote).foregroundStyle(tint(check.state))
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func symbol(_ state: CheckState) -> String {
        switch state {
        case .pass: "checkmark.circle.fill"
        case .fail: "xmark.circle.fill"
        case .pending: "clock"
        }
    }

    private func tint(_ state: CheckState) -> Color {
        switch state {
        case .pass: Palette.sage
        case .fail: Palette.rose
        case .pending: Palette.dawn
        }
    }
}

private struct PRDescription: View {
    static let foldedLength = 600

    let markdown: Markdown
    @State private var expanded: Bool

    init(markdown: Markdown) {
        self.markdown = markdown
        _expanded = State(initialValue: markdown.text.count <= Self.foldedLength)
    }

    private var isLong: Bool { markdown.text.count > Self.foldedLength }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            MarkdownView(markdown: markdown)
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
                    Label(expanded ? "Show Less" : "Show More", systemImage: expanded ? "chevron.up" : "chevron.down")
                        .font(.subheadline.weight(.medium))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Palette.sage)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.hairline.opacity(0.5), in: .rect(cornerRadius: 14))
    }
}
