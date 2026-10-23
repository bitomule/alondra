import AlondraContract
import Foundation
import XCTest

/// The daemon's own rendered views, read from the shared fixtures.
enum Fixtures {
    static func directory() throws -> URL {
        let environment = ProcessInfo.processInfo.environment
        return URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/cards")
    }

    static func urls() throws -> [URL] {
        try FileManager.default.contentsOfDirectory(at: directory(), includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" && $0.lastPathComponent != "push-payloads.json" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    /// A card as a daemon newer than the app sends it: kinds of links, items, fields and values the app does not know.
    static func newerDaemonCard(spanish: Bool = false) throws -> ViewDocument {
        let url = try directory().deletingLastPathComponent()
            .appendingPathComponent("newer-daemon/\(spanish ? "card-es" : "card").json")
        return try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: url))
    }

    /// A fixture view, optionally pretending to be a later version of itself.
    static func document(_ name: String, version: String? = nil) throws -> ViewDocument {
        let data = try Data(contentsOf: directory().appendingPathComponent("\(name).json"))
        guard let version else { return try ContractCodec.decode(ViewDocument.self, from: data) }
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        object["version"] = version
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    /// The fleet and node views the daemon still renders but the iPhone no longer opens.
    private static let unopenedViews: Set<String> = ["fleet", "node"]

    /// Every key a view in the iPhone fixtures links to, including the "show more" pages.
    static func linkedKeys() throws -> Set<String> {
        var keys = Set<String>()
        func walk(_ value: Any) {
            if let object = value as? [String: Any] {
                if object["kind"] as? String == "view", let key = object["key"] as? String { keys.insert(key) }
                if let more = object["more"] as? [String: Any], let key = more["key"] as? String { keys.insert(key) }
                object.values.forEach(walk)
            } else if let array = value as? [Any] {
                array.forEach(walk)
            }
        }
        for url in try urls() where !url.lastPathComponent.hasSuffix("-tui.json") && !url.lastPathComponent.hasSuffix("-mac.json") {
            let fixture = try JSONSerialization.jsonObject(with: Data(contentsOf: url))
            if let view = (fixture as? [String: Any])?["view"] as? String, Self.unopenedViews.contains(view) { continue }
            walk(fixture)
        }
        return keys
    }
}
