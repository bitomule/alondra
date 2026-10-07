import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// How a button reads on a card: green filled to advance, red filled when advancing is risky,
/// plain glass when it is neither. The role comes from the daemon.
extension ActionRole {
    var fill: Color? {
        switch self {
        case .advance: Palette.advance
        case .risk: Palette.risk
        case .neutral: nil
        }
    }
}

extension View {
    /// A bar button as Liquid Glass: tinted and prominent to advance or to risk, plain glass otherwise.
    @ViewBuilder func glassRole(_ role: ActionRole) -> some View {
        if let fill = role.fill {
            buttonStyle(.glassProminent).tint(fill)
        } else {
            buttonStyle(.glass)
        }
    }
}

extension WireAction {
    var cardRole: ActionRole { paintedRole }
}

/// The one line of content a card shows; what is behind it opens in the detail.
struct CardContentRow: View {
    let row: ContentRow
    let tint: Color
    let open: () -> Void

    var body: some View {
        Button(action: open) {
            HStack(spacing: 12) {
                Image(systemName: Self.symbol(row.kind)).font(.title3).foregroundStyle(tint)
                    .frame(width: 28)
                VStack(alignment: .leading, spacing: 2) {
                    Text(row.label).font(.subheadline.weight(.semibold)).foregroundStyle(Palette.ink)
                    Text(row.summary).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
                Spacer(minLength: 8)
                if row.moreCount > 0 {
                    Text("+\(row.moreCount)").font(.caption.weight(.semibold).monospacedDigit()).foregroundStyle(.secondary)
                        .padding(.horizontal, 8).padding(.vertical, 3)
                        .background(Palette.hairline, in: .capsule)
                }
                Image(systemName: "chevron.right").font(.footnote.weight(.bold)).foregroundStyle(.tertiary)
            }
            .padding(12)
            .background(tint.opacity(0.10), in: .rect(cornerRadius: 16, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(tint.opacity(0.25)))
            .contentShape(.rect(cornerRadius: 16))
        }
        .buttonStyle(PressStyle())
        .accessibilityElement(children: .combine)
        .accessibilityHint(Text("Opens the details"))
    }

    static func symbol(_ kind: String) -> String {
        switch kind {
        case "diff": "plusminus"
        case "command": "terminal"
        case "plan": "list.bullet.clipboard"
        case "document": "doc.richtext"
        case "report": "doc.text"
        case "steps": "checklist"
        case "options": "list.bullet"
        case "diagnosis": "stethoscope"
        case "attachments": "paperclip"
        default: "doc.text"
        }
    }
}

/// What a card holds back or is held back by, as a link to the other card.
struct BlockChips: View {
    let blocks: [BlockChip]
    let blockedBy: [BlockChip]
    let open: (AlondraContract.Link) -> Void

    var body: some View {
        if !blocks.isEmpty || !blockedBy.isEmpty {
            VStack(alignment: .leading, spacing: 6) {
                ForEach(blockedBy, id: \.id) { chip(chip: $0, symbol: "lock.fill") }
                ForEach(blocks, id: \.id) { chip(chip: $0, symbol: "arrow.turn.down.right") }
            }
        }
    }

    @ViewBuilder private func chip(chip: BlockChip, symbol: String) -> some View {
        let label = Label(chip.label, systemImage: symbol)
            .font(.caption.weight(.medium)).foregroundStyle(Palette.gold)
            .lineLimit(2).multilineTextAlignment(.leading)
            .padding(.horizontal, 10).padding(.vertical, 5)
            .background(Palette.gold.opacity(0.14), in: .capsule)
        if let link = chip.link {
            Button { open(link) } label: { label }.buttonStyle(.plain)
        } else {
            label
        }
    }
}

/// The sheet a button opens when it needs a reason or some words first: "Request clarification"
/// asks which reason applies and what to clarify. Dressed like the app's other writing sheets.
public struct CompositionSheet: View {
    let composition: TriageCardFeature.Composition
    let tint: Color
    let reasonChanged: (String) -> Void
    let noteChanged: (String) -> Void
    let cancel: () -> Void
    let send: () -> Void
    @FocusState private var focused: Bool

    public init(
        composition: TriageCardFeature.Composition, tint: Color, reasonChanged: @escaping (String) -> Void,
        noteChanged: @escaping (String) -> Void, cancel: @escaping () -> Void, send: @escaping () -> Void
    ) {
        self.composition = composition
        self.tint = tint
        self.reasonChanged = reasonChanged
        self.noteChanged = noteChanged
        self.cancel = cancel
        self.send = send
    }

    private var placeholder: String {
        if case .text(let placeholder, _, _) = composition.action.input, let placeholder { return placeholder }
        return String(localized: "Write your answer")
    }

    public var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if composition.needsReason {
                        VStack(spacing: 8) {
                            ForEach(composition.action.options, id: \.id) { option in
                                reasonRow(option)
                            }
                        }
                    }
                    if case .text = composition.action.input {
                        TextField(placeholder, text: Binding(get: { composition.note }, set: noteChanged), axis: .vertical)
                            .lineLimit(4...10).focused($focused)
                            .padding(16)
                            .background(Palette.cardRaised, in: .rect(cornerRadius: 22, style: .continuous))
                            .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).strokeBorder(Palette.hairline))
                    }
                }
                .padding(.horizontal, 20).padding(.vertical, 12)
            }
            .scrollDismissesKeyboard(.interactively)
            .background(DawnBackground(glow: tint))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Text(verbatim: composition.action.label)
                        .font(.display(.headline)).foregroundStyle(Palette.ink).lineLimit(1)
                }
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: .cancel, action: cancel) { Image(systemName: "xmark") }
                        .accessibilityLabel("Cancel")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(action: send) { Image(systemName: "arrow.up") }
                        .buttonStyle(.glassProminent).tint(tint)
                        .disabled(!composition.canSend)
                        .accessibilityLabel("Send")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationBackground(Palette.night)
    }

    private func reasonRow(_ option: ActionOption) -> some View {
        let picked = composition.reason == option.id
        return Button { reasonChanged(option.id) } label: {
            HStack(spacing: 12) {
                Image(systemName: picked ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(picked ? tint : .secondary)
                Text(option.label).font(.body.weight(.medium)).foregroundStyle(Palette.ink)
                    .multilineTextAlignment(.leading)
                Spacer()
            }
            .padding(14)
            .background(picked ? tint.opacity(0.14) : Palette.cardRaised, in: .rect(cornerRadius: 16, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(picked ? tint.opacity(0.5) : Palette.hairline))
            .contentShape(.rect(cornerRadius: 16))
        }
        .buttonStyle(PressStyle())
        .accessibilityAddTraits(picked ? .isSelected : [])
    }
}
