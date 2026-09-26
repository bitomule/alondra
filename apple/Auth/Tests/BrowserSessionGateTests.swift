import Foundation
import XCTest
import AlondraAuth

final class BrowserSessionGateTests: XCTestCase {
    func testOlderLoginCannotOpenBrowserAfterNewerLoginInvalidatedIt() async throws {
        let gate = await MainActor.run { BrowserSessionGate() }
        let admitted = await MainActor.run { () -> Bool in
            gate.invalidate(generation: 2)
            return gate.isSuperseded(1)
        }
        XCTAssertTrue(admitted)
        let oldStarted = expectation(description: "late A reached browser gate")
        let old = Task { @MainActor in
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
                let accepted = gate.begin(id: UUID(), generation: 1, cancel: {}, continuation: continuation)
                XCTAssertFalse(accepted)
                oldStarted.fulfill()
            }
        }
        await fulfillment(of: [oldStarted], timeout: 2)
        do { _ = try await old.value; XCTFail("Superseded A should not open") }
        catch is CancellationError { }
        let busy = await MainActor.run { gate.isBusy }
        XCTAssertFalse(busy)
    }

    func testLateCompletionAndCancellationFromALeaveBAlive() async throws {
        let gate = await MainActor.run { BrowserSessionGate() }
        let firstID = UUID()
        let secondID = UUID()
        let firstStarted = expectation(description: "first browser began")
        let secondStarted = expectation(description: "second browser began")
        let first = Task { @MainActor in
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
                XCTAssertTrue(gate.begin(id: firstID, generation: 1, cancel: {}, continuation: continuation))
                firstStarted.fulfill()
            }
        }
        await fulfillment(of: [firstStarted], timeout: 2)
        await MainActor.run { gate.invalidate(generation: 2) }
        let second = Task { @MainActor in
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<URL, Error>) in
                XCTAssertTrue(gate.begin(id: secondID, generation: 2, cancel: {}, continuation: continuation))
                secondStarted.fulfill()
            }
        }
        await fulfillment(of: [secondStarted], timeout: 2)

        let lateURL = URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak?code=old")!
        let currentURL = URL(string: "com.bitomule.alondra:/oauth2redirect/keycloak?code=new")!
        await MainActor.run {
            gate.finish(id: firstID, callback: lateURL, error: nil)
            gate.cancel(id: firstID)
            gate.invalidate(generation: 1)
            XCTAssertTrue(gate.isBusy)
            gate.finish(id: secondID, callback: currentURL, error: nil)
        }
        do { _ = try await first.value; XCTFail("A should remain cancelled") }
        catch is CancellationError { }
        let returned = try await second.value
        XCTAssertEqual(returned, currentURL)
    }
}
