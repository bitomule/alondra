import AlondraNotifications
import AlondraTransport
import CryptoKit
import Foundation

/// Opens the reports the relay hands the Live Activity, sealed by each Mac for this phone.
public enum ActivityEnvelopes {
    private struct Sealed: Decodable {
        let pushBinding: String
        let payload: Payload
        enum CodingKeys: String, CodingKey { case pushBinding = "push_binding", payload }
    }

    private struct Payload: Decodable {
        let inbox: Int
        let waiting: Int
        let working: Int
        let macName: String?
        let measuredAtMs: Int64
        let seq: UInt64
        enum CodingKeys: String, CodingKey {
            case inbox, waiting, working, seq, macName = "mac_name", measuredAtMs = "measured_at_ms"
        }
    }

    /// The newest report of each Mac. An envelope no pairing opens is left out, and one past its
    /// expiry still counts: the relay resends a Mac's envelope when another Mac changes, and
    /// `measuredAtMs` is what says it is old.
    public static func open(_ envelopes: [Envelope], with contexts: [NotificationDecryptionContext]) -> [ActivityReport] {
        var newest: [String: (seq: UInt64, report: ActivityReport)] = [:]
        for envelope in envelopes where envelope.kind == "activity" {
            guard let payload = contexts.lazy.compactMap({ open(envelope, with: $0) }).first else { continue }
            if let known = newest[envelope.sender], known.seq >= payload.seq { continue }
            let counts = ActivityCounts(inbox: payload.inbox, waiting: payload.waiting, working: payload.working)
            newest[envelope.sender] = (payload.seq, ActivityReport(macID: envelope.sender, macName: payload.macName, counts: counts,
                measuredAt: Date(timeIntervalSince1970: Double(payload.measuredAtMs) / 1000)))
        }
        return newest.keys.sorted().compactMap { newest[$0]?.report }
    }

    private static func open(_ envelope: Envelope, with context: NotificationDecryptionContext) -> Payload? {
        guard envelope.sender == context.macID, envelope.recipient == context.deviceID,
              let senderKey = try? P256.Signing.PublicKey(rawRepresentation: context.macSigningKey),
              let recipientKey = try? P256.KeyAgreement.PrivateKey(rawRepresentation: context.agreementPrivateKey),
              let plaintext = try? envelope.open(senderKey: senderKey, recipientKey: recipientKey, sender: context.macID,
                                                 recipient: context.deviceID, route: context.macID, nowMilliseconds: nil),
              let sealed = try? JSONDecoder().decode(Sealed.self, from: plaintext),
              sealed.pushBinding == context.binding else { return nil }
        return sealed.payload
    }
}
