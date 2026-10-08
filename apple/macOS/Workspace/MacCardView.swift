import AlondraContract
import AlondraDesign
import AlondraFeatures
import AppKit
import SwiftUI

enum MacCardPalette {
    static func kind(_ kind: CardKind?) -> Color {
        switch kind {
        case .problem: adaptive(light: 0x8F6B12, dark: 0xD6B35A)
        case .authorize: adaptive(light: 0xB8394D, dark: 0xD9697A)
        case .do: adaptive(light: 0x2F6FA8, dark: 0x6EA3D8)
        case .decide: adaptive(light: 0xB5601F, dark: 0xE8935A)
        case .review, .test: adaptive(light: 0x3F7A55, dark: 0x7FA98B)
        case .provide: adaptive(light: 0x7A5BB5, dark: 0xA58BD6)
        case .propose: adaptive(light: 0x7A746A, dark: 0xC9C3BA)
        case .know, nil: adaptive(light: 0x6B717C, dark: 0x9AA0AA)
        }
    }

    static let advance = adaptive(light: 0x2F7D4A, dark: 0x5E9C70)
    static let onAdvance = adaptive(light: 0xFFFFFF, dark: 0x0F1612)
    static let risk = adaptive(light: 0xB8394D, dark: 0xD9697A)
    static let onRisk = adaptive(light: 0xFFFFFF, dark: 0x1A0E10)
    static let neutralBorder = adaptive(light: 0x000000, dark: 0xFFFFFF, lightAlpha: 0.22, darkAlpha: 0.26)
    static let surface = adaptive(light: 0xFFFDFA, dark: 0x1C1E27)
    static let hairline = adaptive(light: 0x000000, dark: 0xFFFFFF, lightAlpha: 0.09, darkAlpha: 0.10)
    static let inset = adaptive(light: 0x000000, dark: 0xFFFFFF, lightAlpha: 0.04, darkAlpha: 0.06)

    static func tone(_ tone: Tone) -> Color {
        switch tone {
        case .neutral: .secondary
        case .info: adaptive(light: 0x2F6FA8, dark: 0x6EA3D8)
        case .accent: adaptive(light: 0xB5601F, dark: 0xE8935A)
        case .success: adaptive(light: 0x3F7A55, dark: 0x7FA98B)
        case .warning, .yourTurn: adaptive(light: 0xB5601F, dark: 0xE8935A)
        case .critical: adaptive(light: 0xB8394D, dark: 0xD9697A)
        }
    }

    private static func adaptive(light: UInt32, dark: UInt32, lightAlpha: Double = 1, darkAlpha: Double = 1) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            let hex = isDark ? dark : light
            return NSColor(
                srgbRed: CGFloat((hex >> 16) & 255) / 255, green: CGFloat((hex >> 8) & 255) / 255,
                blue: CGFloat(hex & 255) / 255, alpha: isDark ? darkAlpha : lightAlpha)
        })
    }
}

extension AlondraContract.Action {
    public var resolvedRole: ActionRole { paintedRole }
}

/// How many buttons a card shows before the rest go behind "More…".
public let macCardVisibleButtons = 3

/// What a card shows of its buttons: the first three in order, and everything else behind "More…".
public struct MacCardButtons: Equatable {
    public let visible: [AlondraContract.Action]
    public let more: [AlondraContract.Action]

    public init(actions: [AlondraContract.Action], more extra: [AlondraContract.Action], excluding hidden: Set<String> = []) {
        let live = actions.filter { $0.id.map(hidden.contains) != true }
        visible = Array(live.prefix(macCardVisibleButtons))
        more = Array(live.dropFirst(macCardVisibleButtons)) + extra
    }

    public init(_ card: Card, excluding hidden: Set<String> = []) {
        self.init(actions: card.usableActions, more: card.usableMore, excluding: hidden)
    }
}

/// One card as the Mac shows it: the kind, what is needed, why, one line of content, what it holds
/// back, the buttons in a column and a reply field. Everything longer is in the detail below it.
struct MacCardView: View {
    let card: Card
    var excludedActionIDs: Set<String> = []
    /// Why the card is back: the last answer to it did not go through.
    var returnReason: String?
    var onAction: (AlondraContract.Action) -> Void
    var onReply: (AlondraContract.Action, String) -> Void
    var onLink: (AlondraContract.Link) -> Void
    var onContent: () -> Void

