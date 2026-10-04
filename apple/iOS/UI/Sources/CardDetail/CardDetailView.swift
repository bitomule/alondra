import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// Any card opened in full: what it needs and why, then the sections the daemon sends (content,
/// context, history), with the card's own actions and the reply field pinned at the bottom. A
/// document's text can be commented block by block before one of the actions sends the comments.
public struct CardDetailView: View {
    @Bindable var store: StoreOf<CardDetailFeature>
    @Environment(\.openURL) private var openURL
    @FocusState private var replyFocused: Bool
    @State private var moreShown = false
    @State private var methodChoice: AlondraContract.Action?

    public init(store: StoreOf<CardDetailFeature>) { self.store = store }

    private var kind: Kind { Kind(store.subject?.card?.kind) }

    public var body: some View {
        methodDialog(sheets(alerts(base)))
            .sensoryFeedback(.selection, trigger: store.composing?.anchor)
            .sensoryFeedback(.success, trigger: store.comments.count) { old, new in new > old }
    }

    private var base: some View {
        Group {
            if let subject = store.subject {
                reader(subject)
            } else {
                placeholder
            }
        }
        .background(DawnBackground(glow: kind.tint))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .task { await store.send(.task).finish() }
        .onDisappear { store.send(.screen(.stop)) }
    }

    private func methodDialog<Content: View>(_ view: Content) -> some View {
        view.confirmationDialog(
            methodChoice?.label ?? "", isPresented: Binding(get: { methodChoice != nil }, set: { if !$0 { methodChoice = nil } }),
            titleVisibility: .visible
        ) {
            ForEach(methodChoice?.options ?? [], id: \.id) { option in
                Button(option.label) {
                    if let action = methodChoice { store.send(.menuOptionPicked(action, optionID: option.id)) }
                }
            }
        }
    }

    private func sheets<Content: View>(_ view: Content) -> some View {
        view
            .sheet(item: Binding(get: { store.composing }, set: { if $0 == nil { store.send(.commentCancelled) } })) { composing in
                CommentSheet(store: store, composing: composing)
            }
            .sheet(item: Binding(get: { store.preview }, set: { if $0 == nil { store.send(.previewClosed) } })) { preview in
                AttachmentPreviewView(preview: preview)
            }
            .sheet(item: Binding(get: { store.composition }, set: { if $0 == nil { store.send(.compositionCancelled) } })) { composition in
                CompositionSheet(
                    composition: composition, tint: kind.tint,
                    reasonChanged: { store.send(.reasonChanged($0)) }, noteChanged: { store.send(.noteChanged($0)) },
                    cancel: { store.send(.compositionCancelled) }, send: { store.send(.compositionConfirmed) }
                )
            }
    }

    private func alerts<Content: View>(_ view: Content) -> some View {
        view
            .alert(store.actions.confirmation?.title ?? "", isPresented: Binding(
                get: { store.actions.confirmation != nil }, set: { if !$0 { store.send(.actions(.cancelConfirmation)) } }
            )) {
                Button(store.actions.confirmation?.button ?? String(localized: "Continue")) { store.send(.actions(.confirm)) }
                Button("Cancel", role: .cancel) { store.send(.actions(.cancelConfirmation)) }
            } message: { Text(store.actions.confirmation?.message ?? "") }
            .alert("This item changed", isPresented: Binding(
                get: { store.actions.changed != nil }, set: { if !$0 { store.send(.actions(.dismiss)) } }
            )) {
                Button(store.actions.changed?.button ?? String(localized: "Continue")) { store.send(.actions(.sendChangedAction)) }
                Button("Cancel", role: .cancel) { store.send(.actions(.dismiss)) }
            } message: { Text(store.actions.error ?? "") }
    }

    @ViewBuilder private var placeholder: some View {
        switch store.screen.phase {
        case .missing:
            ContentUnavailableView("This screen is no longer available", systemImage: "doc.questionmark")
        case .failed(let message):
            ContentUnavailableView {
                Label("Couldn’t open it", systemImage: "doc.questionmark")
            } description: {
                Text(message)
            } actions: {
                Button("Try Again") { store.send(.screen(.retry)) }.buttonStyle(.glass)
            }
        case .loading, .loaded:
            ScrollView { skeleton.padding(20) }
        }
    }

