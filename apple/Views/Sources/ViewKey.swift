import Foundation

public struct ViewKey: RawRepresentable, Hashable, Codable, Sendable {
    public let rawValue: String

    public init(rawValue: String) { self.rawValue = rawValue }

    public static let home = ViewKey(rawValue: "home")
    public static func focusedHome(_ id: String) -> ViewKey { ViewKey(rawValue: "home?focus=\(id)") }
    public static func card(_ id: String) -> ViewKey { ViewKey(rawValue: "card:\(id)") }
    public static let sidebar = ViewKey(rawValue: "sidebar")
    public static let tasks = ViewKey(rawValue: "tasks")
    public static func tasks(section: String, after: UInt) -> ViewKey { ViewKey(rawValue: "tasks?section=\(section)&after=\(after)") }
    public static func task(_ id: String) -> ViewKey { ViewKey(rawValue: "task:\(id)") }
    public static func request(_ id: String) -> ViewKey { ViewKey(rawValue: "request:\(id)") }
    public static func document(_ id: String) -> ViewKey { ViewKey(rawValue: "document:\(id)") }
    public static func pullRequest(_ id: String) -> ViewKey { ViewKey(rawValue: "pull_request:\(id)") }
    public static let fleet = ViewKey(rawValue: "fleet")
    public static func node(_ id: String) -> ViewKey { ViewKey(rawValue: "node:\(id)") }
    public static let schedules = ViewKey(rawValue: "schedules")
    public static func schedule(_ id: String) -> ViewKey { ViewKey(rawValue: "schedule:\(id)") }
    public static func newSchedule(project: String? = nil) -> ViewKey {
        ViewKey(rawValue: "schedule:new" + (project.map { "?project=\(formEncode($0))" } ?? ""))
    }
    public static func newRequest(project: String? = nil) -> ViewKey {
        ViewKey(rawValue: "new_request" + (project.map { "?project=\(formEncode($0))" } ?? ""))
    }
    public static func issuePicker(query: String? = nil, refresh: Bool = false) -> ViewKey {
        ViewKey(rawValue: refresh ? "issue_picker?refresh=1" : query.map { "issue_picker?q=\(formEncode($0))" } ?? "issue_picker")
    }
    public static func search(_ query: String = "") -> ViewKey { ViewKey(rawValue: "search?q=\(formEncode(query))") }
    public static let machine = ViewKey(rawValue: "machine")

    private static func formEncode(_ value: String) -> String {
        var result = ""
        for byte in value.utf8 {
            switch byte {
            case 65...90, 97...122, 48...57, 42, 45, 46, 95: result.append(Character(UnicodeScalar(byte)))
            case 32: result.append("+")
            default: result += String(format: "%%%02X", byte)
            }
        }
        return result
    }

    public init(from decoder: Decoder) throws {
        rawValue = try decoder.singleValueContainer().decode(String.self)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}
