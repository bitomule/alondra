@testable import AlondraPairing
import AlondraTransport
import CryptoKit
import Foundation
import Testing

/// The same fixture as `fixed_vector_for_swift` in `crates/e2e/src/pairing.rs`.
private func key(_ seed: UInt8) throws -> String {
    PairingWire.encode(try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: seed, count: 32)).publicKey.x963Representation)
}

private func nonce(_ seed: UInt8) -> String { PairingWire.encode(Data(repeating: seed, count: 32)) }

private func fixedTranscript() throws -> DirectPairingProtocol.Transcript {
    DirectPairingProtocol.Transcript(pairingID: "pairing-1", macID: "mac-1", macSigningKey: try key(1),
        macAgreementKey: try key(2), deviceID: "client-1", clientSigningKey: try key(3),
        clientAgreementKey: try key(4), clientNonce: nonce(5), macNonce: nonce(6))
}

private func approval(for transcript: DirectPairingProtocol.Transcript, expiresAtMs: UInt64 = 2_000) throws -> DirectPairingProtocol.Approval {
    DirectPairingProtocol.Approval(pairingID: transcript.pairingID, macID: transcript.macID,
        macSigningKey: transcript.macSigningKey, macAgreementKey: transcript.macAgreementKey,
        deviceID: transcript.deviceID, clientSigningKey: transcript.clientSigningKey,
        clientAgreementKey: transcript.clientAgreementKey, transcript: try transcript.digest(),
        expiresAtMs: expiresAtMs)
}

private func sign(_ approval: DirectPairingProtocol.Approval, with key: P256.Signing.PrivateKey) throws -> String {
    PairingWire.encode(try key.signature(for: approval.signedBytes()).rawRepresentation)
}

