import XCTest

/// Two «Do» cards in Waiting for You, touched through the system the way a person does: only a tap
/// on a button of the card in front answers, and it answers that card.
@MainActor
final class TriageTouchUITests: XCTestCase {
    private let front = "t-holepunch-ports-1"
    private let back = "t-holepunch-xcode-iphone-1"
    private var app: XCUIApplication!

    override func setUp() async throws {
        continueAfterFailure = false
        app = XCUIApplication()
        app.launch()
        XCTAssertTrue(done(of: front).waitForExistence(timeout: 20), "the front card never showed its Done button")
        expectInFront(front)
    }

    override func tearDown() async throws {
        app.terminate()
    }

    func test_tappingDoneAnswersOnlyTheFrontCard() throws {
        done(of: front).tap()
        try expectAnswers("\(front):done")
        XCTAssertTrue(done(of: back).waitForExistence(timeout: 5), "the card behind did not come to the front")
        try expectAnswers("\(front):done", after: .seconds(2))
    }

    func test_swipingAwayFromDoneAnswersNothing() throws {
        for (card, next, dx) in [(front, back, -260.0), (back, front, 260.0)] {
            let start = done(of: card).coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
            let received = trail().count
            start.press(forDuration: 0.05, thenDragTo: start.withOffset(CGVector(dx: dx, dy: 0)), withVelocity: .fast, thenHoldForDuration: 0)
            expectInFront(next, after: "dragEnded", since: received)
            try expectAnswers("none", after: .seconds(2))
        }
    }

    func test_aShortDragOnDoneThatSpringsBackAnswersNothing() throws {
        let start = done(of: front).coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        start.press(forDuration: 0.05, thenDragTo: start.withOffset(CGVector(dx: -40, dy: 0)), withVelocity: .slow, thenHoldForDuration: 0.1)
        try expectAnswers("none", after: .seconds(2))
        XCTAssertTrue(done(of: front).isHittable, "the front card left on a drag too short to skip it")
    }

    func test_tappingWhereTheCardBehindPeeksAnswersNothing() throws {
        let need = app.staticTexts["Open ports 443 and 7842 on the router"].firstMatch
        XCTAssertTrue(need.waitForExistence(timeout: 5))
        let cardTop = need.frame.minY - 20
        for y in [cardTop - 4, cardTop - 10, cardTop - 16, cardTop - 21] {
            app.coordinate(withNormalizedOffset: .zero).withOffset(CGVector(dx: need.frame.midX, dy: y)).tap()
        }
        try expectAnswers("none", after: .seconds(2))
    }

    func test_afterLeavingTheFrontCardForLaterDoneAnswersTheNewFrontCard() throws {
        let later = app.buttons["triage.later.\(front)"]
        XCTAssertTrue(later.isHittable, "the front card has no Leave for Later to tap")
        let received = trail().count
        later.tap()
        expectInFront(back, after: "later", since: received)
        done(of: back).tap()
        try expectAnswers("\(back):done")
    }

    private func done(of card: String) -> XCUIElement {
        app.buttons["triage.action.\(card).done"]
    }

    /// Waits until `card` is in front, the stack has stopped moving and its Done button takes a tap.
    /// When it does not, the failure says how far the touch got, from what the stack received since.
    private func expectInFront(_ card: String, after action: String? = nil, since received: Int = 0) {
        let deadline = Date().addingTimeInterval(5)
        var stack = Stack(app.staticTexts["stack"].label)
        while Date() < deadline {
            if stack.front == card, stack.settled, done(of: card).isHittable { return }
            Thread.sleep(forTimeInterval: 0.1)
            stack = Stack(app.staticTexts["stack"].label)
        }
        let since = Array(stack.trail.dropFirst(received))
        let reason = if let action, !since.contains(where: { $0.hasPrefix(action) }) {
            "the \(action) never reached the stack"
        } else if !stack.settled {
            "the stack never settled"
        } else if stack.front != card {
            "the stack settled with \(stack.front) in front"
        } else {
            "\(card) is in front and settled, but its Done button cannot be tapped"
        }
        print("[touch-proof] \(name): \(reason)\n\(app.debugDescription)")
        XCTFail("\(card) did not come to the front: \(reason); the stack received \(since)")
    }

    private func trail() -> [String] {
        Stack(app.staticTexts["stack"].label).trail
    }

    private func expectAnswers(_ expected: String, after wait: Duration = .zero) throws {
        let label = app.staticTexts["answers"]
        XCTAssertTrue(label.waitForExistence(timeout: 5))
        if wait > .zero { Thread.sleep(forTimeInterval: Double(wait.components.seconds)) }
        let deadline = Date().addingTimeInterval(5)
        while label.label != expected, Date() < deadline { Thread.sleep(forTimeInterval: 0.2) }
        print("[touch-proof] \(name): answers=\(label.label)")
        XCTAssertEqual(label.label, expected)
    }
}

/// The host's `stack` label: `front=<card>;settled=<bool>;trail=<action>|<action>…`.
private struct Stack {
    let front: String
    let settled: Bool
    let trail: [String]

    init(_ label: String) {
        let fields = Dictionary(label.split(separator: ";").compactMap { field -> (String, String)? in
            let pair = field.split(separator: "=", maxSplits: 1).map(String.init)
            return pair.count == 2 ? (pair[0], pair[1]) : nil
        }, uniquingKeysWith: { first, _ in first })
        front = fields["front"] ?? "none"
        settled = fields["settled"] == "true"
        trail = fields["trail"].map { $0.split(separator: "|").map(String.init) } ?? []
    }
}
