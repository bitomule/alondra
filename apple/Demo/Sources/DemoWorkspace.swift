import AlondraContract
import AlondraFeed
import Foundation

/// Bundled, network-free Views documents for Demo. The fixture documents use the same
/// protocol DTOs as the real relay so the shipping Views UI runs unchanged.
enum DemoWorkspace {
    static func client() -> ViewsClient {
        let load: @Sendable (ViewKey, String?) async throws -> ViewFetch = { key, known in
            guard let fixture = fixtureName(for: key.rawValue) else {
                throw ViewsClientError.missingView
            }
            return try loadFixture(fixture, key: key.rawValue, known: known)
        }
        return ViewsClient(
            load: load,
            open: { _ in },
            openScreen: { _, _ in },
            closeScreen: { _ in },
            feed: { AsyncStream { $0.finish() } },
            observe: { key in
                AsyncThrowingStream { continuation in
                    let task = Task {
                        do { continuation.yield(try await load(key, nil)); continuation.finish() }
                        catch { continuation.finish(throwing: error) }
                    }
                    continuation.onTermination = { _ in task.cancel() }
                }
            },
            reconnections: { AsyncStream { _ in } },
            prepare: { $0 },
            perform: { _ in try response() },
            undeliveredCards: { AsyncStream { $0.finish() } },
            setAsideAnswers: { nil },
            dismissSetAsideAnswers: {},
            fetchContent: { reference in
                if reference.kind == .diff {
                    return """
                    diff --git a/Welcome.swift b/Welcome.swift
                    --- a/Welcome.swift
                    +++ b/Welcome.swift
                    @@ -1 +1 @@
                    -let greeting = "Welcome to the dashboard"
                    +let greeting = "What needs your attention?"
                    """
                }
                return "# Sample document\n\nThis document exists only in the demo."
            },
            fetchAttachment: { _ in Data("Demo attachment".utf8) }
        )
    }

    private static func fixtureName(for key: String) -> String? {
        if key == "home" { return "home-ios" }
        if key == "sidebar" { return "home-tui" }
        if key == "tasks" || key.hasPrefix("tasks?") { return "tasks-ios" }
        if key == "fleet" { return "fleet-ios" }
        if key == "new_request" || key.hasPrefix("new_request?") { return "new-request-ios" }
        if key.hasPrefix("node:") { return "node-ios" }
        if key.hasPrefix("task:") { return "task-ios" }
        if key.hasPrefix("request:") { return "request-ios" }
        if key.hasPrefix("pull_request:") { return "pull-request-ios" }
        if key == "machine" { return "machine-ios" }
        if key == "schedules" { return "schedules-ios" }
        if key.hasPrefix("schedule:") { return "schedule-new-ios" }
        guard key.hasPrefix("card:") else { return nil }
        let cardID = String(key.dropFirst("card:".count))
        if cardID == "f12d034b-249e-4487-a89b-57de38c26372" { return "card-information" }
        if cardID.hasPrefix("hf-weekly-") { return "card-delivery" }
        if cardID.contains("/") && cardID.contains("#") { return "card-pr" }
        return "card-question"
    }

    private static func loadFixture(_ name: String, key: String, known: String?) throws -> ViewLoad {
        let fixture = try fixtureData(name)
        var object = try JSONSerialization.jsonObject(with: fixture)
        if key.hasPrefix("card:") { object = replacingCardID(in: object, with: String(key.dropFirst("card:".count))) }
        guard var document = object as? [String: Any] else { throw ViewsClientError.missingView }
        document["key"] = key
        document["client"] = key == "sidebar" ? "mac" : "ios"
        document["data_epoch"] = "demo-views"
        document["cursor"] = "demo-views-cursor"
        let stableVersion = key.utf8.reduce(UInt64(14_695_981_039_346_656_037)) { ($0 ^ UInt64($1)) &* 1_099_511_628_211 }
        document["version"] = "demo-" + String(stableVersion, radix: 16)
        let decoded = try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: document))
        if known == decoded.version { return .unchanged(cursor: decoded.cursor) }
        return .document(decoded)
    }

    private static func fixtureRoot() -> URL {
        let environment = ProcessInfo.processInfo.environment
        if let sourceDirectory = environment["TEST_SRCDIR"] {
            return URL(fileURLWithPath: sourceDirectory)
                .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
                .appendingPathComponent("fixtures/views")
        }
        return URL(fileURLWithPath: FileManager.default.currentDirectoryPath).appendingPathComponent("fixtures/views")
    }

    private static func fixtureData(_ name: String) throws -> Data {
        if let sourceDirectory = ProcessInfo.processInfo.environment["TEST_SRCDIR"] {
            let root = URL(fileURLWithPath: sourceDirectory)
                .appendingPathComponent(ProcessInfo.processInfo.environment["TEST_WORKSPACE"] ?? "_main")
                .appendingPathComponent("fixtures/views")
            return try firstExisting(root, name)
        }
        if let bundled = Bundle.main.url(forResource: name, withExtension: "json", subdirectory: "fixtures/views")
            ?? Bundle.main.url(forResource: name, withExtension: "json", subdirectory: "views")
            ?? Bundle.main.url(forResource: name, withExtension: "json") {
            return try Data(contentsOf: bundled)
        }
        return try firstExisting(fixtureRoot(), name)
    }

    private static func firstExisting(_ root: URL, _ name: String) throws -> Data {
        guard let data = try? Data(contentsOf: root.appendingPathComponent("cards/\(name).json")) else { throw ViewsClientError.missingView }
        return data
    }

    private static func replacingCardID(in value: Any, with newID: String) -> Any {
        switch value {
        case let string as String:
            guard string.contains("a98d034b-249e-4487-a89b-57de38c26372") else { return value }
            return string.replacingOccurrences(of: "a98d034b-249e-4487-a89b-57de38c26372", with: newID)
        case let array as [Any]: return array.map { replacingCardID(in: $0, with: newID) }
        case let dictionary as [String: Any]: return dictionary.mapValues { replacingCardID(in: $0, with: newID) }
        default: return value
        }
    }

    private static func response() throws -> ActionOutcome {
        try ContractCodec.decode(ActionOutcome.self, from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved in the demo","revision":2}"#.utf8))
    }
}
