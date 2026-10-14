import AlondraContract
import Foundation
import XCTest

/// The daemon's own tests compare what it renders with `fixtures/views/cards/*.json`. These tests make
/// the same files the Swift contract: a renamed section or field breaks the build here instead of
/// leaving an empty screen on the phone.
final class FixtureContractTests: XCTestCase {
    private func fixtureURLs() throws -> [URL] {
        let environment = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
            .appendingPathComponent("fixtures/views/cards")
        return try FileManager.default.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
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

    /// The apps draw a card's buttons as they come: the server already chose the three, left out
    /// what cannot be used, painted each one and put words only under «Allow always».
    func testEveryCardButtonComesReadyToPaint() throws {
        var checked = 0
        for url in try fixtureURLs() {
            let document = try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: url))
            for case let .card(card) in document.sections.flatMap(\.items) {
                let name = "\(url.lastPathComponent) \(card.id)"
                XCTAssertLessThanOrEqual(card.actions.count, 3, name)
                for action in card.actions + card.more + (card.reply.map { [$0] } ?? []) {
                    XCTAssertTrue(action.enabled, "\(name): \(action.label)")
                    XCTAssertNotNil(action.role, "\(name): \(action.label)")
                    XCTAssertEqual(action.role == .risk, action.style == .destructive, "\(name): \(action.label)")
                    if action.description != nil { XCTAssertEqual(action.optionId, "always", "\(name): \(action.label)") }
                }
                checked += card.actions.count
            }
        }
        XCTAssertGreaterThan(checked, 10, "the card fixtures lost their buttons")
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

    private func macDocument(_ name: String) throws -> ViewDocument {
        let url = try XCTUnwrap(fixtureURLs().first { $0.lastPathComponent == "\(name).json" }, name)
        return try ContractCodec.decode(ViewDocument.self, from: Data(contentsOf: url))
    }

    func testMacFixturesAreRenderedForTheMac() throws {
        let names = ["sidebar-mac", "card-question-mac", "card-pr-mac", "pull-request-mac", "node-mac", "tasks-mac", "fleet-mac"]
        for name in names {
            XCTAssertEqual(try macDocument(name).client, .mac, name)
        }
    }

    func testMacSidebarListsWhatWaitsForYouAsCardLinks() throws {
        let sidebar = try macDocument("sidebar-mac")
        XCTAssertEqual(sidebar.view, "sidebar")
        let waiting = try XCTUnwrap(sidebar.sections.first { $0.id == "waiting" })
        let links = waiting.items.compactMap { item -> String? in
            guard case let .row(row) = item, case let .view(key)? = row.link else { return nil }
            return key
        }
        XCTAssertEqual(links.count, waiting.items.count)
        XCTAssertTrue(links.allSatisfy { $0.hasPrefix("card:") })
    }

    func testMacCardActionsCarryKeyboardShortcuts() throws {
        let card = try macDocument("card-pr-mac")
        let actions = card.sections.flatMap(\.items).flatMap { item -> [AlondraContract.Action] in
            if case let .card(card) = item { card.actions } else { [] }
        }
        XCTAssertFalse(actions.isEmpty)
        XCTAssertTrue(actions.contains { $0.key != nil }, "the Mac client is the one that gets shortcuts")
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
