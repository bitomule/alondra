@testable import AlondraPairing
import Foundation
import Testing

struct MacBrowserTests {
    @Test func aReadyBrowserEndsARefusalSavedByAnEarlierRun() {
        #expect(MacBrowser.leavesDenied(refusedDuringBrowse: false, saved: .denied))
    }

    @Test func aReadyBrowserEndsARefusalSeenInThisBrowse() {
        #expect(MacBrowser.leavesDenied(refusedDuringBrowse: true, saved: .unknown))
    }

    @Test func readyFollowedByARefusalLeavesAccessDenied() async {
        let denials = Counter()
        let confirmed = await MacBrowser.confirmsAllowed(denialCount: { denials.value }, sleep: { _ in denials.add() })
        #expect(!confirmed, "the refusal arrived after ready, inside the wait")
    }

    @Test func readyThatStaysReadyWithoutARefusalIsAllowed() async {
        let denials = Counter()
        #expect(await MacBrowser.confirmsAllowed(denialCount: { denials.value }, sleep: { _ in }))
    }

    @Test func aWaitThatIsCancelledNeverAllows() async {
        let denials = Counter()
        #expect(await !MacBrowser.confirmsAllowed(denialCount: { denials.value }, sleep: { _ in throw CancellationError() }))
    }

    @Test func aReadyBrowserSaysNothingWhenNoRefusalWasKnown() {
        #expect(!MacBrowser.leavesDenied(refusedDuringBrowse: false, saved: .unknown))
        #expect(!MacBrowser.leavesDenied(refusedDuringBrowse: false, saved: .allowed))
    }
}

private final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var value: Int { lock.withLock { count } }
    func add() { lock.withLock { count += 1 } }
}
