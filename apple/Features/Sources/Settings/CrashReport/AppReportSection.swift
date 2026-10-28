import AlondraContract
import Foundation

/// The section an app adds to a crash report (`apps.jsonl`): the app, its connection, its log
/// lines redacted by the code's text around each value, and its connection measurements, which
/// hold nothing private by design. The Mac checks it for anything private like the rest.
public enum AppReportSection {
    /// What fits in one sealed request to the Mac with room to spare.
    public static let budget = 96 * 1024
    static let measurementsBudget = 32 * 1024
    static let pseudonymPrefix = "app-"

    public struct App: Equatable, Sendable {
        public var client: String
        public var version: String
        public var system: String
        public var connection: ConnectionState

        public init(client: String, version: String, system: String, connection: ConnectionState) {
            self.client = client; self.version = version; self.system = system; self.connection = connection
        }
    }

    public static func text(app: App, entries: [AppLogEntry], measurements: [String], budget: Int = budget) -> String {
        let vocabulary = Set(entries.flatMap { $0.components.flatMap { ReportShapes.tokens($0.text) } })
        var redactor = ReportRedactor(vocabulary: vocabulary, prefix: pseudonymPrefix)
        let header = line(["kind": "app", "client": app.client, "version": redactor.value(app.version, hint: .value),
                           "system": redactor.value(app.system, hint: .value), "connection": connectionWords(app.connection)])
        let measured = measurements.compactMap { text -> String? in
            guard let data = text.data(using: .utf8),
                  let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return nil }
            return line(["kind": "measurement", "measurement": object])
        }
        let keptMeasurements = newest(measured, within: min(measurementsBudget, budget / 3))
        let logs = entries.sorted { $0.date < $1.date }.map { entry in
            line(["kind": "log", "at": ReportShapes.utc(millis: Int((entry.date.timeIntervalSince1970 * 1_000).rounded(.down))),
                  "category": entry.category, "level": entry.level,
                  "message": message(entry, &redactor)])
        }
        let used = header.utf8.count + keptMeasurements.reduce(0) { $0 + $1.utf8.count + 1 } + 256
        let keptLogs = newest(logs, within: max(0, budget - used))
        var lines = [header] + keptLogs + keptMeasurements
        let omittedLogs = logs.count - keptLogs.count
        let omittedMeasurements = measured.count - keptMeasurements.count
        if omittedLogs > 0 || omittedMeasurements > 0 || measured.count < measurements.count {
            lines.append(line(["kind": "omitted", "oldest_log_lines": omittedLogs, "oldest_measurements": omittedMeasurements,
                               "measurements_not_json": measurements.count - measured.count]))
        }
        return lines.joined(separator: "\n") + "\n"
    }

    static func message(_ entry: AppLogEntry, _ redactor: inout ReportRedactor) -> String {
        var out = ""
        for component in entry.components {
            out += component.text
            switch component.argument {
            case .none: break
            case .string(let value): out += redactor.value(value, hint: .value)
            case .number(let value): out += ReportShapes.isNumber(value) ? value : redactor.pseudonym(.value, value)
            case .data: out += "<data>"
            case .unknown: out += "<value>"
            }
        }
        return out
    }

    /// The connection as words of the code: its state and its route or problem, never a Mac's name.
    static func connectionWords(_ state: ConnectionState) -> String {
        switch state {
        case .connecting: "connecting"
        case .connected(let route): "connected \(route.rawValue)"
        case .unreachable(let problem): "unreachable \(problemWord(problem))"
        }
    }

    private static func problemWord(_ problem: ConnectionProblem) -> String {
        switch problem {
        case .noInternet: "noInternet"
        case .serverUnreachable: "serverUnreachable"
        case .macNotAnswering: "macNotAnswering"
        case .alondraNotRunning: "alondraNotRunning"
        case .sessionExpired: "sessionExpired"
        case .accessNotConfirmed: "accessNotConfirmed"
        case .deviceRevoked: "deviceRevoked"
        case .macNotOnNetwork: "macNotOnNetwork"
        case .macChangedIdentity: "macChangedIdentity"
        case .localNetworkDenied: "localNetworkDenied"
        case .signOutIncomplete: "signOutIncomplete"
        case .deviceStorage: "deviceStorage"
        case .unexpected: "unexpected"
        }
    }

    private static func newest(_ lines: [String], within budget: Int) -> [String] {
        var total = 0
        var kept: [String] = []
        for line in lines.reversed() {
            total += line.utf8.count + 1
            guard total <= budget else { break }
            kept.append(line)
        }
        return kept.reversed()
    }

    private static func line(_ object: [String: Any]) -> String {
        let data = (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys, .withoutEscapingSlashes])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }
}
