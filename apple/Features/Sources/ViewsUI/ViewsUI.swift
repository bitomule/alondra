import AlondraDesign
import AlondraContract
import Foundation
import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

public typealias WireLabel = AlondraContract.Label
public typealias WireForm = AlondraContract.Form
public typealias WireGauge = AlondraContract.Gauge
public typealias Link = AlondraContract.Link

public enum ViewsDate {
    public static func display(_ raw: String, format: TimeFormat = .relative) -> String {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        guard let date = fractional.date(from: raw) ?? ISO8601DateFormatter().date(from: raw) else {
            return raw
        }
        switch format {
        case .relative:
            let now = RelativeClock.now()
            guard abs(date.timeIntervalSince(now)) < 7 * 24 * 60 * 60 else {
                return date.formatted(date: .abbreviated, time: .shortened)
            }
            let formatter = RelativeDateTimeFormatter()
            formatter.dateTimeStyle = .named
            return formatter.localizedString(for: date, relativeTo: now)
        case .absolute: return date.formatted(date: .abbreviated, time: .shortened)
        case .time: return date.formatted(date: .omitted, time: .shortened)
        }
    }
}

/// The view palette mirrors the app's existing Palette values and adapts them to the same
/// light/dark colors. These components deliberately leave screen layout to their callers.
enum ViewPalette {
    static let ink = adaptive(light: 0x1B1A18, dark: 0xF4EFE8)
    static let card = adaptive(light: 0xFFFDFA, dark: 0x1C1E27)
    static let raised = adaptive(light: 0xFFFFFF, dark: 0x252836)
    static let hairline = adaptive(light: 0x000000, dark: 0xFFFFFF, lightAlpha: 0.08, darkAlpha: 0.09)
    static let night = adaptive(light: 0xF6F1EA, dark: 0x0C0D12)
    static let dawn = adaptive(light: 0xD9661F, dark: 0xFF9A4D)
    static let ember = adaptive(light: 0xC2410C, dark: 0xFF7A45)
    static let sky = adaptive(light: 0x2F6FB0, dark: 0x7FB6F0)
    static let sage = adaptive(light: 0x3F7D4E, dark: 0x8CCB94)
    static let rose = adaptive(light: 0xB83A4B, dark: 0xFF8A9A)
    static let slate = adaptive(light: 0x6B6F7A, dark: 0xA6ABB8)

    private static func adaptive(light: UInt32, dark: UInt32, lightAlpha: Double = 1, darkAlpha: Double = 1) -> Color {
        #if canImport(UIKit)
        Color(uiColor: UIColor { traits in
            let hex = traits.userInterfaceStyle == .dark ? dark : light
            let alpha = traits.userInterfaceStyle == .dark ? darkAlpha : lightAlpha
            return UIColor(red: CGFloat((hex >> 16) & 255) / 255, green: CGFloat((hex >> 8) & 255) / 255, blue: CGFloat(hex & 255) / 255, alpha: alpha)
        })
        #elseif canImport(AppKit)
        Color(nsColor: NSColor(name: nil) { appearance in
            let isDark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            let hex = isDark ? dark : light
            let alpha = isDark ? darkAlpha : lightAlpha
            return NSColor(srgbRed: CGFloat((hex >> 16) & 255) / 255, green: CGFloat((hex >> 8) & 255) / 255, blue: CGFloat(hex & 255) / 255, alpha: alpha)
        })
        #else
        Color(red: Double((light >> 16) & 255) / 255, green: Double((light >> 8) & 255) / 255, blue: Double(light & 255) / 255)
        #endif
    }
}

public enum SidebarTint {
    public static func node(_ tone: Tone?) -> Color {
        switch tone {
        case nil, .neutral: .gray
        case .info: .indigo
        case .accent: .orange
        case .success: .green
        case .warning: .yellow
        case .critical: .red
        case .yourTurn: .pink
        }
    }

