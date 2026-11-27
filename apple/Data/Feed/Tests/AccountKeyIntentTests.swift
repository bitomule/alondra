@testable import AlondraFeed
import AlondraContract
import AlondraTestSupport
import AlondraTransport
import CryptoKit
import Foundation
import Testing

/// An Antigravity key typed into an account's settings travels as every action does: encrypted in
/// this device's pending answers until the Mac confirms it, sealed for the Mac on the wire, and
/// gone from the device once answered.
@Suite(.temporaryFiles) struct AccountKeyIntentTests {
    private let key = "AIzaSyD-example-0000000000000000000000"
    private let scope = ActionIntentScope(service: "mac", device: "phone", credentialRevision: "approval")
    private var request: ActionRequest {
        ActionRequest(commandId: "00000000-0000-4000-8000-000000000001", dataEpoch: "epoch", client: .ios, lang: "en",
            actionId: "account_credential:studio@7~antigravity", input: ActionInput(text: key))
    }

    private func bytes(under directory: URL) throws -> Data {
        let files = FileManager.default.enumerator(at: directory, includingPropertiesForKeys: nil)?.compactMap { $0 as? URL } ?? []
        return try files.filter { !$0.hasDirectoryPath }.reduce(into: Data()) { $0.append(try Data(contentsOf: $1)) }
    }

    // spec: antigravity.apps/outbox-keeps-the-key-encrypted
    @Test func theKeyIsOnlyEverOnDiskEncryptedAndLeavesWithTheAnswer() async throws {
        let directory = try TestFiles.directory()
        let store = try await ViewActionIntentStore(directory: directory, keyMaterial: Data(repeating: 17, count: 32), scope: scope)
        let prepared = try await store.prepareNew(request, generation: 7)
        guard case .created(let dispatched) = try await store.reserve(prepared.restoredRequest, body: prepared.body) else {
            Issue.record("a prepared key is claimed once"); return
        }
        #expect(dispatched.restoredRequest.input.text == key, "the pending answer still carries the key to resend it")
        let pending = try bytes(under: directory)
        #expect(!pending.isEmpty)
        #expect(pending.range(of: Data(key.utf8)) == nil, "the pending answer is encrypted on disk")
        let applied = try ContractCodec.decode(ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"applied","stamp":"Key saved","revision":2}"#.utf8))
        try await store.complete(dispatched, response: applied)
        let reopened = try await ViewActionIntentStore(directory: directory, keyMaterial: Data(repeating: 17, count: 32), scope: scope)
        #expect(reopened.restored.isEmpty, "an answered key is not kept to send again")
        #expect(try bytes(under: directory).range(of: Data(key.utf8)) == nil)
    }

    // spec: antigravity.apps/relay-sees-only-the-sealed-envelope
    @Test func theRelayCarriesTheKeyOnlyInsideTheSealedEnvelope() throws {
        let (device, deviceAgreement) = (P256.Signing.PrivateKey(), P256.KeyAgreement.PrivateKey())
        let (mac, macAgreement) = (P256.Signing.PrivateKey(), P256.KeyAgreement.PrivateKey())
        let codec = EnvelopeCodec(macID: "mac-1", deviceID: "phone-1", signing: device, agreement: deviceAgreement,
            macSigning: mac.publicKey, macAgreement: macAgreement.publicKey, now: { 1_000_000 })
        let body = try ContractCodec.encode(request)
        let envelope = try codec.sealRequest(requestID: "request-1", method: "POST", path: "/actions", body: body)
        let carried = try JSONEncoder().encode(envelope)
        #expect(carried.range(of: Data(key.utf8)) == nil, "what the relay sees never holds the key")
        let opened = try envelope.open(senderKey: device.publicKey, recipientKey: macAgreement, sender: "phone-1",
            recipient: "mac-1", route: "mac-1", nowMilliseconds: 1_000_000)
        let payload = try #require(try JSONSerialization.jsonObject(with: opened) as? [String: Any])
        let input = try #require((payload["body"] as? [String: Any])?["input"] as? [String: Any])
        #expect(input["text"] as? String == key, "only the Mac opens it, and finds the key as the action's text")
        #expect(String(decoding: opened, as: UTF8.self).components(separatedBy: key).count == 2, "the key appears once")
    }
}
