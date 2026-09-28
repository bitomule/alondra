import AlondraAPIClient
import AlondraNotificationActions
import AlondraNotificationPayload
import CryptoKit
import Foundation
import Testing

@MainActor struct NotificationRoutingCoordinatorTests {
    @Test func inactiveTapSelectsAuthenticatedMacAndRejectsForgedHint() async throws {
        let a = try Fixture(1), b = try Fixture(2)
        let router = NotificationRoutingCoordinator()
        try router.install([a.entry(), b.entry()], selected: b.binding)
        let approved = try router.authorize(a.wire(), nowMilliseconds: 1000)
        #expect(approved.item.inboxID == "q42")
        #expect(router.selected == b.binding)
        try await router.activate(approved) { binding in
            #expect(binding == a.binding)
            try router.install([a.entry(), b.entry()], selected: binding)
        }
        #expect(router.selected == a.binding)
        #expect(throws: (any Error).self) { try router.authorize(a.wire(hint: b.binding), nowMilliseconds: 1000) }
        #expect(throws: (any Error).self) { try router.authorize(a.wire(inner: b.binding), nowMilliseconds: 1000) }
        #expect(throws: (any Error).self) { try router.authorize(a.wire(), nowMilliseconds: 3000) }
    }

    @Test func commandCapturedOnAStaysOnAWhileSelectionChangesToB() async throws {
        let a = try Fixture(1), b = try Fixture(2), gate = Gate()
        var destinations: [String] = []
        let entryA = a.entry { _, item in
            await MainActor.run { destinations.append("A:\(item)") }
            await gate.pause()
            return .confirmed
        }
        let entryB = b.entry { _, item in
            await MainActor.run { destinations.append("B:\(item)") }
            return .confirmed
        }
        let router = NotificationRoutingCoordinator()
        try router.install([entryA, entryB], selected: a.binding)
        let approved = try router.authorize(a.wire(), nowMilliseconds: 1000)
        let task = Task { await router.submit(approved, actionID: NotificationPayload.Action.moreContext, text: nil) }
        await gate.waitForPause()
        try router.install([entryA, entryB], selected: b.binding)
        await gate.resume()
        let result = await task.value
        guard case .confirmed? = result else { Issue.record("A lost its accepted result"); return }
        #expect(destinations == ["A:q42"])
        #expect(router.selected == b.binding)
    }

    @Test func logoutDuringActivationCannotRestoreRouteAndOldCapabilityCannotSurviveRelogin() async throws {
        let a = try Fixture(1), b = try Fixture(2), gate = Gate()
        let router = NotificationRoutingCoordinator()
        try router.install([a.entry(), b.entry()], selected: b.binding)
        let approved = try router.authorize(a.wire(), nowMilliseconds: 1000)
        let task = Task {
            do {
                try await router.activate(approved) { _ in await gate.pause() }
                Issue.record("Logged-out notification became navigable")
            } catch {}
        }
        await gate.waitForPause()
        router.clear()
        try router.install([a.entry()], selected: a.binding)
        await gate.resume()
        await task.value
        #expect(!router.isValid(approved))
        #expect(await router.submit(approved, actionID: NotificationPayload.Action.moreContext, text: nil) == nil)
    }

    @Test func revokedBindingAndOtherAccountAreRejectedWithoutLosingCurrentSnapshot() throws {
        let a = try Fixture(1), b = try Fixture(2), foreign = try Fixture(3, owner: "another-user")
        let router = NotificationRoutingCoordinator()
        try router.install([a.entry(), b.entry()], selected: a.binding)
        let approved = try router.authorize(b.wire(), nowMilliseconds: 1000)
        #expect(throws: (any Error).self) { try router.install([a.entry(), foreign.entry()], selected: a.binding) }
        #expect(router.isValid(approved))
        #expect(throws: (any Error).self) { try router.authorize(foreign.wire(), nowMilliseconds: 1000) }
        try router.install([a.entry()], selected: a.binding)
        #expect(!router.isValid(approved))
        #expect(throws: (any Error).self) { try router.authorize(b.wire(), nowMilliseconds: 1000) }
    }

    private struct Fixture {
        let binding: NotificationPayload.Binding
        let signer = P256.Signing.PrivateKey()
        let recipient = P256.KeyAgreement.PrivateKey()
        let mac: String
        let owner: String
        init(_ n: UInt8, owner: String = "user") throws {
            binding = try #require(NotificationPayload.Binding(Data(repeating: n, count: 32).base64EncodedString()
                .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")))
            mac = "mac-\(n)"; self.owner = owner
        }
        func entry(submit: @escaping @Sendable (NotificationAction, String) async -> CommandResult = { _, _ in .confirmed }) -> NotificationRoutingCoordinator.Entry {
            let scope = NotificationBindingScope(relayURL: URL(string: "https://relay.example")!, ownerSub: owner,
                macID: mac, deviceID: "phone", credentialRevision: mac)
            return .init(client: ServiceNotificationClient(identity: mac, binding: binding, register: { _, _, _, _, _ in }, submit: submit),
                context: NotificationDecryptionContext(macID: mac, deviceID: "phone", binding: binding.value,
                    macSigningKey: signer.publicKey.rawRepresentation, agreementPrivateKey: recipient.rawRepresentation, scope: scope))
        }
        func wire(hint: NotificationPayload.Binding? = nil, inner: NotificationPayload.Binding? = nil) throws -> Data {
            let payload = try JSONSerialization.data(withJSONObject: ["push_binding": (inner ?? binding).value, "with_sound": false,
                "payload": ["aps": ["alert": "Synthetic"], "alondra": ["kind": "question", "id": 42]]])
            let envelope = try EnvelopeV1.seal(route: mac, sender: mac, recipient: "phone", requestID: UUID().uuidString,
                kind: "push", expiresAtMilliseconds: 2000, plaintext: payload, senderKey: signer, recipientKey: recipient.publicKey)
            return try JSONSerialization.data(withJSONObject: ["alondra": ["push_binding": (hint ?? binding).value,
                "envelope": JSONSerialization.jsonObject(with: JSONEncoder().encode(envelope))]])
        }
    }
    private actor Gate {
        var waiter: CheckedContinuation<Void, Never>?
        var entered = false
        var observer: CheckedContinuation<Void, Never>?
        func pause() async {
            await withCheckedContinuation { continuation in
                waiter = continuation; entered = true; observer?.resume(); observer = nil
            }
        }
        func waitForPause() async { if !entered { await withCheckedContinuation { observer = $0 } } }
        func resume() { waiter?.resume(); waiter = nil }
    }
}