    public static func task(_ tone: Tone?) -> Color {
        switch tone {
        case nil, .neutral: .gray
        case .info: .indigo
        case .accent: .orange
        case .success: .green
        case .warning: .orange
        case .critical: .red
        case .yourTurn: .pink
        }
    }
}

private func toneColor(_ tone: Tone) -> Color {
    switch tone {
    case .neutral: .secondary
    case .info: ViewPalette.sky
    case .accent: ViewPalette.dawn
    case .success: ViewPalette.sage
    case .warning: ViewPalette.ember
    case .critical: ViewPalette.rose
    case .yourTurn: ViewPalette.ember
    }
}

private func iconName(_ icon: Icon) -> String {
    switch icon {
    case .question: "questionmark.bubble"
    case .action: "hand.point.right"
    case .document: "doc.richtext"
    case .pullRequest: "arrow.triangle.pull"
    case .notice: "bell"
    case .delivery: "flag.checkered"
    case .task: "checklist"
    case .request: "paperplane"
    case .node: "cpu"
    case .orchestrator: "point.3.connected.trianglepath.dotted"
    case .mission: "scope"
    case .project: "folder"
    case .schedule: "calendar"
    case .attachment: "paperclip"
    case .check: "checkmark"
    case .warning: "exclamationmark.triangle"
    case .merge: "arrow.triangle.merge"
    case .branch: "arrow.branch"
    case .history: "clock.arrow.circlepath"
    case .machine: "desktopcomputer"
    }
}

public struct ViewItemRenderer: View {
    public let item: ViewItem
    public var onAction: (Action) -> Void
    public var onLink: (Link) -> Void
    public var onForm: (WireForm) -> Void
    public var onContent: ((ContentRef) -> Void)?
    public var onAttachment: ((Attachment) -> Void)?
    public var excludedActionIDs: Set<String> = []
    public init(item: ViewItem, onAction: @escaping (Action) -> Void = { _ in }, onLink: @escaping (Link) -> Void = { _ in }, onForm: @escaping (WireForm) -> Void = { _ in }, onContent: ((ContentRef) -> Void)? = nil, onAttachment: ((Attachment) -> Void)? = nil, excludingActionIDs: Set<String> = []) {
        self.item = item; self.onAction = onAction; self.onLink = onLink; self.onForm = onForm; self.onContent = onContent; self.onAttachment = onAttachment; self.excludedActionIDs = excludingActionIDs
    }
    @ViewBuilder public var body: some View {
        switch item {
        case let .card(value): CardView(card: value, onAction: onAction, onLink: onLink, onAttachment: onAttachment, excludingActionIDs: excludedActionIDs)
        case let .row(value): RowView(row: value, onAction: onAction, onLink: onLink)
        case let .markdown(value): MarkdownView(markdown: value)
        case let .contentRef(value): ContentRefView(content: value, onOpen: onLink, onLoad: onContent)
        case let .facts(value): FactsView(facts: value, onLink: onLink)
        case let .checks(value): ChecksView(checks: value, onLink: onLink)
        case let .timeline(value): TimelineView(timeline: value)
        case let .form(value): FormView(form: value, onSubmit: onAction, onOpenForm: onForm)
        case let .notice(value): NoticeView(notice: value, onAction: onAction)
        case let .actions(value): ActionBarView(bar: value, onAction: onAction, onLink: onLink, excludingActionIDs: excludedActionIDs)
        case .unsupported: UnsupportedItemView()
        }
    }
}

