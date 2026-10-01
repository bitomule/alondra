import SwiftUI

public extension EnvironmentValues {
    /// The instant "2 h ago" is measured against. Unset, it is the moment the text is drawn;
    /// snapshots pin it so a baseline never ages.
    @Entry var referenceDate: Date?
}

/// "2 h ago", or "now" inside the first minute.
struct RelativeTime: View {
    let date: Date
    @Environment(\.referenceDate) private var referenceDate
    @Environment(\.locale) private var locale

    var body: some View {
        let now = referenceDate ?? .now
        if abs(date.timeIntervalSince(now)) < 45 {
            Text("now")
        } else {
            Text(verbatim: Self.phrase(for: date, relativeTo: now, locale: locale))
        }
    }

    static func phrase(for date: Date, relativeTo now: Date, locale: Locale) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        formatter.dateTimeStyle = .named
        formatter.locale = locale
        return formatter.localizedString(for: date, relativeTo: now)
    }
}