@Suite struct DirectPairingTests {
    @Test func matchesTheRustVector() throws {
        let transcript = try fixedTranscript()
        #expect(try DirectPairingProtocol.commitment(clientNonce: nonce(5), deviceID: "client-1",
            signingKey: try key(3), agreementKey: try key(4)) == "tGD__kdXmgEOwFVNao_SKTOW91CptgNOe-SWHLOzP1w")
        #expect(try transcript.code() == "36A0E0")
        #expect(try transcript.digest() == "oLgRJFfS1Fdz6LXD2bn1nVz1sOq9NHBkf-WevmCEiBM")
    }

    @Test func anotherMacNonceGivesAnotherCode() throws {
        let honest = try fixedTranscript()
        let attacked = DirectPairingProtocol.Transcript(pairingID: honest.pairingID, macID: honest.macID,
            macSigningKey: honest.macSigningKey, macAgreementKey: honest.macAgreementKey, deviceID: honest.deviceID,
            clientSigningKey: honest.clientSigningKey, clientAgreementKey: honest.clientAgreementKey,
            clientNonce: honest.clientNonce, macNonce: nonce(7))
        #expect(try honest.code() != attacked.code())
    }

    @Test func acceptsTheMacsApprovalOfThisTranscript() throws {
        let mac = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let transcript = try fixedTranscript()
        let signed = try approval(for: transcript)
        try signed.verify(signature: try sign(signed, with: mac), macKey: mac.publicKey, expected: transcript, nowMs: 1_000)
    }

    @Test func refusesAnApprovalSignedByAnotherKey() throws {
        let mac = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let transcript = try fixedTranscript()
        let signed = try approval(for: transcript)
        #expect(throws: PairingError.invalidApproval) {
            try signed.verify(signature: try sign(signed, with: P256.Signing.PrivateKey()), macKey: mac.publicKey,
                              expected: transcript, nowMs: 1_000)
        }
    }

    @Test func refusesAnApprovalOfAnotherTranscript() throws {
        let mac = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let transcript = try fixedTranscript()
        let other = DirectPairingProtocol.Transcript(pairingID: transcript.pairingID, macID: transcript.macID,
            macSigningKey: transcript.macSigningKey, macAgreementKey: transcript.macAgreementKey,
            deviceID: transcript.deviceID, clientSigningKey: transcript.clientSigningKey,
            clientAgreementKey: transcript.clientAgreementKey, clientNonce: transcript.clientNonce, macNonce: nonce(7))
        let signed = try approval(for: other)
        #expect(throws: PairingError.invalidApproval) {
            try signed.verify(signature: try sign(signed, with: mac), macKey: mac.publicKey, expected: transcript, nowMs: 1_000)
        }
    }

    @Test func refusesAnExpiredApproval() throws {
        let mac = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let transcript = try fixedTranscript()
        let signed = try approval(for: transcript)
        #expect(throws: PairingError.requestExpired) {
            try signed.verify(signature: try sign(signed, with: mac), macKey: mac.publicKey, expected: transcript, nowMs: 2_000)
        }
    }

    @Test func aPairingSavedBeforeDirectPairingStillReads() throws {
        let legacy = Data("""
        {"version":1,"pairings":[{"relayURL":"https://alondra-api.davidcollado.dev","macID":"mac-1",
        "deviceID":"phone-1","ownerSub":"owner","macSigningKey":"\(try key(1))","macAgreementKey":"\(try key(2))",
        "signingPrivateKey":"\(Data(repeating: 3, count: 32).base64EncodedString())",
        "agreementPrivateKey":"\(Data(repeating: 4, count: 32).base64EncodedString())","approvedAtMs":1}],
        "selectedID":{"relayURL":"https://alondra-api.davidcollado.dev","ownerSub":"owner","macID":"mac-1","deviceID":"phone-1"}}
        """.utf8)
        let collection = try JSONDecoder().decode(ApprovedPairingCollection.self, from: legacy)
        #expect(collection.selected?.hasRelay == true)
        #expect(collection.selected?.routes == nil)
    }

    @Test func aPairingSavedOnThePreviousRelayHostMovesToTheCurrentOne() throws {
        let saved = Data("""
        {"version":1,"pairings":[{"relayURL":"https://alondra-api.davidcollado.dev","macID":"mac-1",
        "deviceID":"phone-1","ownerSub":"owner","macSigningKey":"\(try key(1))","macAgreementKey":"\(try key(2))",
        "signingPrivateKey":"\(Data(repeating: 3, count: 32).base64EncodedString())",
        "agreementPrivateKey":"\(Data(repeating: 4, count: 32).base64EncodedString())","approvedAtMs":1}],
        "selectedID":{"relayURL":"https://alondra-api.davidcollado.dev","ownerSub":"owner","macID":"mac-1","deviceID":"phone-1"}}
        """.utf8)
        let collection = try JSONDecoder().decode(ApprovedPairingCollection.self, from: saved)
        let current = URL(string: "https://api.getalondra.com")
        #expect(collection.selected?.relayURL == current)
        #expect(collection.selectedID?.relayURL == current)
        #expect(collection.pairings(ownerSub: "owner", relayURL: current!).count == 1)
    }

    @Test func aRelayThatIsNotThePreviousHostIsLeftAlone() {
        let other = URL(string: "https://relay.example.test")
        #expect(ServiceHosts.current(other) == other)
        #expect(ServiceHosts.current(nil) == nil)
    }

    @Test func aDirectPairingRoundTripsWithoutRelayOrOwner() throws {
        var direct = ApprovedServicePairing(relayURL: nil, macID: "mac-1", deviceID: "phone-1", ownerSub: nil,
            macSigningKey: try key(1), macAgreementKey: try key(2), signingPrivateKey: Data(repeating: 3, count: 32),
            agreementPrivateKey: Data(repeating: 4, count: 32), approvedAtMs: 1)
        direct.name = "Studio"
        direct.routes = [DirectEndpoint(host: "192.168.1.5", port: 47810, kind: .localNetwork)]
        var collection = ApprovedPairingCollection()
        try collection.upsert(direct)
        let decoded = try JSONDecoder().decode(ApprovedPairingCollection.self, from: JSONEncoder().encode(collection))
        #expect(decoded.selected?.hasRelay == false)
        #expect(decoded.selected?.name == "Studio")
        #expect(decoded.selected?.routes == direct.routes)
    }

    @Test func aDirectPairingIsPinnedAgainstARelayPairingOfTheSameMac() throws {
        let relay = ApprovedServicePairing(relayURL: URL(string: "https://api.getalondra.com")!, macID: "mac-1",
            deviceID: "phone-1", ownerSub: "owner", macSigningKey: try key(1), macAgreementKey: try key(2),
            signingPrivateKey: Data(repeating: 3, count: 32), agreementPrivateKey: Data(repeating: 4, count: 32), approvedAtMs: 1)
        let impostor = ApprovedServicePairing(relayURL: nil, macID: "mac-1", deviceID: "phone-2", ownerSub: nil,
            macSigningKey: try key(9), macAgreementKey: try key(2), signingPrivateKey: Data(repeating: 5, count: 32),
            agreementPrivateKey: Data(repeating: 6, count: 32), approvedAtMs: 2)
        var collection = ApprovedPairingCollection()
        try collection.upsert(relay)
        #expect(throws: PairingStoreError.identityConflict) { try collection.upsert(impostor) }
    }

    @Test func signingOutKeepsDirectPairings() throws {
        let relay = ApprovedServicePairing(relayURL: URL(string: "https://api.getalondra.com")!, macID: "mac-1",
            deviceID: "phone-1", ownerSub: "owner", macSigningKey: try key(1), macAgreementKey: try key(2),
            signingPrivateKey: Data(repeating: 3, count: 32), agreementPrivateKey: Data(repeating: 4, count: 32), approvedAtMs: 1)
        let direct = ApprovedServicePairing(relayURL: nil, macID: "mac-2", deviceID: "phone-2", ownerSub: nil,
            macSigningKey: try key(7), macAgreementKey: try key(8), signingPrivateKey: Data(repeating: 5, count: 32),
            agreementPrivateKey: Data(repeating: 6, count: 32), approvedAtMs: 2)
        var collection = ApprovedPairingCollection()
        try collection.upsert(direct)
        try collection.upsert(relay)
        collection.removeRelayPairings()
        #expect(collection.pairings.map(\.macID) == ["mac-2"])
        #expect(collection.selectedID == ApprovedPairingID(direct))
    }

    @Test func pairingAMacAgainReplacesItsSavedKeysOnlyInTheSameWrite() throws {
        let old = ApprovedServicePairing(relayURL: nil, macID: "mac-1", deviceID: "phone-1", ownerSub: nil,
            macSigningKey: try key(1), macAgreementKey: try key(2), signingPrivateKey: Data(repeating: 3, count: 32),
            agreementPrivateKey: Data(repeating: 4, count: 32), approvedAtMs: 1)
        let other = ApprovedServicePairing(relayURL: nil, macID: "mac-2", deviceID: "phone-2", ownerSub: nil,
            macSigningKey: try key(7), macAgreementKey: try key(8), signingPrivateKey: Data(repeating: 5, count: 32),
            agreementPrivateKey: Data(repeating: 6, count: 32), approvedAtMs: 2)
        let reinstalled = ApprovedServicePairing(relayURL: nil, macID: "mac-1", deviceID: "phone-3", ownerSub: nil,
            macSigningKey: try key(9), macAgreementKey: try key(10), signingPrivateKey: Data(repeating: 11, count: 32),
            agreementPrivateKey: Data(repeating: 12, count: 32), approvedAtMs: 3)
        var collection = ApprovedPairingCollection()
        try collection.upsert(old)
        try collection.upsert(other)
        #expect(throws: PairingStoreError.identityConflict) { try collection.upsert(reinstalled) }
        #expect(collection.pairings.map(\.deviceID) == ["phone-1", "phone-2"], "a plain save never changes pinned keys")

        try collection.replaceLocalPairing(of: "mac-1", with: reinstalled)
        #expect(collection.pairings.map(\.deviceID) == ["phone-2", "phone-3"])
        #expect(collection.selected?.macSigningKey == reinstalled.macSigningKey)
    }

    @Test func pairingAgainNeverTouchesTheRelayPairingOfThatMac() throws {
        let relay = ApprovedServicePairing(relayURL: URL(string: "https://api.getalondra.com")!, macID: "mac-1",
            deviceID: "phone-1", ownerSub: "owner", macSigningKey: try key(1), macAgreementKey: try key(2),
            signingPrivateKey: Data(repeating: 3, count: 32), agreementPrivateKey: Data(repeating: 4, count: 32), approvedAtMs: 1)
        let impostor = ApprovedServicePairing(relayURL: nil, macID: "mac-1", deviceID: "phone-2", ownerSub: nil,
            macSigningKey: try key(9), macAgreementKey: try key(2), signingPrivateKey: Data(repeating: 5, count: 32),
            agreementPrivateKey: Data(repeating: 6, count: 32), approvedAtMs: 2)
        var collection = ApprovedPairingCollection()
        try collection.upsert(relay)
        #expect(throws: PairingStoreError.identityConflict) { try collection.replaceLocalPairing(of: "mac-1", with: impostor) }
    }
}