public struct SectionView: View {
    public let section: ViewSection
    public var onAction: (Action) -> Void
    public var onLink: (Link) -> Void
    public var onMore: (String) -> Void
    public var onForm: (WireForm) -> Void
    public var onContent: ((ContentRef) -> Void)?
    public var onAttachment: ((Attachment) -> Void)?
    public var excludedActionIDs: Set<String> = []
    public init(section: ViewSection, onAction: @escaping (Action) -> Void = { _ in }, onLink: @escaping (Link) -> Void = { _ in }, onMore: @escaping (String) -> Void = { _ in }, onForm: @escaping (WireForm) -> Void = { _ in }, onContent: ((ContentRef) -> Void)? = nil, onAttachment: ((Attachment) -> Void)? = nil, excludingActionIDs: Set<String> = []) {
        self.section = section; self.onAction = onAction; self.onLink = onLink; self.onMore = onMore; self.onForm = onForm; self.onContent = onContent; self.onAttachment = onAttachment; self.excludedActionIDs = excludingActionIDs
    }
    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            if section.title != nil || section.count != nil {
                HStack {
                    if let title = section.title { Text(title).font(.headline).foregroundStyle(ViewPalette.ink).accessibilityAddTraits(.isHeader) }
                    Spacer()
                    if let count = section.count { Text("\(count)").font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
                }
            }
            if !section.collapsed {
                ForEach(Array(section.items.enumerated()), id: \.offset) { _, item in
                    ViewItemRenderer(item: item, onAction: onAction, onLink: onLink, onForm: onForm, onContent: onContent, onAttachment: onAttachment, excludingActionIDs: excludedActionIDs)
                }
                if let more = section.more {
                    Button("Show \(more.remaining) more") { onMore(more.key) }
                        .font(.footnote.weight(.semibold)).foregroundStyle(ViewPalette.dawn)
                }
            }
        }
    }
}

public struct CardView: View {
    public let card: Card
    public var onAction: (Action) -> Void
    public var onLink: (Link) -> Void
    public var onAttachment: ((Attachment) -> Void)?
    public var excludedActionIDs: Set<String> = []
    public init(card: Card, onAction: @escaping (Action) -> Void = { _ in }, onLink: @escaping (Link) -> Void = { _ in }, onAttachment: ((Attachment) -> Void)? = nil, excludingActionIDs: Set<String> = []) { self.card = card; self.onAction = onAction; self.onLink = onLink; self.onAttachment = onAttachment; self.excludedActionIDs = excludingActionIDs }
    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: iconName(card.icon)).foregroundStyle(ViewPalette.dawn)
                if let eyebrow = card.eyebrow { LabelPill(label: eyebrow) }
                Spacer()
                if let state = card.state { LabelPill(label: state) }
            }
            Text(card.title).font(.title3.weight(.semibold)).foregroundStyle(ViewPalette.ink).fixedSize(horizontal: false, vertical: true)
            if let subtitle = card.subtitle { Text(subtitle).font(.subheadline).foregroundStyle(.secondary) }
            if !card.badges.isEmpty { LabelList(labels: card.badges) }
            ForEach(Array(card.context.enumerated()), id: \.offset) { _, line in
                HStack(spacing: 6) { Image(systemName: iconName(line.icon)).foregroundStyle(.secondary); Text(line.text).font(.footnote); if let chip = line.chip { LabelPill(label: chip) } }
                    .onTapGesture { if let link = line.link { onLink(link) } }
            }
            ForEach(Array(card.timestamps.enumerated()), id: \.offset) { _, stamp in Label(ViewsDate.display(stamp.at, format: stamp.format), systemImage: "clock").font(.caption).foregroundStyle(.secondary) }
            if let body = card.body { MarkdownView(markdown: body) }
            if !card.attachments.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(card.attachments, id: \.id) { attachment in
                        if let onAttachment {
                            Button { onAttachment(attachment) } label: {
                                Label("\(attachment.name) · \(ByteCountFormatter.string(fromByteCount: Int64(clamping: attachment.size), countStyle: .file))", systemImage: attachment.kind == .image ? "photo" : "paperclip")
                                    .font(.footnote.weight(.medium)).frame(maxWidth: .infinity, alignment: .leading)
                            }.buttonStyle(.plain).help("Open attachment")
                        } else {
                            Label("\(attachment.name) · \(ByteCountFormatter.string(fromByteCount: Int64(clamping: attachment.size), countStyle: .file))", systemImage: attachment.kind == .image ? "photo" : "paperclip")
                                .font(.footnote.weight(.medium)).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            let actions = card.actions.filter { $0.id.map(excludedActionIDs.contains) != true }
            if !actions.isEmpty { ActionButtons(actions: actions, onAction: onAction, onLink: onLink) }
        }
        .padding(16).frame(maxWidth: .infinity, alignment: .leading)
        .background(ViewPalette.card, in: .rect(cornerRadius: 20, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(ViewPalette.hairline))
    }
}

