import AlondraLiveActivity
import SwiftUI
#if canImport(UIKit)
import UIKit
#else
import AppKit
#endif

/// Every presentation of the Live Activity, as plain SwiftUI so tests can draw each one outside
/// ActivityKit. The strings live in the `LiveActivity` table, bundled by the widget extension.
public enum ActivityPalette {
    public static let accent = Color(light: 0xC2410C, dark: 0xFF7A45)
    public static let lockScreenBackground = Color(light: 0xF6F3EE, dark: 0x1C1C1E)
    static let outdatedOpacity = 0.55
    static let inboxSymbol = "tray.full.fill"
    static let waitingSymbol = "hourglass"
    static let workingSymbol = "gearshape.2.fill"
}

/// The lock screen: the three numbers, and a line under them when something needs saying.
public struct ActivityLockScreenView: View {
    let summary: ActivitySummary

    public init(summary: ActivitySummary) { self.summary = summary }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Image(systemName: "sunrise.fill").foregroundStyle(ActivityPalette.accent)
                Text(verbatim: "Alondra").font(.subheadline.weight(.semibold))
            }
            HStack(alignment: .firstTextBaseline) {
                CountColumn(value: summary.shown(\.inbox), label: Text("Inbox", tableName: "LiveActivity"))
                CountColumn(value: summary.shown(\.waiting), label: Text("Waiting", tableName: "LiveActivity"))
                CountColumn(value: summary.shown(\.working), label: Text("Working", tableName: "LiveActivity"))
            }
            .opacity(summary.isOutdated ? ActivityPalette.outdatedOpacity : 1)
            if let notice = summary.notice {
                NoticeLine(notice: notice).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(16)
    }
}

/// A count, or a dash while none is known.
private struct CountText: View {
    let value: Int?

    var body: some View {
        if let value {
            Text(value, format: .number).monospacedDigit().contentTransition(.numericText(value: Double(value)))
        } else {
            Text(verbatim: "–")
        }
    }
}

private extension ActivitySummary {
    func shown(_ count: KeyPath<ActivityCounts, Int>) -> Int? { isKnown ? totals[keyPath: count] : nil }

    var inboxLabel: Text {
        isKnown ? Text("Inbox \(totals.inbox)", tableName: "LiveActivity") : Text("Inbox", tableName: "LiveActivity")
    }
    var waitingLabel: Text {
        isKnown ? Text("Waiting \(totals.waiting)", tableName: "LiveActivity") : Text("Waiting", tableName: "LiveActivity")
    }
    var workingLabel: Text {
        isKnown ? Text("Working \(totals.working)", tableName: "LiveActivity") : Text("Working", tableName: "LiveActivity")
    }
}

private struct CountColumn: View {
    let value: Int?
    let label: Text

    var body: some View {
        VStack(spacing: 2) {
            CountText(value: value).font(.title.weight(.semibold))
            label.font(.caption).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
    }
}

/// The line under the numbers: which Mac has gone quiet, that none has news, or that all is done.
public struct NoticeLine: View {
    let notice: ActivitySummary.Notice

    public init(notice: ActivitySummary.Notice) { self.notice = notice }

    public var body: some View {
        switch notice {
        case .noNewsFromAnyMac(let since):
            Text("No news from your Macs since \(since, format: .dateTime.hour().minute())", tableName: "LiveActivity")
        case .noNewsFromMac(let name?, let since):
            Text("No news from \(name) since \(since, format: .dateTime.hour().minute())", tableName: "LiveActivity")
        case .noNewsFromMac(nil, let since):
            Text("No news from a Mac since \(since, format: .dateTime.hour().minute())", tableName: "LiveActivity")
        case .openTheApp:
            Text("Open Alondra to see your activity", tableName: "LiveActivity")
        case .macsNotCounted(let count):
            Text("+\(count) Macs not counted", tableName: "LiveActivity")
        case .allCaughtUp:
            Text("All caught up", tableName: "LiveActivity")
        }
    }
}

/// The compact island's left side: Waiting for You.
public struct ActivityCompactLeading: View {
    let summary: ActivitySummary

    public init(summary: ActivitySummary) { self.summary = summary }

    public var body: some View {
        SymbolCount(symbol: ActivityPalette.inboxSymbol, value: summary.shown(\.inbox))
            .accessibilityLabel(summary.inboxLabel)
    }
}

/// The compact island's right side: what is working.
public struct ActivityCompactTrailing: View {
    let summary: ActivitySummary

    public init(summary: ActivitySummary) { self.summary = summary }

    public var body: some View {
        SymbolCount(symbol: ActivityPalette.workingSymbol, value: summary.shown(\.working))
            .accessibilityLabel(summary.workingLabel)
    }
}

/// Beside another app's Live Activity: Waiting for You while it has cards, otherwise what works.
public struct ActivityMinimalView: View {
    let summary: ActivitySummary

    public init(summary: ActivitySummary) { self.summary = summary }

