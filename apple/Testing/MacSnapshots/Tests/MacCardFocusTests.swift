import AlondraContract
import AlondraFeatures
import AlondraMacWorkspace
import AppKit
import ComposableArchitecture
import SwiftUI
import XCTest

/// A card's reply field on the Mac keeps the keyboard and its draft while the card's view arrives
/// again from the daemon, also when another card lands above it.
@MainActor
final class MacCardFocusTests: XCTestCase {
    override func setUp() async throws {
        _ = NSApplication.shared
    }

    private func document(cards: [String]) throws -> ViewDocument {
        let environment = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: try XCTUnwrap(environment["TEST_SRCDIR"]))
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
        let data = try Data(contentsOf: root.appendingPathComponent("fixtures/views/cards/card-question-mac.json"))
        var object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        var sections = try XCTUnwrap(object["sections"] as? [[String: Any]])
        let template = try XCTUnwrap((sections[0]["items"] as? [[String: Any]])?.first)
        sections[0]["items"] = try cards.map { name in
            var card = template
            card["id"] = name
            card["need"] = "Decide \(name)"
            var reply = try XCTUnwrap(card["reply"] as? [String: Any])
            var input = try XCTUnwrap(reply["input"] as? [String: Any])
            input["placeholder"] = "Reply to \(name)"
            reply["input"] = input
            card["reply"] = reply
            return card
        }
        object["sections"] = sections
        return try ContractCodec.decode(ViewDocument.self, from: JSONSerialization.data(withJSONObject: object))
    }

    func test_replyKeepsFocusAndDraftOnItsCardWhenTheViewRefreshesWithACardAbove() async throws {
        let first = try document(cards: ["beta"])
        var state = MacPresentationFeature.State()
        state.detailDocument = first
        state.selection = .viewKey(first.key)
        state.selectedViewKey = first.key
        let store = Store(initialState: state) {
            Reduce<MacPresentationFeature.State, MacPresentationFeature.Action> { state, action in
                if case let .viewLoaded(_, .document(document)) = action { state.detailDocument = document }
                return .none
            }
        }

        let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: 700, height: 900), styleMask: [.titled],
                              backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = NSHostingView(rootView: MacServiceDetail(presentation: store))
        window.makeKeyAndOrderFront(nil)
        defer { window.close() }
        try await settle()

        let field = try XCTUnwrap(textField(placeholder: "Reply to beta", in: window))
        XCTAssertTrue(window.makeFirstResponder(field))
        try await settle()
        field.currentEditor()?.insertText("Only if the title stays on one line")
        try await settle()

        store.send(.viewLoaded(first.key, .document(try document(cards: ["beta"]))))
        try await settle()
        XCTAssertEqual(focusedField(in: window)?.placeholderString, "Reply to beta", "a refresh took the keyboard away")

        store.send(.viewLoaded(first.key, .document(try document(cards: ["alpha", "beta"]))))
        try await settle()
        let focused = focusedField(in: window)
        XCTAssertEqual(focused?.placeholderString, "Reply to beta", "the keyboard moved to another card")
        XCTAssertEqual(textField(placeholder: "Reply to beta", in: window)?.stringValue, "Only if the title stays on one line")
        XCTAssertEqual(textField(placeholder: "Reply to alpha", in: window)?.stringValue, "", "the draft moved to the new card")
        print("[focus-proof] mac after refresh with a card above: focused=\(focused?.placeholderString ?? "none"), beta draft=\"\(textField(placeholder: "Reply to beta", in: window)?.stringValue ?? "")\", alpha draft=\"\(textField(placeholder: "Reply to alpha", in: window)?.stringValue ?? "")\"")
    }

    private func settle() async throws {
        try await Task.sleep(for: .milliseconds(500))
    }

    private func focusedField(in window: NSWindow) -> NSTextField? {
        if let editor = window.firstResponder as? NSTextView, let field = editor.delegate as? NSTextField { return field }
        return window.firstResponder as? NSTextField
    }

    private func textField(placeholder: String, in window: NSWindow) -> NSTextField? {
        func search(_ view: NSView) -> NSTextField? {
            if let field = view as? NSTextField, field.placeholderString == placeholder { return field }
            return view.subviews.lazy.compactMap(search).first
        }
        return window.contentView.flatMap(search)
    }
}
