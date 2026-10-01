import AlondraContract
import Foundation
import XCTest

/// The daemon's own tests compare what it renders with `fixtures/views/*.json`. These tests make
/// the same files the Swift contract: a renamed section or field breaks the build here instead of
/// leaving an empty screen on the phone.
final class FixtureContractTests: XCTestCase {
    private static let notViews: Set<String> = ["push-payloads.json"]

    private func fixtureURLs() throws -> [URL] {
        let environment = ProcessInfo.processInfo.environment
        let directory = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views")
        return try FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" && !Self.notViews.contains($0.lastPathComponent) }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    func testEveryViewFixtureDecodesIntoItsDocument() throws {
        let urls = try fixtureURLs()
        XCTAssertGreaterThanOrEqual(urls.count, 15, "the fixture directory is the contract; it was found empty")
        for url in urls {
            let name = url.lastPathComponent
            let raw = try Data(contentsOf: url)
            let document = try ContractCodec.decode(ViewDocument.self, from: raw)
            let object = try XCTUnwrap(JSONSerialization.jsonObject(with: raw) as? [String: Any], name)
            XCTAssertEqual(document.key, object["key"] as? String, name)
            XCTAssertEqual(document.view, object["view"] as? String, name)
            XCTAssertFalse(document.status.summary.isEmpty, name)
            XCTAssertEqual(
                document.sections.map(\.id),
                (object["sections"] as? [[String: Any]])?.compactMap { $0["id"] as? String },
                "\(name): every section survives decoding in order"
            )
        }
    }

    func testEveryViewFixtureSurvivesAReencode() throws {
        for url in try fixtureURLs() {
            let document = try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: url))
            let again = try ContractCodec.decode(ViewDocument.self, from: ContractCodec.encode(document))
            XCTAssertEqual(again, document, url.lastPathComponent)
        }
    }

    func testEveryLinkedViewKeyIsOneTheAppKnowsHowToOpen() throws {
        let known = [
            "card:", "task:", "request:", "node:", "pull_request:", "document:", "schedule:",
            "home", "tasks", "fleet", "machine", "schedules", "new_request", "sidebar", "issue_picker",
        ]
        var seen = Set<String>()
        for url in try fixtureURLs() {
            let object = try JSONSerialization.jsonObject(with: Data(contentsOf: url))
            collectViewKeys(in: object, into: &seen)
        }
        XCTAssertFalse(seen.isEmpty, "no linked keys found: the walker is not reaching the links")
        for key in seen {
            XCTAssertTrue(known.contains { key == $0 || key.hasPrefix($0) }, "no screen opens \(key)")
        }
    }

    private func collectViewKeys(in value: Any, into keys: inout Set<String>) {
        if let object = value as? [String: Any] {
            if object["kind"] as? String == "view", let key = object["key"] as? String {
                keys.insert(key)
            }
            object.values.forEach { collectViewKeys(in: $0, into: &keys) }
        } else if let array = value as? [Any] {
            array.forEach { collectViewKeys(in: $0, into: &keys) }
        }
    }
}