public struct RowView: View {
    public let row: Row
    public var onAction: (Action) -> Void
    public var onLink: (Link) -> Void
    /// A row the daemon sends folded shows its children once clicked.
    @State private var opened = false
    public init(row: Row, onAction: @escaping (Action) -> Void = { _ in }, onLink: @escaping (Link) -> Void = { _ in }) { self.row = row; self.onAction = onAction; self.onLink = onLink }
    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if row.folded {
                Button { withAnimation(.smooth) { opened.toggle() } } label: {
                    HStack(spacing: 6) {
                        Text(MarkdownBlock.line(row.title)).font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                        Image(systemName: "chevron.right").font(.caption.weight(.bold)).foregroundStyle(.secondary)
                            .rotationEffect(.degrees(opened ? 90 : 0)).accessibilityHidden(true)
                        Spacer()
                    }.contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityHint(opened ? Text("Hides them") : Text("Shows them"))
            } else {
                HStack(spacing: 8) {
                    Image(systemName: iconName(row.icon)).foregroundStyle(ViewPalette.dawn)
                    VStack(alignment: .leading, spacing: 2) { Text(MarkdownBlock.line(row.title)).font(.subheadline.weight(.semibold)).strikethrough(row.struck); if let subtitle = row.subtitle { Text(MarkdownBlock.line(subtitle)).font(.caption).foregroundStyle(.secondary) } }
                    Spacer()
                    if let trailing = row.trailing { TrailingView(trailing: trailing) }
                }
            }
            if !row.badges.isEmpty { LabelList(labels: row.badges) }
            if !row.folded || opened {
                ForEach(row.children, id: \.id) { child in RowView(row: child, onAction: onAction, onLink: onLink).padding(.leading, 18) }
            }
            if !row.actions.isEmpty { ActionButtons(actions: row.actions, onAction: onAction, onLink: onLink) }
        }
        .padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(ViewPalette.night.opacity(0.5), in: .rect(cornerRadius: 12))
        .contentShape(Rectangle()).onTapGesture { if let link = row.link { onLink(link) } }
    }
}

private struct LabelPill: View {
    let label: WireLabel
    var body: some View { Text(label.text).font(.caption.weight(.semibold)).foregroundStyle(toneColor(label.tone)).padding(.horizontal, 8).padding(.vertical, 4).background(toneColor(label.tone).opacity(0.13), in: .capsule) }
}
private struct LabelList: View {
    let labels: [WireLabel]
    var body: some View { ViewThatFits(in: .horizontal) { HStack(spacing: 6) { ForEach(Array(labels.enumerated()), id: \.offset) { _, label in LabelPill(label: label) } }; VStack(alignment: .leading, spacing: 4) { ForEach(Array(labels.enumerated()), id: \.offset) { _, label in LabelPill(label: label) } } } }
}
private struct TrailingView: View {
    let trailing: Trailing
    var body: some View { Group { switch trailing { case let .count(value): Text("\(value)"); case let .time(at, format): Text(ViewsDate.display(at, format: format)); case let .text(text): Text(text) } }.font(.caption.monospacedDigit()).foregroundStyle(.secondary) }
}

