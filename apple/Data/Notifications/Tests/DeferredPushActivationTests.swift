@testable import AlondraNotifications
import Testing

@MainActor
private final class PermissionGate {
    let entered: AsyncStream<Void>
    private let signal: AsyncStream<Void>.Continuation
    private var waiter: CheckedContinuation<Void, Never>?
    init() { (entered, signal) = AsyncStream.makeStream() }
    func request() async {
        await withCheckedContinuation { waiter = $0; signal.yield(()) }
    }
    func release() { waiter?.resume(); waiter = nil }
}

@MainActor
struct DeferredPushActivationTests {
    @Test func logoutFencesAnUncooperativePermissionReply() async {
        let activation = DeferredPushActivation(), permission = PermissionGate()
        var registered = false
        var entered = permission.entered.makeAsyncIterator()
        activation.start { isCurrent in
            await permission.request()
            if isCurrent() { registered = true }
        }
        _ = await entered.next()
        activation.cancel()
        permission.release()
        await activation.drain()
        #expect(!registered)
    }

    @Test func replacementCoalescesBehindThePhysicalPromptAndRunsOnlyTheLatestSession() async {
        let activation = DeferredPushActivation(), permission = PermissionGate()
        var registered: [String] = []
        var entered = permission.entered.makeAsyncIterator()
        activation.start { isCurrent in
            await permission.request()
            if isCurrent() { registered.append("old") }
        }
        _ = await entered.next()
        activation.start { isCurrent in if isCurrent() { registered.append("superseded") } }
        activation.start { isCurrent in if isCurrent() { registered.append("latest") } }
        #expect(registered.isEmpty)
        permission.release()
        await activation.drain()
        #expect(registered == ["latest"])
    }
}
