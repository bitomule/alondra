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
            start.press(forDuration: 0.05, thenDragTo: start.withOffset(CGVector(dx: dx, dy: 0)), withVelocity: .fast, thenHoldForDuration: 0)
            XCTAssertTrue(done(of: next).waitForExistence(timeout: 5), "the swipe did not bring the next card to the front")
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
        XCTAssertTrue(later.waitForExistence(timeout: 5))
        later.tap()
        XCTAssertTrue(done(of: back).waitForExistence(timeout: 5))
        done(of: back).tap()
        try expectAnswers("\(back):done")
    }

    private func done(of card: String) -> XCUIElement {
        app.buttons["triage.action.\(card).done"]
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