public struct MarkdownView: View {
    public let markdown: Markdown
    /// Text set back from the ink, as a card's reasons are; headings keep the ink.
    public var secondary = false
    public init(markdown: Markdown, secondary: Bool = false) { self.markdown = markdown; self.secondary = secondary }
    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(MarkdownParser.segments(from: markdown.text)) { segment in
                switch segment {
                case let .block(block): MarkdownLine(block: block)
                case let .table(rows): MarkdownTable(rows: rows)
                }
            }
            if markdown.truncated { Text("Content is truncated.").font(.caption).foregroundStyle(.secondary) }
        }.foregroundStyle(secondary ? AnyShapeStyle(.secondary) : AnyShapeStyle(ViewPalette.ink))
    }
}
private struct MarkdownLine: View {
    let block: MarkdownBlock
    var body: some View {
        Group {
            switch block.kind {
            case .heading(let level): Text(block.content).font(headingFont(level)).foregroundStyle(ViewPalette.ink).padding(.top, level <= 2 ? 10 : 4).accessibilityAddTraits(.isHeader)
            case .paragraph: Text(block.content).font(.body).lineSpacing(3)
            case .listItem(.bullet): listRow { Circle().fill(ViewPalette.dawn).frame(width: 6, height: 6).padding(.top, 8) }
            case .listItem(.number(let marker)): listRow { Text("\(marker). ").font(.body.monospacedDigit().weight(.semibold)).foregroundStyle(ViewPalette.dawn) }
            case .listItem(.task(let done)): listRow { Image(systemName: done ? "checkmark.square.fill" : "square").foregroundStyle(done ? ViewPalette.sage : .secondary) }
            case .quote: HStack(alignment: .top, spacing: 12) { Capsule().fill(ViewPalette.slate.opacity(0.5)).frame(width: 3); Text(block.content).font(.body.italic()).foregroundStyle(.secondary) }.fixedSize(horizontal: false, vertical: true)
            case .code(let language) where MermaidBlockView.handles(language: language): MermaidBlockView(source: block.plainText)
            case .code(let language): CodeBlock(source: block.plainText, language: language) { HighlightedCode(code: block.plainText, language: language).font(.system(.callout, design: .monospaced)) }.padding(14).frame(maxWidth: .infinity, alignment: .leading).background(ViewPalette.night.opacity(0.7), in: .rect(cornerRadius: 12))
            case .tableRow: EmptyView()
            case .rule: Rectangle().fill(ViewPalette.hairline).frame(height: 1)
            case let .image(url, alt): MarkdownImage(url: url, alt: alt)
            }
        }.padding(.leading, CGFloat(block.depth) * 18)
    }
    private func headingFont(_ level: Int) -> Font { switch level { case 1: .system(.title, design: .serif, weight: .semibold); case 2: .system(.title2, design: .serif, weight: .semibold); case 3: .system(.title3, design: .serif, weight: .semibold); default: .headline } }
    private func listRow<Marker: View>(@ViewBuilder marker: () -> Marker) -> some View { HStack(alignment: .firstTextBaseline, spacing: 10) { marker().frame(minWidth: 18, alignment: .leading); Text(block.content).font(.body).lineSpacing(3) } }
}

private struct MarkdownTable: View {
    let rows: [MarkdownBlock]
    var body: some View {
        ViewThatFits(in: .horizontal) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                ForEach(rows) { block in
                    if let row = block.tableRow {
                        GridRow { ForEach(row.cells.indices, id: \.self) { column in Text(row.cells[column]).font(row.isHeader ? .subheadline.weight(.semibold) : .subheadline).multilineTextAlignment(textAlignment(row.alignment(of: column))).frame(maxWidth: 260, alignment: Alignment(horizontal: alignment(row.alignment(of: column)), vertical: .top)).padding(.horizontal, 8).padding(.vertical, 4) } }
                        if row.isHeader { Divider().gridCellUnsizedAxes(.horizontal) }
                    }
                }
            }.padding(6).background(ViewPalette.night.opacity(0.5), in: .rect(cornerRadius: 12))
            VStack(alignment: .leading, spacing: 8) {
                ForEach(rows.filter { $0.tableRow?.isHeader == false || rows.count == 1 }) { block in
                    if let row = block.tableRow {
                        VStack(alignment: .leading, spacing: 8) { ForEach(row.cells.indices, id: \.self) { index in VStack(alignment: .leading, spacing: 2) { if let header = row.header[safe: index] { Text(header).font(.caption.weight(.semibold)).foregroundStyle(.secondary) }; Text(row.cells[index]).font(.subheadline) } } }.padding(12).frame(maxWidth: .infinity, alignment: .leading).background(ViewPalette.night.opacity(0.5), in: .rect(cornerRadius: 12))
                    }
                }
            }
        }.padding(.leading, CGFloat(rows.first?.depth ?? 0) * 18)
    }
    private func alignment(_ alignment: MarkdownBlock.ColumnAlignment) -> HorizontalAlignment { switch alignment { case .leading: .leading; case .center: .center; case .trailing: .trailing } }
    private func textAlignment(_ alignment: MarkdownBlock.ColumnAlignment) -> TextAlignment { switch alignment { case .leading: .leading; case .center: .center; case .trailing: .trailing } }
}
private extension Array where Element == AttributedString { subscript(safe index: Int) -> Element? { indices.contains(index) ? self[index] : nil } }

