import AlondraNotifications
import AlondraTransport
import CryptoKit
import Foundation
import XCTest

final class NotificationDecoderTests: XCTestCase {
    func testEncryptedPushAuthenticatesInnerBindingAndIgnoresForgedOuterAction() throws {
        let signer = P256.Signing.PrivateKey()
        let recipient = P256.KeyAgreement.PrivateKey()
        let binding = Data(repeating: 1, count: 32).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        let context = NotificationDecryptionContext(macID: "mac-1", deviceID: "client-1", binding: binding,
            macSigningKey: signer.publicKey.rawRepresentation, agreementPrivateKey: recipient.rawRepresentation)
        let plaintext = try JSONSerialization.data(withJSONObject: [
            "push_binding": binding, "with_sound": false,
            "payload": ["aps": ["alert": ["title": "Question", "body": "Approve?"], "sound": "default"],
                        "alondra": ["protocol": 1, "kind": "question", "card": "q-42", "data_epoch": "e", "actions": [["id": "ans:q-42@1", "option": "yes", "label": "Yes"]]]]
        ])
        let envelope = try Envelope.seal(route: "mac-1", sender: "mac-1", recipient: "client-1",
            requestID: "push-123", kind: "push", expiresAtMilliseconds: 2000, plaintext: plaintext,
            senderKey: signer, recipientKey: recipient.publicKey)
        let wireEnvelope = try JSONSerialization.jsonObject(with: JSONEncoder().encode(envelope))
        let wire = try JSONSerialization.data(withJSONObject: ["alondra": [
            "envelope": wireEnvelope, "push_binding": binding, "kind": "report", "id": 999
        ]])
        let decoded = try NotificationDecoder.decrypt(wire, context: context)
        let payload = try XCTUnwrap(JSONSerialization.jsonObject(with: decoded) as? [AnyHashable: Any])
        XCTAssertEqual(NotificationPayload.BoundItem(userInfo: payload)?.item.inboxID, "interaction:q-42",
                       "A push opened long after its envelope expired still names its item")
        XCTAssertNil((payload["aps"] as? [String: Any])?["sound"])
        let anotherDevice = NotificationDecryptionContext(macID: "mac-1", deviceID: "client-2", binding: binding,
            macSigningKey: signer.publicKey.rawRepresentation, agreementPrivateKey: recipient.rawRepresentation)
        XCTAssertThrowsError(try NotificationDecoder.decrypt(wire, context: anotherDevice))
        let anotherSigner = NotificationDecryptionContext(macID: "mac-1", deviceID: "client-1", binding: binding,
            macSigningKey: P256.Signing.PrivateKey().publicKey.rawRepresentation, agreementPrivateKey: recipient.rawRepresentation)
        XCTAssertThrowsError(try NotificationDecoder.decrypt(wire, context: anotherSigner))
        let plaintextOnly = try JSONSerialization.data(withJSONObject: ["alondra": ["kind": "question", "id": 42, "push_binding": binding]])
        XCTAssertThrowsError(try NotificationDecoder.decrypt(plaintextOnly, context: context))
    }
}
