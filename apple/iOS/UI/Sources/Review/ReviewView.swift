import AlondraAttachments
import AlondraContract
import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// A document, a delivery or any card, read in full: tap a block to comment on it, then send the
/// comments with one of the actions the server allows.
public struct ReviewView: View {
    @Bindable var store: StoreOf<ReviewFeature>
    @Environment(\.openURL) private var openURL
    @FocusState private var generalFocused: Bool

    public init(store: StoreOf<ReviewFeature>) { self.store = store }

    public var body: some View {
        Group {
            if let subject = store.subject {
                reader(subject)
            } else {
                placeholder
            }
        }
        .background(DawnBackground(glow: store.subject.map { Kind(icon: $0.icon).tint } ?? Palette.dawn))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .task { await store.send(.task).finish() }
        .onDisappear { store.send(.screen(.stop)) }
        .sheet(item: Binding(get: { store.composing }, set: { if $0 == nil { store.send(.commentCancelled) } })) { composing in
            CommentSheet(store: store, composing: composing)
        }
        .sheet(item: Binding(get: { store.preview }, set: { if $0 == nil { store.send(.previewClosed) } })) { preview in
            AttachmentPreviewView(preview: preview)
        }
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
        .sensoryFeedback(.selection, trigger: store.composing?.anchor)
        .sensoryFeedback(.success, trigger: store.comments.count) { old, new in new > old }
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

    private func reader(_ subject: ReviewFeature.Subject) -> some View {
        let kind = Kind(icon: subject.icon)
        return ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                header(subject, kind: kind).padding(.bottom, 24)
                ArtifactLinks(text: store.markdown, tint: kind.tint)
                content(kind: kind)
                if let options = subject.options {
                    optionButtons(options, kind: kind).padding(.top, 28)
                }
                ForEach(subject.attachments, id: \.id) { attachment in
                    Button { store.send(.attachmentTapped(attachment)) } label: {
                        Label(attachment.name, systemImage: "doc.text.magnifyingglass")
                            .font(.subheadline.weight(.medium))
                            .frame(maxWidth: .infinity, alignment: .leading).padding(14)
                            .background(Palette.card, in: .rect(cornerRadius: 14))
                    }
                    .buttonStyle(.plain)
                }
                if let facts = subject.facts { FactsView(facts: facts, onLink: follow).padding(.top, 12) }
                if let checks = subject.checks { ChecksView(checks: checks, onLink: follow).padding(.top, 12) }
                ForEach(subject.children, id: \.id) { child in
                    CardView(card: child, onAction: { store.send(.actionTapped($0)) },
                             onLink: follow, onAttachment: { store.send(.attachmentTapped($0)) })
                }
                if store.acceptsFeedback, !store.contentPending { commentsSummary(kind: kind).padding(.top, 32) }
            }
            .padding(.horizontal, 20).padding(.bottom, 24)
        }
        .scrollDismissesKeyboard(.interactively)
        .safeAreaBar(edge: .bottom) { actionBar(subject, kind: kind) }
    }

    private func header(_ subject: ReviewFeature.Subject, kind: Kind) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            KindBadge(kind: kind)
            Text(subject.title).font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 6) {
                if let node = subject.nodeName { Text(node).font(.mono(.footnote, weight: .medium)) }
                if let at = subject.createdAt, let since = relativeSince(at) { Text(since) }
            }
            .font(.footnote).foregroundStyle(.secondary)
            if store.acceptsFeedback, !store.blocks.isEmpty {
                Label("Tap a block to comment on it", systemImage: "hand.tap")
                    .font(.footnote.weight(.medium)).foregroundStyle(kind.tint).padding(.top, 4)
            }
        }
    }

    @ViewBuilder private func content(kind: Kind) -> some View {
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
            LazyVStack(alignment: .leading, spacing: 6) {
                ForEach(MarkdownParser.segments(store.blocks)) { segment in
                    if store.acceptsFeedback {
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

    private func optionButtons(_ action: AlondraContract.Action, kind: Kind) -> some View {
        VStack(spacing: 8) {
            ForEach(action.options, id: \.id) { option in
                Button { store.send(.optionPicked(action, optionID: option.id)) } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(option.label).font(.headline)
                        if let description = option.description {
                            Text(description).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .frame(maxWidth: .infinity, minHeight: 46, alignment: .leading)
                    .padding(.horizontal, 6)
                }
                .buttonStyle(OptionPillStyle(tint: kind.tint, picked: false))
                .disabled(store.actions.inFlight)
            }
            if let error = store.actions.error {
                Text(error).font(.caption).foregroundStyle(Palette.rose)
            }
        }
    }

    private func commentsSummary(kind: Kind) -> some View {
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
            TextField("General comment (optional)", text: Binding(
                get: { store.general }, set: { store.send(.generalChanged($0)) }
            ), axis: .vertical)
                .lineLimit(2...8).focused($generalFocused).padding(14)
                .background(Palette.card, in: .rect(cornerRadius: 14))
                .overlay(RoundedRectangle(cornerRadius: 14)
                    .strokeBorder(generalFocused ? kind.tint.opacity(0.6) : Palette.hairline))
            HStack(alignment: .center, spacing: 8) {
                AttachButton(store: store.scope(state: \.attachments, action: \.attachments))
                AttachmentStrip(store: store.scope(state: \.attachments, action: \.attachments))
            }
        }
    }

    private func actionBar(_ subject: ReviewFeature.Subject, kind: Kind) -> some View {
        GlassEffectContainer(spacing: 12) {
            HStack(spacing: 12) {
                ForEach(subject.barActions, id: \.label) { action in
                    barButton(action, subject: subject, kind: kind)
                }
            }
            .controlSize(.large).buttonBorderShape(.capsule)
            .font(.headline).lineLimit(1).minimumScaleFactor(0.75)
        }
        .padding(.horizontal, 20).padding(.vertical, 8)
    }

    @ViewBuilder private func barButton(
        _ action: AlondraContract.Action, subject: ReviewFeature.Subject, kind: Kind
    ) -> some View {
        let tap = {
            if case .url(let url) = action.link, let destination = URL(string: url) { openURL(destination) }
            else { store.send(.actionTapped(action)) }
        }
        let disabled = !store.state.isSendable(action) || store.actions.inFlight
        if action.style == .primary {
            Button(action: tap) {
                if ReviewFeature.Subject.verb(action) == "accept" { Label(action.label, systemImage: "checkmark") }
                else { Text(action.label) }
            }
            .frame(maxWidth: .infinity)
            .buttonStyle(.glassProminent)
            .tint(Palette.sageDeep)
            .disabled(disabled)
        } else {
            Button(action: tap) {
                if subject.isRescue(action) { Label("Rescue Comments", systemImage: "text.bubble") }
                else if ReviewFeature.Subject.verb(action) == "changes" {
                    Label(action.label, systemImage: "arrow.uturn.backward")
                } else { Text(action.label) }
            }
            .frame(maxWidth: .infinity)
            .buttonStyle(.glass)
            .tint(kind.tint)
            .disabled(disabled)
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
