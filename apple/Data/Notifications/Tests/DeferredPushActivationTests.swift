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
private final class FakePushCenter: PushAuthorizing {
    enum Permission { case notDetermined, denied, authorized }
    var permission: Permission
    private(set) var asked = 0
    private(set) var registered = 0
    init(_ permission: Permission) { self.permission = permission }
    var isAuthorized: Bool { permission == .authorized }
    func refreshAuthorization() async {}
    func registerWithAPNs() { registered += 1 }
    func activate(whileCurrent isCurrent: @escaping @MainActor () -> Bool) async {
        asked += 1
        permission = .authorized
        registerWithAPNs()
    }
}

@MainActor
struct DeferredPushActivationTests {
    @Test func connectingNeverAsksForAlertsThePersonHasNotAllowed() async {
        for permission in [FakePushCenter.Permission.notDetermined, .denied] {
            let activation = DeferredPushActivation(), push = FakePushCenter(permission)
            let commit = await activation.prepareRenewal(push) { true }
            commit()
            await activation.drain()
            #expect(push.asked == 0)
            #expect(push.registered == 0)
        }
    }

    @Test func connectingRenewsTheRegistrationOfAlertsAlreadyAllowed() async {
        let activation = DeferredPushActivation(), push = FakePushCenter(.authorized)
        let commit = await activation.prepareRenewal(push) { true }
        commit()
        await activation.drain()
        #expect(push.registered == 1)
    }

    @Test func aSessionReplacedBeforeItsRenewalRunsDoesNotRegister() async {
        let activation = DeferredPushActivation(), push = FakePushCenter(.authorized)
        var committed = true
        let commit = await activation.prepareRenewal(push) { committed }
        committed = false
        commit()
        await activation.drain()
        #expect(push.registered == 0)
    }

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
