import AlondraLiveActivity
import AlondraNotifications
import CryptoKit
import Foundation
import Testing

@Suite struct ActivityKeyFileTests {
    private func withFile(_ body: (ActivityKeyFile, URL) throws -> Void) throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let url = directory.appendingPathComponent("keys.json")
        try body(ActivityKeyFile(url: url), url)
    }

    private func pairing(_ mac: String) -> NotificationDecryptionContext {
        NotificationDecryptionContext(macID: mac, deviceID: "phone", binding: "binding-\(mac)", macSigningKey: Data([1]),
                                      agreementPrivateKey: Data(repeating: 9, count: 32))
    }

    @Test func eachPairingGetsAKeyOfItsOwnNeverItsAgreementKey() throws {
        try withFile { file, _ in
            let publicKeys = try file.update([pairing("mac-a"), pairing("mac-b")])
            #expect(Set(publicKeys.keys) == ["binding-mac-a", "binding-mac-b"])
            #expect(Set(publicKeys.values).count == 2)
            for key in try file.contexts() { #expect(key.agreementPrivateKey != Data(repeating: 9, count: 32)) }
        }
    }

    @Test func aKeyStaysAcrossUpdatesAndLeavesWithItsPairing() throws {
        try withFile { file, _ in
            let first = try file.update([pairing("mac-a"), pairing("mac-b")])
            let second = try file.update([pairing("mac-a")])
            #expect(second == ["binding-mac-a": first["binding-mac-a"]])
            #expect(try file.contexts().map(\.macID) == ["mac-a"])
        }
    }

    @Test func thePublicHalfIsAPointTheMacCanRead() throws {
        try withFile { file, _ in
            let encoded = try #require(try file.update([pairing("mac-a")])["binding-mac-a"])
            let base64 = encoded.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            let padded = base64.padding(toLength: (base64.count + 3) / 4 * 4, withPad: "=", startingAt: 0)
            let bytes = try #require(Data(base64Encoded: padded))
            #expect(try P256.KeyAgreement.PublicKey(x963Representation: bytes).x963Representation == bytes)
        }
    }

    @Test func theFileStaysOutOfBackups() throws {
        try withFile { file, url in
            _ = try file.update([pairing("mac-a")])
            #expect(try url.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup == true)
        }
    }

    @Test func anAccountThatEndsLeavesNoKeys() throws {
        try withFile { file, _ in
            _ = try file.update([pairing("mac-a")])
            #expect(try file.update([]).isEmpty)
            #expect(try file.contexts().isEmpty)
        }
    }
}
