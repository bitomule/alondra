import AlondraAPIClient
import AlondraViews
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
        let approved = try router.authorize(a.wire())
        #expect(approved.item.inboxID == "interaction:q-42")
        #expect(router.selected == b.binding)
        try await router.activate(approved) { binding in
            #expect(binding == a.binding)
            try router.install([a.entry(), b.entry()], selected: binding)
        }
        #expect(router.selected == a.binding)
        #expect(throws: (any Error).self) { try router.authorize(a.wire(hint: b.binding)) }
        #expect(throws: (any Error).self) { try router.authorize(a.wire(inner: b.binding)) }
        #expect(try router.authorize(a.wire()).item.inboxID == "interaction:q-42", "A late push still opens its item")
    }

    @Test func commandCapturedOnAStaysOnAWhileSelectionChangesToB() async throws {
        let a = try Fixture(1), b = try Fixture(2), gate = Gate()
        var destinations: [String] = []
        let entryA = a.entry { request in
            await MainActor.run { destinations.append("A:\(request.actionId)") }
            await gate.pause()
            return .noLongerPending
        }
        let entryB = b.entry { request in
            await MainActor.run { destinations.append("B:\(request.actionId)") }
            return .noLongerPending
        }
        let router = NotificationRoutingCoordinator()
        try router.install([entryA, entryB], selected: a.binding)
        let approved = try router.authorize(a.wire())
        let task = Task { await router.submit(approved, actionID: "A0", text: nil) }
        await gate.waitForPause()
        try router.install([entryA, entryB], selected: b.binding)
        await gate.resume()
        let result = await task.value
        guard case .noLongerPending? = result else { Issue.record("A lost its result"); return }
        #expect(destinations == ["A:ctx:q-42@1"])
        #expect(router.selected == b.binding)
    }

    @Test func logoutDuringActivationCannotRestoreRouteAndOldCapabilityCannotSurviveRelogin() async throws {
        let a = try Fixture(1), b = try Fixture(2), gate = Gate()
        let router = NotificationRoutingCoordinator()
        try router.install([a.entry(), b.entry()], selected: b.binding)
        let approved = try router.authorize(a.wire())
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
        #expect(await router.submit(approved, actionID: "A0", text: nil) == nil)
    }

    @Test func revokedBindingAndOtherAccountAreRejectedWithoutLosingCurrentSnapshot() throws {
        let a = try Fixture(1), b = try Fixture(2), foreign = try Fixture(3, owner: "another-user")
        let router = NotificationRoutingCoordinator()
        try router.install([a.entry(), b.entry()], selected: a.binding)
        let approved = try router.authorize(b.wire())
        #expect(throws: (any Error).self) { try router.install([a.entry(), foreign.entry()], selected: a.binding) }
        #expect(router.isValid(approved))
        #expect(throws: (any Error).self) { try router.authorize(foreign.wire()) }
        try router.install([a.entry()], selected: a.binding)
        #expect(!router.isValid(approved))
        #expect(throws: (any Error).self) { try router.authorize(b.wire()) }
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
        func entry(submit: @escaping @Sendable (ActionRequest) async -> NotificationActionResult = { _ in .noLongerPending }) -> NotificationRoutingCoordinator.Entry {
            let scope = NotificationBindingScope(relayURL: URL(string: "https://relay.example")!, ownerSub: owner,
                macID: mac, deviceID: "phone", credentialRevision: mac)
            return .init(client: ServiceNotificationClient(identity: mac, binding: binding, register: { _, _, _, _, _ in }, submit: submit),
                context: NotificationDecryptionContext(macID: mac, deviceID: "phone", binding: binding.value,
                    macSigningKey: signer.publicKey.rawRepresentation, agreementPrivateKey: recipient.rawRepresentation, scope: scope))
        }
        func wire(hint: NotificationPayload.Binding? = nil, inner: NotificationPayload.Binding? = nil) throws -> Data {
            let payload = try JSONSerialization.data(withJSONObject: ["push_binding": (inner ?? binding).value, "with_sound": false,
                "payload": ["aps": ["alert": "Synthetic"], "alondra": ["protocol": 1, "kind": "question", "card": "q-42", "data_epoch": "e", "actions": [["id": "ctx:q-42@1", "label": "Need context"]]]]])
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
