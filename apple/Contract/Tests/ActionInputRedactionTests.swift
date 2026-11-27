import AlondraContract
import Foundation
import XCTest

/// A key typed into an account's settings travels as an action's text: whatever prints an action,
/// a `dump`, a debugger or a log line, shows that it carries text, never the text.
final class ActionInputRedactionTests: XCTestCase {
    private let key = "AIzaSyD-example-0000000000000000000000"

    // spec: antigravity.apps/printing-an-action-hides-its-text
    func testNothingThatPrintsAnActionShowsItsText() {
        let request = ActionRequest(commandId: "00000000-0000-4000-8000-000000000001", dataEpoch: "epoch", client: .ios, lang: "en",
            actionId: "account_credential:studio@7~antigravity",
            input: ActionInput(text: key, comments: [ActionComment(anchor: "a", text: key)], fields: ["token": .string(key)], content: ["answer": .string(key)]))
        var dumped = ""
        dump(request, to: &dumped)
        for printed in [dumped, String(describing: request), String(reflecting: request), "\(request.input)", String(reflecting: request.input)] {
            XCTAssertFalse(printed.contains(key), printed)
        }
        XCTAssertTrue(dumped.contains("redacted"), dumped)
        XCTAssertTrue(dumped.contains("account_credential:studio@7~antigravity"), "the rest of the action still shows: \(dumped)")
        XCTAssertEqual(request.input.text, key, "the input itself still carries the key to send it")
    }
}
