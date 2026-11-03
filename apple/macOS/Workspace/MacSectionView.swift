import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// A server section as the Mac lays it out: cards in the card anatomy, everything else as it comes.
struct MacSectionView: View {
    let section: ViewSection
    var returnReasons: [String: String] = [:]
    var onAction: (AlondraContract.Action) -> Void
    var onReply: (AlondraContract.Action, String) -> Void
    var onLink: (AlondraContract.Link) -> Void
    var onMore: (String) -> Void
    var onForm: (AlondraContract.Form) -> Void
    var onContent: (ContentRef) -> Void
    var onContentRow: () -> Void
    var onFilledForm: (AlondraContract.Action, RequestedForm, [String: JSONValue]) -> Void = { _, _, _ in }
    var diff: MacDiff?
    var diffFailed: String?
    var onDiffNeeded: (ContentRef) -> Void = { _ in }
    var opened = false
    var page: SectionPage?
    var onToggle: () -> Void = {}
    var onHeaderAction: (AlondraContract.Action) -> Void = { _ in }

    private var folded: Bool { section.collapsed && !opened }
    /// A folded section that came without its rows has them fetched when it is opened.
    private var fetched: Bool { section.collapsed && section.items.isEmpty && section.more != nil }
    private var items: [ViewItem] { fetched ? page?.items ?? [] : section.items }
    private var more: More? { fetched ? page?.more : section.more }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let title = section.title, section.id != "diff" {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    if section.collapsed && section.count != 0 {
                        Button {
                            withAnimation(.smooth, onToggle)
                        } label: {
                            HStack(spacing: 6) {
                                Text(verbatim: section.count.map { "\(title) (\($0))" } ?? title)
                                Image(systemName: "chevron.right")
                                    .rotationEffect(.degrees(opened ? 90 : 0))
                                    .accessibilityHidden(true)
                            }
                            .font(.headline)
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(.isHeader)
                        .accessibilityHint(opened ? Text("Hides them") : Text("Shows them"))
                    } else {
                        Text(title).font(.headline).accessibilityAddTraits(.isHeader)
                    }
                    if !section.actions.isEmpty {
                        Spacer(minLength: 0)
                        ForEach(Array(section.actions.enumerated()), id: \.offset) { _, action in
                            Button(action.label) { onHeaderAction(action) }
                                .disabled(!action.enabled)
                                .help(action.disabledReason ?? "")
                        }
                    }
                }
                if let reason = returnReasons[section.id] {
                    Text(reason).font(.callout).foregroundStyle(.red)
                }
            }
            if !folded {
                ForEach(identifiedItems, id: \.id) { _, item in
                    if case let .card(card) = item, card.need != nil {
                        MacCardView(card: card, returnReason: returnReasons[card.id], onAction: onAction, onReply: onReply, onLink: onLink, onContent: onContentRow, onFilledForm: onFilledForm)
                    } else if case let .actions(bar) = item {
                        MacActionBar(bar: bar, onAction: onAction)
                    } else if section.id == "documents", case let .facts(facts) = item {
                        MacDocumentLinks(facts: facts, onLink: onLink)
                    } else if case let .contentRef(reference) = item, reference.kind == .diff {
                        diffSummary(reference)
                    } else {
                        ViewItemRenderer(item: item, onAction: onAction, onLink: onLink, onForm: onForm, onContent: onContent)
                    }
                }
                if fetched && page == nil {
                    ProgressView().controlSize(.small)
                }
                if let more {
                    Button("Show \(more.remaining) more") { onMore(more.key) }
                        .font(.footnote.weight(.semibold))
                }
            }
        }
    }

    private func diffSummary(_ reference: ContentRef) -> some View {
        let identity = reference.digest ?? reference.fetch
        return DiffSummaryCard(summary: diff.flatMap { $0.digest == identity ? DiffSummary($0.files) : nil },
                               failed: diffFailed == identity, shortcut: KeyboardShortcut("d", modifiers: .command),
                               open: { onContent(reference) }, retry: { onDiffNeeded(reference) })
            .frame(maxWidth: 560, alignment: .leading)
            .task(id: identity) { onDiffNeeded(reference) }
    }

    /// A card is identified by its id, so a card that arrives above it in a refresh never takes over
    /// its reply draft or its keyboard focus.
    private var identifiedItems: [(id: String, item: ViewItem)] {
        items.enumerated().map { offset, item in
            if case let .card(card) = item { ("card:\(card.id)", item) } else { ("item:\(offset)", item) }
        }
    }
}