public struct ContentRefView: View {
    public let content: ContentRef
    public var onOpen: (Link) -> Void
    public var onLoad: ((ContentRef) -> Void)?
    public init(content: ContentRef, onOpen: @escaping (Link) -> Void = { _ in }, onLoad: ((ContentRef) -> Void)? = nil) { self.content = content; self.onOpen = onOpen; self.onLoad = onLoad }
    public var body: some View {
        Button { if let onLoad { onLoad(content) } else { onOpen(.url(url: content.fetch)) } } label: { Label(content.title, systemImage: content.kind == .diff ? "arrow.left.arrow.right" : "doc.text.magnifyingglass").font(.subheadline.weight(.medium)).frame(maxWidth: .infinity, alignment: .leading).padding(12).background(ViewPalette.night.opacity(0.5), in: .rect(cornerRadius: 12)) }
            .buttonStyle(.plain).accessibilityHint("\(content.size) bytes")
    }
}

public struct FactsView: View {
    public let facts: Facts
    public var onLink: (Link) -> Void
    public init(facts: Facts, onLink: @escaping (Link) -> Void = { _ in }) { self.facts = facts; self.onLink = onLink }
    public var body: some View { VStack(alignment: .leading, spacing: 7) { ForEach(Array(facts.items.enumerated()), id: \.offset) { _, fact in HStack(alignment: .firstTextBaseline) { Text(fact.label).font(.caption.weight(.semibold)).foregroundStyle(.secondary); Spacer(minLength: 12); Text(MarkdownBlock.line(fact.value)).font(.subheadline).foregroundStyle(fact.tone.map(toneColor) ?? ViewPalette.ink).multilineTextAlignment(.trailing) }.contentShape(Rectangle()).onTapGesture { if let link = fact.link { onLink(link) } } } }.padding(12).background(ViewPalette.night.opacity(0.5), in: .rect(cornerRadius: 12)) }
}

public struct ChecksView: View {
    public let checks: Checks
    public var onLink: (Link) -> Void
    public init(checks: Checks, onLink: @escaping (Link) -> Void = { _ in }) { self.checks = checks; self.onLink = onLink }
    public var body: some View { VStack(alignment: .leading, spacing: 6) { ForEach(Array(checks.items.enumerated()), id: \.offset) { _, check in Label(check.name, systemImage: symbol(check.state)).font(.footnote).foregroundStyle(color(check.state)) } }.frame(maxWidth: .infinity, alignment: .leading).padding(12).background(ViewPalette.night.opacity(0.5), in: .rect(cornerRadius: 12)).contentShape(Rectangle()).onTapGesture { if let link = checks.link { onLink(link) } } }
    private func symbol(_ state: CheckState) -> String { switch state { case .pass: "checkmark.circle.fill"; case .fail: "xmark.circle.fill"; case .pending: "clock" } }
    private func color(_ state: CheckState) -> Color { switch state { case .pass: ViewPalette.sage; case .fail: ViewPalette.rose; case .pending: .secondary } }
}

