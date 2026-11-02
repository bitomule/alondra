import AlondraContract
import Foundation
import SwiftUI

/// How long a task has run, counted on this device from the instant the daemon sent: "<1 min",
/// "12 min", "1 h 05 min", "2 d 3 h". Once it ended the total stays as it is.
public enum ElapsedTime {
    public static func text(seconds: Int, locale: Locale) -> String {
        let seconds = max(seconds, 0)
        switch seconds {
        case ..<60:
            return String(localized: "<1 min", table: "AlondraDesign", bundle: #bundle, locale: locale)
        case ..<3_600:
            return String(localized: "\(seconds / 60) min", table: "AlondraDesign", bundle: #bundle, locale: locale)
        case ..<86_400:
            let minutes = (seconds % 3_600 / 60).formatted(.number.precision(.integerLength(2...2)).locale(locale))
            return String(localized: "\(seconds / 3_600) h \(minutes) min", table: "AlondraDesign", bundle: #bundle, locale: locale)
        default:
            return String(localized: "\(seconds / 86_400) d \(seconds % 86_400 / 3_600) h", table: "AlondraDesign", bundle: #bundle, locale: locale)
        }
    }

    /// The label's text, with the time it counts in place of the daemon's fallback.
    public static func text(_ label: AlondraContract.Label, now: Date, locale: Locale) -> String {
        guard let elapsed = label.elapsed, let since = date(elapsed.since) else { return label.text }
        let end = elapsed.until.flatMap(date) ?? now
        return text(seconds: Int(end.timeIntervalSince(since)), locale: locale)
    }

    static func date(_ iso: String) -> Date? {
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return withFraction.date(from: iso) ?? ISO8601DateFormatter().date(from: iso)
    }

    static func counts(_ labels: [AlondraContract.Label]) -> Bool {
        labels.contains { $0.elapsed != nil && $0.elapsed?.until == nil }
    }
}

/// One label as text, counting on its own every minute while its span is open.
public struct ElapsedText: View {
    private let labels: [AlondraContract.Label]
    private let separator: String
    @Environment(\.locale) private var locale

    public init(_ label: AlondraContract.Label) { self.init([label], separator: "") }

    /// Several labels on one line, such as a sidebar row's "Working · Lumo · 12 min".
    public init(_ labels: [AlondraContract.Label], separator: String = " · ") {
        self.labels = labels
        self.separator = separator
    }

    public var body: some View {
        if ElapsedTime.counts(labels) {
            SwiftUI.TimelineView(.periodic(from: .now, by: 60)) { _ in line }
        } else {
            line
        }
    }

    private var line: Text {
        let now = RelativeClock.now()
        return Text(verbatim: labels.map { ElapsedTime.text($0, now: now, locale: locale) }.joined(separator: separator))
    }
}
