import AlondraContract
import AlondraDesign
import SwiftUI

/// A row of a section the daemon styles "list", as a settings list shows it: a symbol in the tone
/// of its first label, the title, that label as a chip, the subtitle line by line, its children
/// below, and its first action as a button with the rest in a menu. An action presented as a menu
/// lists its options. What each row offers is the daemon's: this view only lays it out.
public struct ListRowView: View {
    public let row: AlondraContract.Row
    public var busy: Set<String>
    public var onAction: (Action) -> Void
    public var onLink: (Link) -> Void
    public var onOption: (Action, String) -> Void

    public init(row: AlondraContract.Row, busy: Set<String> = [], onAction: @escaping (Action) -> Void, onLink: @escaping (Link) -> Void,
                onOption: @escaping (Action, String) -> Void = { _, _ in }) {
        self.row = row; self.busy = busy; self.onAction = onAction; self.onLink = onLink; self.onOption = onOption
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .center, spacing: 12) {
                    ListToneSymbol(icon: row.icon, tone: row.badges.first?.tone)
                    labels.fixedSize(horizontal: true, vertical: false)
                    Spacer(minLength: 0)
                    actions
                }
                HStack(alignment: .top, spacing: 12) {
                    ListToneSymbol(icon: row.icon, tone: row.badges.first?.tone)
                    VStack(alignment: .leading, spacing: 8) {
                        labels
                        if !row.actions.isEmpty { actions }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            if let model = row.model {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Requested: \(model.requested)")
                    if let effective = model.effective { Text("Effective: \(effective)") }
                    if let pending = model.pending { Text("Pending: \(pending)").foregroundStyle(Palette.ember) }
                    if let reason = model.reason { Text(verbatim: reason).foregroundStyle(.secondary) }
                }.font(.caption).padding(.leading, 44)
            }
            ForEach(disabledReasons, id: \.self) { reason in
                Text(verbatim: reason).font(.footnote).foregroundStyle(.secondary).padding(.leading, 44)
            }
            ForEach(row.children, id: \.id) { child in
                HStack(spacing: 8) {
                    Circle().fill(ListTone.tint(child.badges.first?.tone)).frame(width: 7, height: 7)
                    Text(verbatim: child.title).font(.subheadline).foregroundStyle(Palette.ink).lineLimit(1)
                    Spacer(minLength: 8)
                    if let badge = child.badges.first { ListToneChip(label: badge) }
                }
                .padding(.leading, 44)
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("list.row.\(row.id)")
    }

    private var labels: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(verbatim: row.title).font(.body.weight(.semibold)).foregroundStyle(Palette.ink).lineLimit(2).fixedSize(horizontal: false, vertical: true)
            if let badge = row.badges.first { ListToneChip(label: badge) }
            if let subtitle = row.subtitle {
                Text(verbatim: subtitle).font(.footnote).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var disabledReasons: [String] {
        row.actions.filter { !$0.enabled }.compactMap { $0.disabledReason ?? ($0.needsNewerApp ? String(localized: "Update Alondra to use this action") : nil) }
            .reduce(into: []) { if !$0.contains($1) { $0.append($1) } }
    }

    private func run(_ action: Action) {
        if let link = action.link { onLink(link) } else { onAction(action) }
    }

    @ViewBuilder private var actions: some View {
        let shown = row.actions
        HStack(spacing: 6) {
            if let first = shown.first { button(first).disabled(!first.enabled) }
            if shown.count > 1 {
                Menu {
                    ForEach(Array(shown.dropFirst()), id: \.label) { action in
                        if action.presentation == .menu, !action.options.isEmpty {
                            Menu { options(action) } label: { Text(verbatim: action.label) }.disabled(!action.enabled)
                        } else {
                            Button(role: action.style == .destructive ? .destructive : nil) { run(action) } label: { Text(verbatim: action.label) }.disabled(!action.enabled)
                        }
                    }
                } label: {
                    Label("More", systemImage: "ellipsis.circle").labelStyle(.iconOnly).font(.title3)
                }
                .menuStyle(.button)
                .buttonStyle(.borderless)
                .fixedSize()
            }
        }
        .disabled(shown.contains { $0.id.map(busy.contains) == true })
    }

    @ViewBuilder private func options(_ action: Action) -> some View {
        ForEach(action.options, id: \.id) { option in
            Button(role: option.style == .destructive ? .destructive : nil) { onOption(action, option.id) } label: { Text(verbatim: option.label) }
        }
    }

    @ViewBuilder private func button(_ action: Action) -> some View {
        if action.presentation == .menu, !action.options.isEmpty {
            Menu { options(action) } label: { Text(verbatim: action.label) }
                .menuStyle(.button).buttonStyle(.bordered).controlSize(.small).fixedSize()
        } else if action.style == .primary {
            Button { run(action) } label: {
                Text(verbatim: action.label).font(.subheadline.weight(.semibold)).padding(.horizontal, 14).padding(.vertical, 7)
            }
            .buttonStyle(FilledStyle(tint: Palette.dawn)).fixedSize()
        } else {
            Button(role: action.style == .destructive ? .destructive : nil) { run(action) } label: { Text(verbatim: action.label) }
                .buttonStyle(.bordered).controlSize(.small).fixedSize()
        }
    }
}

enum ListTone {
    static func tint(_ tone: Tone?) -> Color {
        switch tone {
        case .success: Palette.sage
        case .critical: Palette.rose
        case .yourTurn, .warning: Palette.ember
        case .info, .accent: Palette.sky
        case .neutral, nil: Palette.slate
        }
    }

    static func symbol(_ tone: Tone?) -> String {
        switch tone {
        case .success: "checkmark"
        case .critical: "exclamationmark"
        case .yourTurn, .warning: "key.fill"
        case .info, .accent: "arrow.triangle.2.circlepath"
        case .neutral, nil: "pause.fill"
        }
    }
}

/// The row's state as a symbol in a tinted circle; a project row shows a folder, and a row with
/// no state the symbol of what it is.
struct ListToneSymbol: View {
    let icon: Icon
    let tone: Tone?

    private var symbol: String {
        switch (icon, tone) {
        case (.project, _): "folder.fill"
        case (.action, nil): "plus"
        case (.node, nil): "arrow.triangle.branch"
        default: ListTone.symbol(tone)
        }
    }

    var body: some View {
        let tint = icon == .project ? Palette.slate : ListTone.tint(tone)
        Image(systemName: symbol)
            .font(.subheadline.weight(.bold))
            .foregroundStyle(tint)
            .frame(width: 32, height: 32)
            .background(tint.opacity(0.15), in: .circle)
            .accessibilityHidden(true)
    }
}

/// A label in its tone, as the app's state chips read.
struct ListToneChip: View {
    let label: AlondraContract.Label

    var body: some View {
        Text(verbatim: label.text)
            .font(.caption.weight(.semibold))
            .foregroundStyle(ListTone.tint(label.tone))
            .padding(.horizontal, 9)
            .padding(.vertical, 3)
            .background(ListTone.tint(label.tone).opacity(0.14), in: .capsule)
            .lineLimit(1)
            .fixedSize()
    }
}
