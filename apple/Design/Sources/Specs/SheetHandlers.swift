import AlondraContract
import SwiftUI

/// What a sheet does with the person's taps: the screen that shows it decides.
public struct SheetHandlers {
    public var onLink: (AlondraContract.Link) -> Void
    public var onAction: (Action) -> Void
    /// Starts a comment anchored to a part of a sheet; absent where the sheet cannot be commented on.
    public var onComment: ((SheetAnchor) -> Void)?
    /// The anchors that already carry a comment.
    public var commented: Set<String>

    public init(onLink: @escaping (AlondraContract.Link) -> Void = { _ in }, onAction: @escaping (Action) -> Void = { _ in },
                onComment: ((SheetAnchor) -> Void)? = nil, commented: Set<String> = []) {
        self.onLink = onLink; self.onAction = onAction; self.onComment = onComment; self.commented = commented
    }
}

/// An edit of a part of a sheet that has not been confirmed by the Mac yet.
public enum SheetSaving: Equatable, Sendable {
    case saving
    /// The Mac has not confirmed it for a while; it goes on retrying on its own.
    case overdue
}

public extension EnvironmentValues {
    @Entry var sheetHandlers = SheetHandlers()
    /// The parts of the sheets being saved, by the link that edits them.
    @Entry var sheetSaving: [String: SheetSaving] = [:]
}

enum SheetStyle {
    static func color(_ tone: Tone) -> Color {
        switch tone {
        case .neutral: Palette.slate
        case .info: Palette.sky
        case .accent: Palette.dawn
        case .success: Palette.sage
        case .warning: Palette.gold
        case .critical: Palette.rose
        case .yourTurn: Palette.ember
        }
    }

    static func symbol(_ icon: Icon, _ tone: Tone) -> String {
        switch (icon, tone) {
        case (.check, .success): "checkmark.circle.fill"
        case (.check, .critical): "xmark.circle.fill"
        case (.check, _): "checkmark.circle"
        case (.warning, _): "exclamationmark.triangle.fill"
        case (.attachment, _): "photo"
        case (.history, _): "clock.arrow.circlepath"
        case (.question, _): "questionmark.bubble"
        case (.document, _): "doc.text"
        case (.task, _): "checklist"
        case (.notice, .success): "checkmark.circle.fill"
        case (.notice, .critical): "xmark.circle.fill"
        case (.notice, .warning): "exclamationmark.triangle.fill"
        case (.notice, _): "info.circle"
        default: "circle.fill"
        }
    }

    static func symbol(_ change: SheetChange) -> String {
        switch change {
        case .new: "plus.circle.fill"
        case .changed: "pencil.circle.fill"
        case .removed: "minus.circle.fill"
        case .unchanged: "equal.circle"
        }
    }

    static func color(_ change: SheetChange) -> Color {
        switch change {
        case .new: Palette.sage
        case .changed: Palette.sky
        case .removed: Palette.rose
        case .unchanged: Palette.slate
        }
    }

    static func color(_ lane: SheetLaneKind) -> Color {
        switch lane {
        case .given: Palette.sky
        case .when: Palette.gold
        case .then: Palette.sage
        case .other: Palette.slate
        }
    }
}

/// A change in a sheet, always an icon and a word.
struct SheetMarkView: View {
    let mark: SheetMark

    var body: some View {
        Label(mark.label, systemImage: SheetStyle.symbol(mark.change))
            .font(.caption.weight(.semibold))
            .foregroundStyle(SheetStyle.color(mark.change))
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(SheetStyle.color(mark.change).opacity(0.12), in: .capsule)
            .fixedSize(horizontal: false, vertical: true)
    }
}

struct SheetSavingView: View {
    let saving: SheetSaving

    var body: some View {
        switch saving {
        case .saving:
            Label { Text("Saving…", tableName: "AlondraDesign", bundle: #bundle) } icon: { ProgressView().controlSize(.mini) }
                .font(.caption.weight(.semibold)).foregroundStyle(Palette.slate)
        case .overdue:
            Label { Text("Your Mac hasn’t confirmed it yet · Retrying…", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "arrow.triangle.2.circlepath") }
                .font(.caption.weight(.semibold)).foregroundStyle(Palette.gold)
        }
    }
}

/// A line of a result: an icon in the tone's colour and the words, so colour is never all there is.
struct SheetLineView: View {
    let line: SheetLine
    var monospaced = false

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: SheetStyle.symbol(line.icon, line.tone)).foregroundStyle(SheetStyle.color(line.tone)).accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(line.text).font(monospaced ? .mono(.footnote) : .subheadline).foregroundStyle(Palette.ink)
                if let detail = line.detail { Text(detail).font(.caption).foregroundStyle(.secondary) }
            }
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .accessibilityElement(children: .combine)
    }
}

struct SheetCard<Content: View>: View {
    var raised = false
    @ViewBuilder let content: Content
    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        content
            .padding(typeSize.isAccessibilitySize ? 10 : 14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(raised ? Palette.cardRaised : Palette.card, in: .rect(cornerRadius: 16, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(Palette.hairline))
    }
}

/// The buttons at the end of a header: comment and edit, each at least 44 points wide.
struct SheetAffordances: View {
    let anchor: SheetAnchor?
    let edit: AlondraContract.Link?
    let subject: String
    @Environment(\.sheetHandlers) private var handlers

    var body: some View {
        HStack(spacing: 0) {
            if let anchor, let onComment = handlers.onComment {
                let done = handlers.commented.contains(anchor.id)
                Button { onComment(anchor) } label: {
                    Image(systemName: done ? "bubble.left.fill" : "bubble.left").frame(minWidth: 44, minHeight: 44)
                }
                .accessibilityLabel(Text("Comment on \(subject)", tableName: "AlondraDesign", bundle: #bundle))
                .accessibilityValue(done ? Text("Has a comment", tableName: "AlondraDesign", bundle: #bundle) : Text(verbatim: ""))
            }
            if let edit {
                Button { handlers.onLink(edit) } label: { Image(systemName: "pencil").frame(minWidth: 44, minHeight: 44) }
                    .accessibilityLabel(Text("Edit \(subject)", tableName: "AlondraDesign", bundle: #bundle))
            }
        }
        .buttonStyle(.borderless)
        .foregroundStyle(Palette.dawn)
    }
}