    @State private var reply = ""
    @State private var moreOpen = false
    @FocusState private var replyFocused: Bool

    private var buttons: MacCardButtons { MacCardButtons(card, excluding: excludedActionIDs) }
    private var kindColor: Color { MacCardPalette.kind(card.kind) }
    private var kindLabel: String { card.eyebrow?.text ?? card.title }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            VStack(alignment: .leading, spacing: 6) {
                Text(card.need ?? card.title).font(.title3.weight(.semibold)).fixedSize(horizontal: false, vertical: true)
                if let why = card.why { Text(why).font(.body).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true) }
                if let meta = card.meta { Text(meta).font(.caption).foregroundStyle(.tertiary) }
            }
            if let row = card.contentRow { contentRow(row) }
            if !card.blocks.isEmpty || !card.blockedBy.isEmpty { chips }
            MacActionColumn(buttons: buttons, moreOpen: $moreOpen, onAction: onAction)
            if let returnReason {
                Text(returnReason).font(.callout).foregroundStyle(MacCardPalette.risk).fixedSize(horizontal: false, vertical: true)
            }
            if let action = card.reply { replyField(action) }
        }
        .padding(18)
        .frame(maxWidth: 640, alignment: .leading)
        .background(MacCardPalette.surface, in: .rect(cornerRadius: 18, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(MacCardPalette.hairline))
        .background {
            if card.detail == .included {
                MacShortcuts(buttons: buttons, moreOpen: $moreOpen, onAction: onAction, reply: card.reply != nil ? { replyFocused = true } : nil)
                    .disabled(replyFocused)
            }
        }
        .accessibilityElement(children: .contain)
    }

    private var header: some View {
        HStack(spacing: 8) {
            Circle().fill(kindColor).frame(width: 8, height: 8)
            Text(kindLabel).font(.caption.weight(.bold)).kerning(1).textCase(.uppercase).foregroundStyle(kindColor)
            Spacer(minLength: 8)
            if let state = card.state {
                Text(state.text).font(.caption).foregroundStyle(MacCardPalette.tone(state.tone)).lineLimit(1)
            }
        }
    }

    private func contentRow(_ row: ContentRow) -> some View {
        Button(action: onContent) {
            HStack(spacing: 10) {
                Image(systemName: Self.symbol(forContent: row)).foregroundStyle(.secondary).frame(width: 18)
                Text(row.label).font(.subheadline.weight(.semibold))
                Text(row.summary).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
                if row.moreCount > 0 {
                    Text("+\(row.moreCount)").font(.caption.monospacedDigit().weight(.semibold)).foregroundStyle(.secondary)
                }
                Spacer(minLength: 4)
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(.tertiary)
            }
            .padding(.horizontal, 12).padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(MacCardPalette.inset, in: .rect(cornerRadius: 10, style: .continuous))
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help("Show the details")
        .accessibilityLabel("\(row.label), \(row.summary)")
    }

    private var chips: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 6) { chipItems }
            VStack(alignment: .leading, spacing: 6) { chipItems }
        }
    }

    @ViewBuilder private var chipItems: some View {
        ForEach(card.blocks, id: \.id) { chip($0, holdsBack: false) }
        ForEach(card.blockedBy, id: \.id) { chip($0, holdsBack: true) }
    }

    private func chip(_ chip: BlockChip, holdsBack: Bool) -> some View {
        let tint = holdsBack ? MacCardPalette.tone(.warning) : Color.secondary
        return Button { if let link = chip.link { onLink(link) } } label: {
            Label(chip.label, systemImage: holdsBack ? "lock" : "arrow.turn.down.right")
                .font(.caption.weight(.medium)).lineLimit(1).foregroundStyle(tint)
                .padding(.horizontal, 9).padding(.vertical, 4)
                .background(tint.opacity(0.12), in: .capsule)
        }
        .buttonStyle(.plain)
        .disabled(chip.link == nil)
    }

    private func replyField(_ action: AlondraContract.Action) -> some View {
        let placeholder: String = {
            if case let .text(placeholder, _, _) = action.input, let placeholder { return placeholder }
            return String(localized: "Reply to the node…")
        }()
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        return HStack(alignment: .bottom, spacing: 8) {
            TextField(placeholder, text: $reply, axis: .vertical)
                .textFieldStyle(.plain).lineLimit(1...5)
                .focused($replyFocused)
                .onSubmit { send(action) }
                .onKeyPress(.escape) { replyFocused = false; return .handled }
            Button { send(action) } label: {
                Image(systemName: "arrow.up.circle.fill").font(.title3)
                    .foregroundStyle(text.isEmpty ? Color.secondary.opacity(0.5) : MacCardPalette.kind(card.kind))
            }
            .buttonStyle(.plain)
            .disabled(text.isEmpty)
            .help("Send the reply")
            .accessibilityLabel("Send the reply")
        }
        .padding(.horizontal, 12).padding(.vertical, 9)
        .background(MacCardPalette.inset, in: .rect(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous)
            .strokeBorder(replyFocused ? MacCardPalette.kind(card.kind).opacity(0.7) : MacCardPalette.hairline))
    }

    private func send(_ action: AlondraContract.Action) {
        let text = reply.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        reply = ""
        onReply(action, text)
    }

    static func symbol(forContent row: ContentRow) -> String {
        switch row.kind {
        case "plan": "list.bullet.rectangle"
        case "document": "doc.text"
        case "diff": "arrow.left.arrow.right"
        case "command": "terminal"
        case "report": "doc.plaintext"
        case "steps": "list.number"
        case "options": "list.bullet"
        case "diagnosis": "stethoscope"
        case "attachments": "paperclip"
        default: "doc.text"
        }
    }
}

