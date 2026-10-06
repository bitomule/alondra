import AlondraAccount
import Foundation
import Testing

@MainActor private final class FakeBackgroundTime {
    var begun: [Int] = []
    var ended: [Int] = []
    var expiration: (@MainActor @Sendable () -> Void)?
    var available = true
    private var next = 1

    func begin(_ expiration: @escaping @MainActor @Sendable () -> Void) -> Int? {
        guard available else { return nil }
        self.expiration = expiration
        defer { next += 1 }
        begun.append(next)
        return next
    }

    func end(_ id: Int) { ended.append(id) }
}

@MainActor
struct PairedRefreshGrantTests {
    @Test func finishingEndsTheGrantOnceEvenIfTheSystemExpiresItLater() async {
        let system = FakeBackgroundTime()
        let grant = pairedRefreshGrant(begin: { system.begin($0) }, end: { system.end($0) })
        let end = await grant { Issue.record("Not expired") }
        await end()
        await end()
        #expect(system.begun == [1])
        #expect(system.ended == [1])
    }

    @Test func expiryEndsTheGrantAndCancelsTheRefreshOnce() async {
        let system = FakeBackgroundTime()
        let grant = pairedRefreshGrant(begin: { system.begin($0) }, end: { system.end($0) })
        let expirations = Counter()
        let end = await grant { expirations.increment() }
        system.expiration?()
        await end()
        #expect(expirations.value == 1)
        #expect(system.ended == [1])
    }

    @Test func unavailableBackgroundTimeIsNeverEnded() async {
        let system = FakeBackgroundTime()
        system.available = false
        let grant = pairedRefreshGrant(begin: { system.begin($0) }, end: { system.end($0) })
        let end = await grant {}
        await end()
        #expect(system.begun.isEmpty)
        #expect(system.ended.isEmpty)
    }
}

private final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var value: Int { lock.withLock { count } }
    func increment() { lock.withLock { count += 1 } }
}
