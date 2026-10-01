import AlondraContract
import SwiftUI

public extension EnvironmentValues {
    /// Whether diff lines are syntax coloured. Colouring finishes after the first frame, so a
    /// snapshot turns it off to capture one stable image.
    @Entry var highlightsDiffs = true
}

/// A server timestamp as the daemon asked for it: relative ("2 min ago") or a date and time.
struct TimeText: View {
    let raw: String
    var format: TimeFormat = .relative
    @Environment(\.locale) private var locale

    var body: some View {
        if let date = Self.date(raw) {
            switch format {
            case .relative: RelativeTime(date: date)
            case .time: Text(verbatim: date.formatted(.dateTime.hour().minute().locale(locale)))
            case .absolute: Text(verbatim: date.formatted(.dateTime.day().month(.abbreviated).year().hour().minute().locale(locale)))
            }
        } else {
            Text(verbatim: raw)
        }
    }

    static func date(_ raw: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: raw) ?? ISO8601DateFormatter().date(from: raw)
    }
}