public struct TimelineView: View {
    public let timeline: Timeline
    public init(timeline: Timeline) { self.timeline = timeline }
    public var body: some View { VStack(alignment: .leading, spacing: 0) { ForEach(Array(timeline.entries.enumerated()), id: \.offset) { index, entry in HStack(alignment: .top, spacing: 10) { VStack(spacing: 0) { Circle().fill(entry.tone.map(toneColor) ?? ViewPalette.dawn).frame(width: 8, height: 8); if index < timeline.entries.count - 1 { Rectangle().fill(ViewPalette.hairline).frame(width: 2).frame(minHeight: 30) } }; VStack(alignment: .leading, spacing: 3) { Text(entry.title).font(.subheadline.weight(.semibold)); if let text = entry.text { Text(text).font(.footnote).foregroundStyle(.secondary) }; Text(ViewsDate.display(entry.at)).font(.caption2).foregroundStyle(.tertiary) }.padding(.bottom, 12) } } }.frame(maxWidth: .infinity, alignment: .leading) }
}

public struct NoticeView: View {
    public let notice: Notice
    public var onAction: (Action) -> Void
    public init(notice: Notice, onAction: @escaping (Action) -> Void = { _ in }) { self.notice = notice; self.onAction = onAction }
    public var body: some View { HStack(alignment: .top, spacing: 10) { Image(systemName: notice.tone == .critical || notice.tone == .warning ? "exclamationmark.triangle.fill" : "info.circle.fill").foregroundStyle(toneColor(notice.tone)); VStack(alignment: .leading, spacing: 6) { Text(notice.text).font(.callout); if let action = notice.action { Button(action.label) { onAction(action) }.font(.footnote.weight(.semibold)).disabled(!action.enabled); DisabledReason(action).font(.caption).foregroundStyle(.secondary) } } }.padding(12).frame(maxWidth: .infinity, alignment: .leading).background(toneColor(notice.tone).opacity(0.10), in: .rect(cornerRadius: 12)) }
}

public struct ActionBarView: View {
    public let bar: ActionBar
    public var onAction: (Action) -> Void
    public var onLink: (Link) -> Void
    public var excludingActionIDs: Set<String> = []
    public init(bar: ActionBar, onAction: @escaping (Action) -> Void = { _ in }, onLink: @escaping (Link) -> Void = { _ in }, excludingActionIDs: Set<String> = []) {
        self.bar = bar; self.onAction = onAction; self.onLink = onLink; self.excludingActionIDs = excludingActionIDs
    }
    public var body: some View { ActionButtons(actions: bar.actions.filter { $0.id.map { !excludingActionIDs.contains($0) } ?? true }, onAction: onAction, onLink: onLink) }
}

private struct ActionButtons: View {
    let actions: [Action]
    let onAction: (Action) -> Void
    let onLink: (Link) -> Void
    var body: some View { ViewThatFits(in: .horizontal) { HStack(spacing: 8) { buttons }; VStack(alignment: .leading, spacing: 8) { buttons } } }
    private var buttons: some View { ForEach(Array(actions.enumerated()), id: \.offset) { _, action in
        VStack(alignment: .leading, spacing: 4) {
            Button {
                if let link = action.link { onLink(link) } else { onAction(action) }
            } label: {
                Text(action.label).font(.subheadline.weight(.semibold))
                    .frame(maxWidth: .infinity).padding(.vertical, 10).padding(.horizontal, 12)
            }
            .buttonStyle(.borderedProminent)
            .tint(action.style == .destructive ? ViewPalette.rose : action.style == .primary ? ViewPalette.dawn : ViewPalette.slate)
            .disabled(!action.enabled)
            DisabledReason(action).font(.caption).foregroundStyle(.secondary)
        }
    } }
}

