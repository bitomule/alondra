import Foundation
import OSLog

/// The app's own log lines since `since`, read from this process. `OSLogStore` gives this process
/// its values without the private mark, so each line keeps the code's text and its raw values for
/// `AppReportSection` to redact; the connection's lines are left out because its measurements go
/// whole.
public enum AppLogs {
    static let subsystems = ["com.bitomule.alondra", "dev.davidcollado.alondra"]

    public static func recent(since: Date) -> [AppLogEntry] {
        guard let store = try? OSLogStore(scope: .currentProcessIdentifier),
              let entries = try? store.getEntries(at: store.position(date: since),
                                                  matching: NSPredicate(format: "subsystem IN %@", subsystems)) else { return [] }
        return entries.compactMap { $0 as? OSLogEntryLog }
            .filter { $0.category != "connection" }
            .map { entry in
                AppLogEntry(date: entry.date, category: entry.category, level: level(entry.level),
                            components: entry.components.map(component))
            }
    }

    private static func component(_ component: OSLogMessageComponent) -> AppLogEntry.Component {
        let argument: AppLogEntry.Component.Argument = switch component.argumentCategory {
        case .string: .string(component.argumentStringValue ?? "")
        case .int64: .number(String(component.argumentInt64Value))
        case .uInt64: .number(String(component.argumentUInt64Value))
        case .double: .number(String(component.argumentDoubleValue))
        case .data: .data
        case .undefined: component.placeholder.isEmpty ? .none : .unknown
        @unknown default: .unknown
        }
        return AppLogEntry.Component(text: component.formatSubstring, argument: argument)
    }

    private static func level(_ level: OSLogEntryLog.Level) -> String {
        switch level {
        case .debug: "debug"
        case .info: "info"
        case .notice: "notice"
        case .error: "error"
        case .fault: "fault"
        case .undefined: "undefined"
        @unknown default: "undefined"
        }
    }
}
