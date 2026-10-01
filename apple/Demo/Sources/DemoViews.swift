import AlondraViews
import AlondraViewsDependency
import Foundation

/// Bundled, network-free Views documents for Demo. The fixture documents use the same
/// protocol DTOs as the real relay so the shipping Views UI runs unchanged.
enum DemoViews {
    static func client() -> ViewsClient {
        ViewsClient(
            load: { key, known in
                if let fixture = fixtureName(for: key.rawValue) {
                    return try loadFixture(fixture, key: key.rawValue, known: known)
                }
                return try loadGenerated(key.rawValue, known: known)
            },
            open: { _ in },
            openScreen: { _, _ in },
            closeScreen: { _ in },
            feed: { AsyncStream { $0.finish() } },
            perform: { _ in try response() },
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
        if key.hasPrefix("pull_request:") { return "card-pr" }
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
        if key.hasPrefix("pull_request:") { document = try pullRequestDocument(from: document, key: key) }
        document["key"] = key
        document["view"] = key.split(separator: "?", maxSplits: 1).first.map(String.init) ?? key
        document["client"] = key == "sidebar" ? "mac" : "ios"
        document["data_epoch"] = "demo-views"
        document["cursor"] = "demo-views-cursor"
        let stableVersion = key.utf8.reduce(UInt64(14_695_981_039_346_656_037)) { ($0 ^ UInt64($1)) &* 1_099_511_628_211 }
        document["version"] = "demo-" + String(stableVersion, radix: 16)
        let decoded = try ViewsCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: document))
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
            return try Data(contentsOf: root.appendingPathComponent("\(name).json"))
        }
        if let bundled = Bundle.main.url(forResource: name, withExtension: "json", subdirectory: "fixtures/views")
            ?? Bundle.main.url(forResource: name, withExtension: "json", subdirectory: "views")
            ?? Bundle.main.url(forResource: name, withExtension: "json") {
            return try Data(contentsOf: bundled)
        }
        return try Data(contentsOf: fixtureRoot().appendingPathComponent("\(name).json"))
    }

    private static func loadGenerated(_ key: String, known: String?) throws -> ViewLoad {
        let base = try JSONSerialization.jsonObject(with: fixtureData("home-ios")) as! [String: Any]
        let canonicalKey = key.split(separator: "?", maxSplits: 1).first.map(String.init) ?? key
        let context = try questionContext()
        let task = context.first { (($0["icon"] as? String) == "task") }
        let mission = context.first { (($0["icon"] as? String) == "mission") }
        let node = context.first { (($0["icon"] as? String) == "node") }
        let sections: [[String: Any]]
        let title: String
        switch canonicalKey {
        case "tasks":
            title = "Tasks"
            sections = [["id": "tasks", "title": "Tasks", "collapsed": false,
                         "items": [row(from: task, icon: "task"), row(from: mission, icon: "request")].compactMap { $0 }]]
        case "fleet":
            title = "Fleet"
            sections = [["id": "fleet", "title": "Agents", "collapsed": false,
                         "items": [row(from: node, icon: "node")].compactMap { $0 }]]
        case "new_request", "new_request?":
            title = "New request"
            sections = [newRequestSection()]
        case let detail where detail.hasPrefix("pull_request:"):
            let fixture = try JSONSerialization.jsonObject(with: fixtureData("card-pr")) as! [String: Any]
            let document = try pullRequestDocument(from: fixture, key: key)
            return try decode(document, key: key, title: nil, known: known)
        case let detail where detail.hasPrefix("node:"):
            title = node?["text"] as? String ?? "Node"
            sections = [factsSection(title: "Node", value: title)]
        case let detail where detail.hasPrefix("task:"):
            title = task?["text"] as? String ?? "Task"
            sections = [factsSection(title: "Task", value: title)]
        case let detail where detail.hasPrefix("request:"):
            title = mission?["text"] as? String ?? "Request"
            sections = [factsSection(title: "Request", value: title)]
        case "machine":
            title = "Demo Mac"
            sections = [factsSection(title: "Machine", value: "Sample data")]
        default:
            throw ViewsClientError.missingView
        }
        return try decode(base, key: key, title: title, sections: sections, known: known)
    }

    private static func decode(_ source: [String: Any], key: String, title: String?, sections: [[String: Any]]? = nil,
                               known: String?) throws -> ViewLoad {
        var document = source
        document["key"] = key
        document["view"] = key.split(separator: "?", maxSplits: 1).first.map(String.init) ?? key
        document["client"] = "ios"
        document["data_epoch"] = "demo-views"
        document["cursor"] = "demo-views-cursor"
        document["title"] = title
        if let sections { document["sections"] = sections }
        let stableVersion = key.utf8.reduce(UInt64(14_695_981_039_346_656_037)) { ($0 ^ UInt64($1)) &* 1_099_511_628_211 }
        document["version"] = "demo-" + String(stableVersion, radix: 16)
        let view = try ViewsCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: document))
        if known == view.version { return .unchanged(cursor: view.cursor) }
        return .document(view)
    }

    private static func questionContext() throws -> [[String: Any]] {
        let data = try fixtureData("card-question")
        let document = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        let sections = document["sections"] as? [[String: Any]] ?? []
        let items = sections.flatMap { $0["items"] as? [[String: Any]] ?? [] }
        let card = items.first { $0["type"] as? String == "card" } ?? [:]
        return card["context"] as? [[String: Any]] ?? []
    }

    private static func row(from context: [String: Any]?, icon: String) -> [String: Any]? {
        guard let context,
              let text = context["text"] as? String,
              let link = context["link"] as? [String: Any],
              let key = link["key"] as? String else { return nil }
        let id = String(key.split(separator: ":").last ?? "")
        return ["type": "row", "id": id, "link": link, "icon": icon, "title": text,
                "depth": 0, "struck": false]
    }

    private static func factsSection(title: String, value: String) -> [String: Any] {
        ["id": "detail", "title": title, "collapsed": false,
         "items": [["type": "facts", "items": [["label": title, "value": value], ["label": "Source", "value": "Sample data"]]]]]
    }

    private static func newRequestSection() -> [String: Any] {
        ["id": "new_request", "title": "New request", "collapsed": false, "items": [[
            "type": "form", "id": "new_request", "fields": [
                ["id": "text", "label": "What would you like done?", "kind": ["kind": "multiline", "placeholder": "Describe the work"], "value": "", "required": true, "reload_on_change": false],
                ["id": "project", "label": "Project", "kind": ["kind": "choice", "items": [["id": "Demo Garden", "label": "Demo Garden"]]], "value": "Demo Garden", "required": false, "reload_on_change": false]
            ],
            "submit": ["id": "new_request:submit@demo", "label": "Send Request", "style": "primary", "presentation": "button", "input": ["kind": "none"], "attachments": true, "enabled": true]
        ]]]
    }

    private static func pullRequestDocument(from source: [String: Any], key: String) throws -> [String: Any] {
        var document = source
        let sections = source["sections"] as? [[String: Any]] ?? []
        let items = sections.flatMap { $0["items"] as? [[String: Any]] ?? [] }
        guard let card = items.first(where: { $0["type"] as? String == "card" }) else { throw ViewsClientError.missingView }
        let actions = (card["actions"] as? [[String: Any]] ?? []).map { action -> [String: Any] in
            var action = action
            action.removeValue(forKey: "link")
            return action
        }
        let prID = String(key.dropFirst("pull_request:".count))
        let title = card["title"] as? String ?? prID
        document["title"] = title
        document["sections"] = [
            ["id": "summary", "title": title, "collapsed": false,
             "items": [["type": "facts", "items": [["label": "Repository", "value": "example/repo"], ["label": "Base", "value": "main"]]]]],
            ["id": "actions", "collapsed": false, "items": [["type": "actions", "actions": actions]]],
            ["id": "diff", "title": "Diff", "collapsed": false,
             "items": [["type": "content_ref", "kind": "diff", "title": "Diff", "size": 0,
                         "digest": "demo-head-1", "fetch": "/pull-requests/\(prID)/diff", "commentable": true]]]
        ]
        return document
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
        try ViewsCodec.decode(ActionOutcome.self, from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Saved in the demo","revision":2}"#.utf8))
    }
}
