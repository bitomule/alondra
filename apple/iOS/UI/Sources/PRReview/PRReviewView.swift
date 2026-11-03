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
        .toolbar { hostToolbar }
        .task { await store.send(.task).finish() }
        .navigationDestination(isPresented: Binding(get: { store.showsChanges }, set: { store.send(.changesShown($0)) })) {
            PRChangesView(files: store.diff.files, failed: store.diff.failed, truncated: store.diffTruncated,
                          collapsed: store.collapsed, comments: store.comments,
                          toggle: { store.send(.toggleFile($0)) }, toggleAll: { store.send(.toggleAllFiles) },
                          comment: { store.send(.comment(anchor: $0, $1)) }, retry: { store.send(.retryDiff) })
        }
        .sheet(isPresented: Binding(get: { store.isComposing }, set: { if !$0 { store.send(.dismissSheet) } })) { sheet }
        .actionDialogs(store.scope(state: \.actions, action: \.actions))
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
                if let description = review.description, !description.text.isEmpty {
                    PRDescription(markdown: description)
                }
                diff
            }
            .padding(16)
        }
        .scrollDismissesKeyboard(.interactively)
        .safeAreaBar(edge: .bottom) { PRActionBar(store: store, review: review) }
    }

    @ViewBuilder private var diff: some View {
        if store.diff != .unavailable {
            DiffSummaryCard(summary: store.diff.files.map(DiffSummary.init), pending: nil,
                            failed: store.diff.failed, open: { store.send(.changesShown(true)) }, retry: { store.send(.retryDiff) })
        }
    }

    @ToolbarContentBuilder private var hostToolbar: some ToolbarContent {
        if let url = store.review?.pageURL {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Open on GitHub", systemImage: "arrow.up.right.square") { openURL(url) }
            }
        }
    }

    private var sheet: some View {
        let actions = store.scope(state: \.actions, action: \.actions)
        return ActionComposerSheet(store: actions, title: actions.action?.label ?? "") {
            ActionInputView(store: actions, showCommentEditor: false)
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

struct PRDescription: View {
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