    private var skeleton: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(0..<6, id: \.self) { index in
                RoundedRectangle(cornerRadius: 6)
                    .fill(Palette.hairline)
                    .frame(height: 14)
                    .frame(maxWidth: index.isMultiple(of: 3) ? 220 : .infinity, alignment: .leading)
            }
        }
        .phaseAnimator([0.45, 1]) { view, opacity in view.opacity(opacity) } animation: { _ in .easeInOut(duration: 0.8) }
    }

    private func reader(_ subject: CardDetailFeature.Subject) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 28) {
                header(subject)
                ForEach(subject.sections, id: \.id) { section in
                    sectionView(section)
                }
                if store.canComment, store.content != nil || store.contentPending { commentsSummary }
            }
            .padding(.horizontal, 20).padding(.bottom, 24)
        }
        .scrollDismissesKeyboard(.interactively)
        .scrollEdgeEffectHidden(true, for: .bottom)
        .safeAreaBar(edge: .bottom) { actionBar(subject) }
    }

    private func header(_ subject: CardDetailFeature.Subject) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            if subject.card != nil { KindBadge(kind: kind) }
            Text(subject.title).font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
            if let why = subject.card?.why {
                Text(why).font(.body).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 6) {
                if let meta = subject.card?.meta { Text(meta).font(.footnote.weight(.medium)) }
                if let at = subject.card?.timestamps.first?.at, let since = relativeSince(at) { Text("· \(since)") }
            }
            .font(.footnote).foregroundStyle(.secondary)
            BlockChips(blocks: subject.card?.blocks ?? [], blockedBy: subject.card?.blockedBy ?? [], open: follow)
            if store.canComment, store.content != nil {
                Label("Tap a block to comment on it", systemImage: "hand.tap")
                    .font(.footnote.weight(.medium)).foregroundStyle(kind.tint).padding(.top, 4)
            }
        }
    }

    private func sectionView(_ section: ViewSection) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            if let title = section.title {
                Text(title).font(.footnote.weight(.bold)).textCase(.uppercase).kerning(0.6).foregroundStyle(.secondary)
            }
            ForEach(Array(section.items.enumerated()), id: \.offset) { _, item in
                itemView(item)
            }
        }
    }

    @ViewBuilder private func itemView(_ item: ViewItem) -> some View {
        switch item {
        case .markdown(let markdown):
            ArtifactLinks(text: markdown.text, tint: kind.tint)
            MarkdownView(markdown: markdown)
        case .facts(let facts):
            FactsView(facts: facts, onLink: follow)
        case .checks(let checks):
            ChecksView(checks: checks, onLink: follow)
        case .contentRef(let reference):
            contentReference(reference)
        case .row(let row):
            historyRow(row)
        case .notice(let notice):
            NoticeView(notice: notice)
        case .timeline(let timeline):
            TimelineView(timeline: timeline)
        case .card, .form, .actions:
            EmptyView()
        }
    }

    @ViewBuilder private func contentReference(_ reference: ContentRef) -> some View {
        switch reference.kind {
        case .document:
            documentBody
        case .diff:
            if let key = CardDetailFeature.Subject.diffKey(of: reference) {
                Button { store.send(.opened(key)) } label: {
                    HStack(spacing: 12) {
                        Image(systemName: "plusminus").font(.title3).foregroundStyle(kind.tint)
                        Text(reference.title).font(.headline).foregroundStyle(Palette.ink)
                        Spacer()
                        Image(systemName: "chevron.right").font(.footnote.weight(.bold)).foregroundStyle(.tertiary)
                    }
                    .padding(14)
                    .background(kind.tint.opacity(0.10), in: .rect(cornerRadius: 16, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(kind.tint.opacity(0.25)))
                }
                .buttonStyle(PressStyle())
            }
        case .file:
            EmptyView()
        }
    }

    private func historyRow(_ row: Row) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: row.icon == .check ? "checkmark.circle.fill" : "clock.arrow.circlepath")
                .foregroundStyle(row.icon == .check ? Palette.sage : .secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(row.title).font(.subheadline.weight(.semibold)).foregroundStyle(Palette.ink)
                if let subtitle = row.subtitle { Text(subtitle).font(.footnote).foregroundStyle(.secondary) }
            }
            Spacer(minLength: 0)
        }
        .padding(14).frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.card.opacity(0.9), in: .rect(cornerRadius: 16, style: .continuous))
    }

    @ViewBuilder private var documentBody: some View {
        if store.contentFailed {
            ContentUnavailableView {
                Label("Couldn’t open it", systemImage: "doc.questionmark")
            } description: {
                Text("Couldn’t load this from your Mac. Try again.")
            } actions: {
                Button("Try Again") { store.send(.screen(.retry)) }.buttonStyle(.glass)
            }
            .frame(maxWidth: .infinity).padding(.top, 40)
        } else if store.contentPending {
            skeleton
        } else {
            ArtifactLinks(text: store.markdown, tint: kind.tint)
            LazyVStack(alignment: .leading, spacing: 6) {
                ForEach(MarkdownParser.segments(store.blocks)) { segment in
                    if store.canComment {
                        switch segment {
                        case .block(let block):
                            BlockRow(block: block, comment: store.comments[block.draftKey]?.text, tint: kind.tint) {
                                store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText))
                            }
                        case .table(let rows):
                            TableRows(rows: rows, comments: store.comments.mapValues(\.text), tint: kind.tint) {
                                store.send(.blockTapped(anchor: $0.draftKey, quote: $0.plainText))
                            }
                        }
                    } else {
                        MarkdownSegmentView(segment: segment)
                    }
                }
            }
        }
    }

    private var commentsSummary: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("Your comments").font(.display(.title3))
                Spacer()
                if !store.feedback.isEmpty {
                    Text(store.feedback.count, format: .number)
                        .font(.subheadline.monospacedDigit().weight(.bold)).foregroundStyle(kind.tint)
                }
            }
            if store.feedback.isEmpty {
                Text("You haven’t commented on any block yet.")
                    .font(.subheadline).foregroundStyle(.secondary)
            }
            ForEach(store.blocks.filter { store.comments[$0.draftKey] != nil }) { block in
                Button { store.send(.blockTapped(anchor: block.draftKey, quote: block.plainText)) } label: {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("“\(ReviewFeedback.quote(block.plainText))”")
                            .font(.footnote.italic()).foregroundStyle(.secondary).lineLimit(3)
                        Text(store.comments[block.draftKey]?.text ?? "")
                            .font(.subheadline).foregroundStyle(Palette.ink)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading).padding(14)
                    .background(Palette.card, in: .rect(cornerRadius: 14))
                }
                .buttonStyle(.plain)
            }
        }
    }

    // MARK: The pinned actions

    private func actionBar(_ subject: CardDetailFeature.Subject) -> some View {
        GlassActionBar {
            if let error = store.actions.error { Text(error).font(.caption).foregroundStyle(Palette.rose) }
            ForEach(Array(subject.buttons.enumerated()), id: \.offset) { _, action in
                barButton(action)
            }
            if !subject.menu.isEmpty { moreMenu(subject.menu) }
            if let reply = subject.replyAction {
                AttachmentStrip(store: store.scope(state: \.attachments, action: \.attachments))
                composer(reply)
            }
        }
        .background(Palette.night)
        .overlay(alignment: .top) { Palette.hairline.frame(height: 1) }
    }

    @ViewBuilder private func barButton(_ action: AlondraContract.Action) -> some View {
        let disabled = !store.state.isSendable(action)
        VStack(spacing: 4) {
            if action.presentation == .menu, !action.options.isEmpty {
                Button { methodChoice = action } label: { Text(action.label) }
                    .buttonStyle(RoleButtonStyle(role: action.cardRole))
                    .disabled(disabled)
            } else {
                Button { perform(action) } label: { Text(action.label).multilineTextAlignment(.center) }
                    .buttonStyle(RoleButtonStyle(role: action.cardRole))
                    .disabled(disabled)
            }
            if !action.enabled, let reason = action.disabledReason {
                Text(reason).font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
        }
    }

    private func moreMenu(_ actions: [AlondraContract.Action]) -> some View {
        Button { moreShown = true } label: {
            Text("More… (\(actions.count))")
                .font(.subheadline.weight(.medium)).foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, minHeight: 32)
        }
        .buttonStyle(.plain)
        .confirmationDialog("More…", isPresented: $moreShown, titleVisibility: .hidden) {
            ForEach(Array(actions.enumerated()), id: \.offset) { _, action in
                Button(action.label) { perform(action) }.disabled(!store.state.isSendable(action))
            }
        }
    }

    private func composer(_ action: AlondraContract.Action) -> some View {
        let placeholder: String = {
            if case .text(let placeholder, _, _) = action.input, let placeholder { return placeholder }
            return String(localized: "Reply to the node…")
        }()
        let empty = !store.state.hasFeedback
        return HStack(alignment: .bottom, spacing: 8) {
            if action.attachments {
                AttachButton(store: store.scope(state: \.attachments, action: \.attachments)).padding(.leading, 6).padding(.bottom, 4)
            }
            TextField(placeholder, text: Binding(get: { store.reply }, set: { store.send(.replyChanged($0)) }), axis: .vertical)
                .lineLimit(1...4).focused($replyFocused)
                .padding(.leading, 16).padding(.vertical, 10)
            Button {
                replyFocused = false
                store.send(.actionTapped(action))
            } label: {
                Image(systemName: "arrow.up").font(.body.weight(.bold)).foregroundStyle(Palette.onAccent)
                    .frame(width: 34, height: 34).background(empty ? Palette.slate.opacity(0.5) : kind.tint, in: .circle)
            }
            .buttonStyle(PressStyle())
            .disabled(empty || !action.enabled)
            .padding(4)
            .accessibilityLabel("Send")
        }
        .glassEffect(.regular, in: .rect(cornerRadius: 24, style: .continuous))
    }

    private func perform(_ action: AlondraContract.Action) {
        if case .url(let url) = action.link, let destination = URL(string: url) {
            openURL(destination)
        } else {
            store.send(.actionTapped(action))
        }
    }

    private func follow(_ link: AlondraContract.Link) {
        switch link {
        case .view(let key): store.send(.opened(ViewKey(rawValue: key)))
        case .url(let url): if let destination = URL(string: url) { openURL(destination) }
        case .local: break
        }
    }
}