/// The buttons in a column by role, and what does not fit behind "More…".
struct MacActionColumn: View {
    let buttons: MacCardButtons
    @Binding var moreOpen: Bool
    var onAction: (AlondraContract.Action) -> Void

    var body: some View {
        VStack(spacing: 8) {
            ForEach(Array(buttons.visible.enumerated()), id: \.offset) { _, action in
                VStack(spacing: 4) {
                    Button { onAction(action) } label: { MacCardButtonLabel(action: action) }
                        .buttonStyle(MacCardButtonStyle(role: action.resolvedRole))
                }
            }
            if !buttons.more.isEmpty { moreButton }
        }
    }

    private var moreButton: some View {
        Button { moreOpen.toggle() } label: {
            HStack(spacing: 6) {
                Text("More…").font(.subheadline.weight(.medium))
                Text("\(buttons.more.count)").font(.caption.monospacedDigit()).foregroundStyle(.tertiary)
                Spacer(minLength: 0)
                MacKeyHint(key: "m")
            }
            .padding(.horizontal, 12).frame(maxWidth: .infinity, minHeight: 32, alignment: .leading)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        .popover(isPresented: $moreOpen, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(buttons.more.enumerated()), id: \.offset) { _, action in
                    Button {
                        moreOpen = false
                        onAction(action)
                    } label: {
                        HStack(spacing: 16) {
                            Text(action.label)
                            Spacer(minLength: 12)
                            if let key = action.key { MacKeyHint(key: key) }
                        }
                        .padding(.horizontal, 10).padding(.vertical, 6).contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(6).frame(minWidth: 220)
        }
    }
}

/// An action bar the way a card shows its buttons: three by role, the rest behind "More…".
struct MacActionBar: View {
    let buttons: MacCardButtons
    var onAction: (AlondraContract.Action) -> Void
    @State private var moreOpen = false

    init(bar: ActionBar, onAction: @escaping (AlondraContract.Action) -> Void) {
        buttons = MacCardButtons(actions: bar.actions, more: [])
        self.onAction = onAction
    }

    var body: some View {
        MacActionColumn(buttons: buttons, moreOpen: $moreOpen, onAction: onAction)
            .frame(maxWidth: 640)
            .background { MacShortcuts(buttons: buttons, moreOpen: $moreOpen, onAction: onAction, reply: nil) }
    }
}

/// Single-key shortcuts for the buttons of a card or action bar, kept off screen.
struct MacShortcuts: View {
    let buttons: MacCardButtons
    @Binding var moreOpen: Bool
    var onAction: (AlondraContract.Action) -> Void
    var reply: (() -> Void)?

    var body: some View {
        ZStack {
            ForEach(Array((buttons.visible + buttons.more).enumerated()), id: \.offset) { _, action in
                if let key = action.key, key.count == 1, let character = key.first, !(key == "m" && !buttons.more.isEmpty), !(key == "r" && reply != nil) {
                    Button(action.label) { onAction(action) }
                        .keyboardShortcut(KeyEquivalent(character), modifiers: [])
                }
            }
            if !buttons.more.isEmpty {
                Button("More…") { moreOpen.toggle() }.keyboardShortcut("m", modifiers: [])
            }
            if let reply {
                Button("Reply", action: reply).keyboardShortcut("r", modifiers: [])
            }
        }
        .opacity(0).frame(width: 0, height: 0).accessibilityHidden(true)
    }
}

struct MacKeyHint: View {
    let key: String
    var body: some View {
        Text(key).font(.caption2.monospaced().weight(.semibold))
            .padding(.horizontal, 5).padding(.vertical, 1)
            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.foreground.opacity(0.35)))
            .opacity(0.8)
            .accessibilityHidden(true)
    }
}

