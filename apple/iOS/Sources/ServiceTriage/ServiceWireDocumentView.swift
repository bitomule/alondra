import AlondraShared
import AlondraViews
import AlondraViewsUI
import SwiftUI

/// The Views document uses the same reader and block-comment controls as the
/// previous document screen. The server supplies content and permitted actions.
struct ServiceWireDocumentView: View {
    let card: AlondraViews.Card
    let onAction: (AlondraViews.Action, [ActionComment], String) -> Void
    let onLink: (AlondraViews.Link) -> Void
    let onAttachment: (AlondraViews.Attachment) -> Void

    @State private var comments: [String: ActionComment] = [:]
    @State private var composing: MarkdownBlock?
    @State private var composingText = ""
    @State private var general = ""
    @FocusState private var generalFocused: Bool

    private var kind: Kind { Kind(card) }
    private var blocks: [MarkdownBlock] { MarkdownParser.blocks(from: card.body?.text ?? "") }
    private var canComment: Bool { card.actions.contains { $0.comments?.kind == .blocks } }
    private var feedback: [ActionComment] {
        blocks.compactMap { comments[$0.draftKey] }
    }
    private var hasFeedback: Bool {
        !feedback.isEmpty || !general.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                header.padding(.bottom, 24)
                LazyVStack(alignment: .leading, spacing: 6) {
                    ForEach(MarkdownParser.segments(blocks)) { segment in
                        if canComment {
                            switch segment {
                            case .block(let block):
                                ServiceBlockRow(block: block, comment: comments[block.draftKey]?.text,
                                                tint: kind.tint) { beginComment(block) }
                            case .table(let rows):
                                ServiceTableRows(rows: rows, comments: comments.mapValues(\.text),
                                                 tint: kind.tint, onTap: beginComment)
                            }
                        } else {
                            MarkdownSegmentView(segment: segment)
                        }
                    }
                }
                ForEach(card.attachments, id: \.id) { attachment in
                    Button { onAttachment(attachment) } label: {
                        Label(attachment.name, systemImage: "doc.text.magnifyingglass")
                            .font(.subheadline.weight(.medium))
                            .frame(maxWidth: .infinity, alignment: .leading).padding(14)
                            .background(Palette.card, in: .rect(cornerRadius: 14))
                    }
                    .buttonStyle(.plain)
                }
                if let facts = card.facts { FactsView(facts: facts, onLink: onLink).padding(.top, 12) }
                if let checks = card.checks { ChecksView(checks: checks, onLink: onLink).padding(.top, 12) }
                ForEach(card.children, id: \.id) { child in
                    CardView(card: child, onAction: { onAction($0, feedback, general) },
                             onLink: onLink, onAttachment: onAttachment)
                }
                if canComment { commentsSummary.padding(.top, 32) }
            }
            .padding(.horizontal, 20).padding(.bottom, 24)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(DawnBackground(glow: kind.tint))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .safeAreaBar(edge: .bottom) { actionBar }
        .sheet(item: $composing) { block in
            ServiceCommentSheet(block: block, initial: comments[block.draftKey]?.text ?? "",
                                text: $composingText) { value in
                if value.isEmpty { comments.removeValue(forKey: block.draftKey) }
                else { comments[block.draftKey] = ActionComment(anchor: block.draftKey,
                                                                 text: value, quote: block.plainText) }
            }
        }
        .sensoryFeedback(.selection, trigger: composing?.id)
        .sensoryFeedback(.success, trigger: comments.count) { old, new in new > old }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 12) {
            KindBadge(kind: kind)
            Text(card.title).font(.display(.largeTitle, weight: .bold))
                .foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 6) {
                if let node = card.context.first(where: { $0.icon == .node }) {
                    Text(node.text).font(.mono(.footnote, weight: .medium))
                }
                if let at = card.timestamps.first?.at {
                    Text(relativeStamp(at))
                }
            }
            .font(.footnote).foregroundStyle(.secondary)
            if canComment, !blocks.isEmpty {
                Label("Tap a block to comment on it", systemImage: "hand.tap")
                    .font(.footnote.weight(.medium)).foregroundStyle(kind.tint).padding(.top, 4)
            }
        }
    }

    private var commentsSummary: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("Your comments").font(.display(.title3))
                Spacer()
                if !feedback.isEmpty {
                    Text(feedback.count, format: .number)
                        .font(.subheadline.monospacedDigit().weight(.bold)).foregroundStyle(kind.tint)
                }
            }
            if feedback.isEmpty {
                Text("You haven’t commented on any block yet.")
                    .font(.subheadline).foregroundStyle(.secondary)
            }
            ForEach(blocks.filter { comments[$0.draftKey] != nil }) { block in
                Button { beginComment(block) } label: {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("“\(ReviewFeedback.quote(block.plainText))”")
                            .font(.footnote.italic()).foregroundStyle(.secondary).lineLimit(3)
                        Text(comments[block.draftKey]?.text ?? "")
                            .font(.subheadline).foregroundStyle(Palette.ink)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading).padding(14)
                    .background(Palette.card, in: .rect(cornerRadius: 14))
                }
                .buttonStyle(.plain)
            }
            TextField("General comment (optional)", text: $general, axis: .vertical)
                .lineLimit(2...8).focused($generalFocused).padding(14)
                .background(Palette.card, in: .rect(cornerRadius: 14))
                .overlay(RoundedRectangle(cornerRadius: 14)
                    .strokeBorder(generalFocused ? kind.tint.opacity(0.6) : Palette.hairline))
        }
    }

    private var actionBar: some View {
        GlassEffectContainer(spacing: 12) {
            HStack(spacing: 12) {
                ForEach(card.actions, id: \.label) { action in
                    ServiceWireActionButton(action: action, tint: kind.tint,
                                            disabled: !action.enabled || (action.comments != nil && !hasFeedback)) {
                        if let link = action.link { onLink(link) }
                        else { onAction(action, feedback, general) }
                    }
                }
            }
            .controlSize(.large).buttonBorderShape(.capsule)
            .font(.headline).lineLimit(1).minimumScaleFactor(0.75)
        }
        .padding(.horizontal, 20).padding(.vertical, 8)
    }

    private func beginComment(_ block: MarkdownBlock) {
        composingText = comments[block.draftKey]?.text ?? ""
        composing = block
    }

    private func relativeStamp(_ value: String) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        guard let date = formatter.date(from: value) ?? ISO8601DateFormatter().date(from: value) else {
            return value
        }
        return relative(Int64(date.timeIntervalSince1970 * 1_000))
    }
}

private struct ServiceWireActionButton: View {
    let action: AlondraViews.Action
    let tint: Color
    let disabled: Bool
    let onTap: () -> Void

    var body: some View {
        if action.style == .primary {
            Button(action.label, action: onTap)
                .frame(maxWidth: .infinity)
                .buttonStyle(.glassProminent)
                .tint(Palette.sageDeep)
                .disabled(disabled)
        } else {
            Button(action.label, action: onTap)
                .frame(maxWidth: .infinity)
                .buttonStyle(.glass)
                .tint(tint)
                .disabled(disabled)
        }
    }
}