public struct OptionGrid: View {
    public let options: [ActionOption]
    public var selection: String?
    public var onSelect: (ActionOption) -> Void
    public init(options: [ActionOption], selection: String? = nil, onSelect: @escaping (ActionOption) -> Void = { _ in }) { self.options = options; self.selection = selection; self.onSelect = onSelect }
    public var body: some View { LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), alignment: .leading)], alignment: .leading, spacing: 8) { ForEach(options, id: \.id) { option in Button { onSelect(option) } label: { VStack(alignment: .leading, spacing: 3) { Text(MarkdownBlock.line(option.label)).font(.subheadline.weight(.semibold)); if let detail = option.description { Text(MarkdownBlock.line(detail)).font(.caption).foregroundStyle(.secondary).lineLimit(2) } }.frame(maxWidth: .infinity, minHeight: 44, alignment: .leading).padding(10).background(selection == option.id ? ViewPalette.dawn.opacity(0.14) : ViewPalette.night.opacity(0.5), in: .rect(cornerRadius: 10)) }.buttonStyle(.plain).overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(selection == option.id ? ViewPalette.dawn : ViewPalette.hairline)) } } }
}

public struct FormView: View {
    public let form: WireForm
    public var onSubmit: (Action) -> Void
    public var onOpenForm: ((WireForm) -> Void)?
    public init(form: WireForm, onSubmit: @escaping (Action) -> Void = { _ in }, onOpenForm: ((WireForm) -> Void)? = nil) { self.form = form; self.onSubmit = onSubmit; self.onOpenForm = onOpenForm }
    public var body: some View { VStack(alignment: .leading, spacing: 12) { ForEach(form.fields, id: \.id) { field in VStack(alignment: .leading, spacing: 5) { Text(field.label).font(.subheadline.weight(.semibold)); fieldValue(field); if let help = field.help { Text(help).font(.caption).foregroundStyle(.secondary) } } }; Button(form.submit.label) { if let onOpenForm { onOpenForm(form) } else { onSubmit(form.submit) } }.buttonStyle(.borderedProminent).disabled(!form.submit.enabled); DisabledReason(form.submit).font(.caption).foregroundStyle(.secondary) }.padding(14).background(ViewPalette.card, in: .rect(cornerRadius: 16)).overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(ViewPalette.hairline)) }
    @ViewBuilder private func fieldValue(_ field: Field) -> some View { switch field.kind { case let .text(placeholder), let .multiline(placeholder), let .lines(placeholder): TextField(placeholder ?? "", text: .constant(field.value.stringValue ?? ""), axis: .vertical).textFieldStyle(.roundedBorder); case .toggle: Toggle(field.label, isOn: .constant(field.value.boolValue ?? false)).labelsHidden(); case let .choice(items): VStack(alignment: .leading, spacing: 5) { ForEach(items, id: \.id) { item in Label(item.label, systemImage: "circle") } }; case .attachments: Label("Attachments", systemImage: "paperclip").foregroundStyle(.secondary); case .picker(let source): Text(source).foregroundStyle(.secondary); case .date, .time: Text(field.value.stringValue ?? "").foregroundStyle(.secondary); case let .stepper(min, max): Text("\(field.value.intValue ?? min) (\(min)–\(max))").foregroundStyle(.secondary) } }
}
private extension JSONValue {
    var stringValue: String? { if case let .string(value) = self { return value }; return nil }
    var boolValue: Bool? { if case let .bool(value) = self { return value }; return nil }
    var intValue: Int64? { if case let .integer(value) = self { return value }; return nil }
}

public struct GaugeView: View {
    public let gauge: WireGauge
    public init(gauge: WireGauge) { self.gauge = gauge }
    public var body: some View { VStack(alignment: .leading, spacing: 7) { HStack { Text(gauge.label).font(.caption.weight(.semibold)); Spacer(); Text(gauge.text).font(.caption.monospacedDigit()).foregroundStyle(.secondary) }; ProgressView(value: min(max(gauge.value, 0), 1)).tint(levelColor); if let detail = gauge.detail { Text(detail).font(.caption2).foregroundStyle(.secondary) } }.padding(12).background(ViewPalette.night.opacity(0.5), in: .rect(cornerRadius: 12)) }
    private var levelColor: Color { switch gauge.level { case .calm: ViewPalette.sage; case .warm: ViewPalette.dawn; case .hot: ViewPalette.rose } }
}