private struct MacCardButtonLabel: View {
    let action: AlondraContract.Action
    var body: some View {
        ZStack {
            VStack(spacing: 2) {
                Text(action.label).font(.subheadline.weight(.semibold)).multilineTextAlignment(.center)
                if let description = action.description {
                    Text(description).font(.caption).opacity(0.85).multilineTextAlignment(.center).lineLimit(2)
                }
            }
            .padding(.horizontal, 36)
            HStack {
                Spacer()
                if let key = action.key, key.count == 1 { MacKeyHint(key: key) }
            }
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .frame(maxWidth: .infinity, minHeight: 34)
    }
}

private struct MacCardButtonStyle: ButtonStyle {
    let role: ActionRole
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        let shape = RoundedRectangle(cornerRadius: 10, style: .continuous)
        configuration.label
            .foregroundStyle(isEnabled ? foreground : Color.secondary)
            .background(fill.opacity(isEnabled ? 1 : 0.3), in: shape)
            .overlay(shape.strokeBorder(role == .neutral ? MacCardPalette.neutralBorder : .clear))
            .contentShape(shape)
            .opacity(configuration.isPressed ? 0.8 : 1)
    }

    private var fill: Color {
        switch role {
        case .advance: MacCardPalette.advance
        case .risk: MacCardPalette.risk
        case .neutral: .clear
        }
    }

    private var foreground: Color {
        switch role {
        case .advance: MacCardPalette.onAdvance
        case .risk: MacCardPalette.onRisk
        case .neutral: .primary
        }
    }
}

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
    var onAttachment: (Attachment) -> Void
    var onContentRow: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if let title = section.title {
                Text(title).font(.headline).accessibilityAddTraits(.isHeader)
            }
            if !section.collapsed {
                ForEach(identifiedItems, id: \.id) { _, item in
                    if case let .card(card) = item, card.need != nil {
                        MacCardView(card: card, returnReason: returnReasons[card.id], onAction: onAction, onReply: onReply, onLink: onLink, onContent: onContentRow)
                    } else if case let .actions(bar) = item {
                        MacActionBar(bar: bar, onAction: onAction)
                    } else {
                        ViewItemRenderer(item: item, onAction: onAction, onLink: onLink, onForm: onForm, onContent: onContent, onAttachment: onAttachment)
                    }
                }
                if let more = section.more {
                    Button("Show \(more.remaining) more") { onMore(more.key) }
                        .font(.footnote.weight(.semibold))
                }
            }
        }
    }

    /// A card is identified by its id, so a card that arrives above it in a refresh never takes over
    /// its reply draft or its keyboard focus.
    private var identifiedItems: [(id: String, item: ViewItem)] {
        section.items.enumerated().map { offset, item in
            if case let .card(card) = item { ("card:\(card.id)", item) } else { ("item:\(offset)", item) }
        }
    }
}
