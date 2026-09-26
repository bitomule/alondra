import AlondraServicePairing
import Foundation
import Testing

@MainActor struct PairingAttemptLeaseTests {
    @Test func anotherWindowCannotAcquireApproveOrCancelAnExistingAttempt() throws {
        let lease = PairingAttemptLease(), first = UUID(), second = UUID()
        let ticket = try lease.acquire(scope: first)
        #expect(throws: CancellationError.self) { try lease.acquire(scope: second) }
        #expect(lease.current(scope: second) == nil)
        #expect(lease.current(scope: nil) == nil)
        #expect(lease.contains(ticket))
        #expect(lease.release(ticket))
        let next = try lease.acquire(scope: second)
        #expect(!lease.release(ticket))
        #expect(lease.contains(next))
    }
    @Test func logoutInvalidatesInFlightTicketEvenIfSameSheetStartsAgain() throws {
        let lease = PairingAttemptLease(), scope = UUID()
        let old = try lease.acquire(scope: scope)
        lease.clear()
        let next = try lease.acquire(scope: scope)
        #expect(!lease.contains(old))
        #expect(!lease.release(old))
        #expect(lease.current(scope: scope) == next)
    }
}