    public var body: some View {
        if summary.totals.inbox > 0 {
            Text(summary.totals.inbox, format: .number).font(.footnote.weight(.semibold)).monospacedDigit()
                .contentTransition(.numericText(value: Double(summary.totals.inbox)))
                .foregroundStyle(ActivityPalette.accent)
                .accessibilityLabel(summary.inboxLabel)
        } else {
            SymbolCount(symbol: ActivityPalette.workingSymbol, value: summary.shown(\.working), spacing: 1)
                .font(.caption2.weight(.semibold))
                .accessibilityLabel(summary.workingLabel)
        }
    }
}

/// The island when its width is limited, as in landscape: only Waiting for You, in a circle.
public struct ActivityLimitedWidthView: View {
    let summary: ActivitySummary

    public init(summary: ActivitySummary) { self.summary = summary }

    public var body: some View {
        CountText(value: summary.shown(\.inbox)).font(.caption.weight(.bold))
            .minimumScaleFactor(0.6)
            .foregroundStyle(.black)
            .frame(width: 28, height: 28)
            .background(Circle().fill(ActivityPalette.accent))
            .accessibilityLabel(summary.inboxLabel)
    }
}

public struct ActivityExpandedLeading: View {
    let summary: ActivitySummary

    public init(summary: ActivitySummary) { self.summary = summary }

    public var body: some View {
        ExpandedCount(symbol: ActivityPalette.inboxSymbol, value: summary.shown(\.inbox),
                      label: Text("Inbox", tableName: "LiveActivity"))
    }
}

public struct ActivityExpandedTrailing: View {
    let summary: ActivitySummary

    public init(summary: ActivitySummary) { self.summary = summary }

    public var body: some View {
        ExpandedCount(symbol: ActivityPalette.workingSymbol, value: summary.shown(\.working),
                      label: Text("Working", tableName: "LiveActivity"), alignment: .trailing)
    }
}

public struct ActivityExpandedCenter: View {
    public init() {}

    public var body: some View {
        Text(verbatim: "Alondra").font(.headline)
    }
}

/// Under the expanded island: what waits and when it was updated, or the line that needs saying.
public struct ActivityExpandedBottom: View {
    let summary: ActivitySummary

    public init(summary: ActivitySummary) { self.summary = summary }

    public var body: some View {
        Group {
            switch summary.notice {
            case .some(.allCaughtUp), nil:
                if let updatedAt = summary.updatedAt {
                    Text("Waiting \(summary.totals.waiting) · updated \(updatedAt, format: .dateTime.hour().minute())",
                         tableName: "LiveActivity")
                } else {
                    Text("Waiting \(summary.totals.waiting)", tableName: "LiveActivity")
                }
            case .some(let notice):
                NoticeLine(notice: notice)
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }
}

/// CarPlay and the Apple Watch: the three numbers in one line.
public struct ActivitySmallView: View {
    let summary: ActivitySummary

    public init(summary: ActivitySummary) { self.summary = summary }

    public var body: some View {
        HStack(spacing: 10) {
            SymbolCount(symbol: ActivityPalette.inboxSymbol, value: summary.shown(\.inbox))
                .accessibilityLabel(summary.inboxLabel)
            SymbolCount(symbol: ActivityPalette.waitingSymbol, value: summary.shown(\.waiting))
                .accessibilityLabel(summary.waitingLabel)
            SymbolCount(symbol: ActivityPalette.workingSymbol, value: summary.shown(\.working))
                .accessibilityLabel(summary.workingLabel)
        }
        .font(.headline)
        .padding(10)
    }
}

private struct SymbolCount: View {
    let symbol: String
    let value: Int?
    var spacing: CGFloat = 4

    var body: some View {
        HStack(spacing: spacing) {
            Image(systemName: symbol).foregroundStyle(ActivityPalette.accent)
            CountText(value: value)
        }
    }
}

private struct ExpandedCount: View {
    let symbol: String
    let value: Int?
    let label: Text
    var alignment = HorizontalAlignment.leading

    var body: some View {
        VStack(alignment: alignment, spacing: 2) {
            Image(systemName: symbol).font(.caption).foregroundStyle(ActivityPalette.accent)
                .frame(height: 16)
            CountText(value: value).font(.title2.weight(.semibold))
            label.font(.caption2).foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
        .padding(.horizontal, 4)
    }
}

private extension Color {
    init(light: UInt32, dark: UInt32) {
        #if canImport(UIKit)
        self.init(uiColor: UIColor { $0.userInterfaceStyle == .dark ? UIColor(hex: dark) : UIColor(hex: light) })
        #else
        self.init(nsColor: NSColor(name: nil) { $0.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? NSColor(hex: dark) : NSColor(hex: light) })
        #endif
    }
}

#if canImport(UIKit)
private extension UIColor {
    convenience init(hex: UInt32) {
        self.init(red: CGFloat(hex >> 16 & 0xFF) / 255, green: CGFloat(hex >> 8 & 0xFF) / 255, blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
    }
}
#else
private extension NSColor {
    convenience init(hex: UInt32) {
        self.init(srgbRed: CGFloat(hex >> 16 & 0xFF) / 255, green: CGFloat(hex >> 8 & 0xFF) / 255, blue: CGFloat(hex & 0xFF) / 255, alpha: 1)
    }
}
#endif
