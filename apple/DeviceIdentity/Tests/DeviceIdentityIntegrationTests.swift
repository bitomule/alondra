import AlondraAPIClient
import AlondraDeviceIdentity
import CryptoKit
import Foundation
import Security
import XCTest

private final class MemoryKeyStorage: DeviceKeyStorage {
    var key: DeviceSigningKey?
    var loadFailure: Error?

    func save(_ key: DeviceSigningKey) throws {
        self.key = key
        loadFailure = nil
    }
    func load() throws -> DeviceSigningKey? {
        if let loadFailure { throw loadFailure }
        return key
    }
    func delete() { key = nil }
}

final class DeviceIdentityIntegrationTests: XCTestCase {
    func testServerURLVariantsStayScopedToTheSameHTTPBase() throws {
        let key = DeviceSigningKey.software(P256.Signing.PrivateKey())
        let identity = DeviceIdentity(
            serverURL: try XCTUnwrap(URL(string: "https://EXAMPLE.test:443/")),
            signer: DeviceSigner(deviceID: "paired", key: key)
        )

        XCTAssertTrue(identity.matches(serverURL: try XCTUnwrap(URL(string: "https://example.test"))))
        XCTAssertFalse(identity.matches(serverURL: try XCTUnwrap(URL(string: "http://example.test"))))
        XCTAssertFalse(identity.matches(serverURL: try XCTUnwrap(URL(string: "https://example.test:444/"))))
        XCTAssertFalse(identity.matches(serverURL: try XCTUnwrap(URL(string: "https://example.test/other"))))
    }

    func testMacServerSwitchReusesKeyAndSignsWithMatchingDeviceID() throws {
        let suite = "DeviceIdentityIntegrationTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let keys = MemoryKeyStorage()
        let firstStore = DeviceIdentityStore(defaults: defaults, keys: keys)
        let firstURL = try XCTUnwrap(URL(string: "http://127.0.0.1:47800"))
        let secondURL = try XCTUnwrap(URL(string: "http://127.0.0.1:47801"))

        let key = try firstStore.macSigningKey()
        _ = firstStore.completeMacPairing(at: firstURL, deviceID: "first-device", key: key)
        XCTAssertNil(try firstStore.pairedMacIdentity(at: secondURL))

        let reusedKey = try firstStore.macSigningKey()
        XCTAssertEqual(reusedKey.publicKey.x963Representation, key.publicKey.x963Representation)
        _ = firstStore.completeMacPairing(at: secondURL, deviceID: "second-device", key: reusedKey)

        // A fresh headless entry point must recover the right ID for each server.
        let reopened = DeviceIdentityStore(defaults: defaults, keys: keys)
        let first = try XCTUnwrap(reopened.pairedMacIdentity(at: firstURL))
        let second = try XCTUnwrap(reopened.pairedMacIdentity(at: secondURL))
        XCTAssertEqual(first.signer.deviceID, "first-device")
        XCTAssertEqual(second.signer.deviceID, "second-device")
        XCTAssertEqual(first.signer.key.publicKey.x963Representation, second.signer.key.publicKey.x963Representation)

        var request = URLRequest(url: try XCTUnwrap(URL(string: "http://127.0.0.1:47801/api/v1/changes?after=1")))
        try RequestSignature.sign(&request, with: second.signer, now: Date(timeIntervalSince1970: 1_700_000_000))
        XCTAssertEqual(request.value(forHTTPHeaderField: RequestSignature.deviceHeader), "second-device")
        let signatureData = try XCTUnwrap(Data(base64Encoded: XCTUnwrap(request.value(forHTTPHeaderField: RequestSignature.signatureHeader))))
        let signature = try P256.Signing.ECDSASignature(derRepresentation: signatureData)
        let message = RequestSignature.message(method: "GET", target: "/api/v1/changes?after=1", timestamp: 1_700_000_000_000, body: Data())
        XCTAssertTrue(second.signer.key.publicKey.isValidSignature(signature, for: message))
    }

    func testFailedIOSPairingRestoresPreviousIdentityAndServer() throws {
        let suite = "DeviceIdentityIntegrationTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let keys = MemoryKeyStorage()
        let store = DeviceIdentityStore(defaults: defaults, keys: keys)
        let originalURL = try XCTUnwrap(URL(string: "https://example.test"))
        let previousKey = DeviceSigningKey.software(P256.Signing.PrivateKey())
        try keys.save(previousKey)
        _ = store.completeIOSPairing(serverURL: originalURL, deviceID: "original", key: previousKey)
        let previous = try XCTUnwrap(store.pairedIOSIdentity())

        let attemptedKey = try store.beginIOSPairing()
        XCTAssertNotEqual(attemptedKey.publicKey.x963Representation, previousKey.publicKey.x963Representation)
        try store.restoreIOSKey(previous)

        let reopened = try XCTUnwrap(DeviceIdentityStore(defaults: defaults, keys: keys).pairedIOSIdentity())
        XCTAssertEqual(reopened.serverURL, originalURL)
        XCTAssertEqual(reopened.signer.deviceID, "original")
        XCTAssertEqual(reopened.signer.key.publicKey.x963Representation, previousKey.publicKey.x963Representation)
    }

    func testCorruptStoredKeyCanBeRepairedButKeychainFailureCannotBeSuppressed() throws {
        let suite = "DeviceIdentityIntegrationTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let keys = MemoryKeyStorage()
        let store = DeviceIdentityStore(defaults: defaults, keys: keys)
        let server = try XCTUnwrap(URL(string: "https://example.test/"))
        let oldKey = DeviceSigningKey.software(P256.Signing.PrivateKey())
        try keys.save(oldKey)
        _ = store.completeIOSPairing(serverURL: server, deviceID: "old", key: oldKey)

        keys.loadFailure = DeviceKeyStore.Failure.invalidStoredKey
        XCTAssertNil(try store.pairedIOSIdentity())
        let replacement = try store.beginIOSPairing()
        _ = store.completeIOSPairing(serverURL: server, deviceID: "new", key: replacement)
        XCTAssertEqual(try store.pairedIOSIdentity()?.signer.deviceID, "new")

        keys.loadFailure = DeviceKeyStore.Failure.keychain(errSecInteractionNotAllowed)
        XCTAssertThrowsError(try store.pairedIOSIdentity()) { error in
            guard case DeviceKeyStore.Failure.keychain(errSecInteractionNotAllowed) = error else {
                return XCTFail("Unexpected error: \(error)")
            }
        }
    }
}
