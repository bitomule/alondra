import AlondraShared
import Foundation
import Testing

@Suite struct InboxRingTests {
    private func question(_ id: UInt64, at askedAt: Int64) -> InboxItem {
        let json = """
        {"id":\(id),"sessionId":"s1","title":"Q\(id)","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","askedAt":\(askedAt)}
        """
        return .question(try! JSONDecoder().decode(Question.self, from: Data(json.utf8)))
    }

    private func report(_ id: UInt64, at createdAt: Int64) -> InboxItem {
        let json = """
        {"id":\(id),"sessionId":"s1","title":"R\(id)","links":[],"attachments":[],"kind":"final","createdAt":\(createdAt)}
        """
        return .report(try! JSONDecoder().decode(Report.self, from: Data(json.utf8)))
    }

    private var five: [InboxItem] { (1...5).map { question($0, at: Int64($0) * 1000) } }

    private func front(_ ring: InboxRing, _ items: [InboxItem]) -> String? {
        ring.arranged(InboxRing.ordered(items)).first?.id
    }

    private func without(_ items: [InboxItem], _ ids: String...) -> [InboxItem] {
        items.filter { !ids.contains($0.id) }
    }

    @Test func orderIsQuestionsByAskedAtThenReportsByCreatedAt() {
        let items = [report(1, at: 500), question(2, at: 3000), question(1, at: 2000), report(2, at: 100)]
        #expect(InboxRing.ordered(items).map(\.id) == ["q1", "q2", "r2", "r1"])
    }

    @Test func resolvingAfterSwipingLeftShowsTheNextCard() {
        var ring = InboxRing()
        ring.move(by: 1, in: five)
        #expect(front(ring, five) == "q2")
        ring.resolve(five[1], in: five)
        #expect(front(ring, without(five, "q2")) == "q3")
    }

    @Test func resolvingAfterSwipingRightShowsTheCardAfterIt() {
        var ring = InboxRing()
        ring.move(by: 2, in: five)
        ring.move(by: -1, in: five)
        #expect(front(ring, five) == "q2")
        ring.resolve(five[1], in: five)
        #expect(front(ring, without(five, "q2")) == "q3")
    }

    @Test func swipingRightFromTheFirstWrapsToTheLast() {
        var ring = InboxRing()
        ring.move(by: -1, in: five)
        #expect(front(ring, five) == "q5")
    }

    @Test func resolvingTheLastCardWrapsToTheFirst() {
        var ring = InboxRing()
        ring.move(by: -1, in: five)
        ring.resolve(five[4], in: five)
        #expect(front(ring, without(five, "q5")) == "q1")
    }

    @Test func nextCardLeavingElsewhereAtTheSameTimeNeverShowsThePreviousOne() {
        var ring = InboxRing()
        ring.move(by: 3, in: five)
        ring.resolve(five[3], in: five)
        #expect(front(ring, without(five, "q4", "q5")) == "q1")
    }

    @Test func frontCardLeavingElsewhereShowsTheNextOne() {
        var ring = InboxRing()
        ring.move(by: 2, in: five)
        #expect(front(ring, without(five, "q3")) == "q4")
        ring.move(by: 2, in: five)
        #expect(front(ring, without(five, "q5")) == "q1")
    }

    @Test func earlierCardsLeavingOrArrivingDoNotMoveTheFront() {
        var ring = InboxRing()
        ring.move(by: 2, in: five)
        #expect(front(ring, without(five, "q1", "q2")) == "q3")
        let reports = (1...5).map { report($0, at: Int64($0) * 1000) }
        var onReports = InboxRing()
        onReports.move(by: 3, in: reports)
        #expect(front(onReports, reports + [question(9, at: 99_000)]) == "r4")
    }

    @Test func failedResolveBringsTheCardBackToTheFront() {
        var ring = InboxRing()
        ring.move(by: 1, in: five)
        ring.resolve(five[1], in: five)
        #expect(front(ring, five) == "q2")
    }

    @Test func resolvingACardThatIsNotInFrontKeepsTheFront() {
        var ring = InboxRing()
        ring.move(by: 1, in: five)
        ring.resolve(five[3], in: five)
        #expect(front(ring, without(five, "q4")) == "q2")
    }

    @Test func skippingFromAStaleCardMovesFromThatCard() {
        var ring = InboxRing()
        ring.move(from: five[2], by: 1, in: five)
        #expect(front(ring, five) == "q4")
    }
}
