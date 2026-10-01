import CryptoKit
import Foundation

/// This Mac's device identity on its own alondrad. Files, not the keychain: the app is ad-hoc
/// signed in development, where the data protection keychain refuses it and the login keychain
/// would ask for a password after every rebuild.
public actor LoopbackIdentityStore {
    private let directory: URL
    private let makeKey: @Sendable () -> LoopbackSigningKey
    private var cachedKey: LoopbackSigningKey?

    public init(directory: URL, makeKey: @escaping @Sendable () -> LoopbackSigningKey = { LoopbackSigningKey.generate() }) {
        self.directory = directory
        self.makeKey = makeKey
    }

    public func signingKey() throws -> LoopbackSigningKey {
        if let cachedKey { return cachedKey }
        let key: LoopbackSigningKey
        if let archived = read("signing-key") {
            key = try LoopbackSigningKey(archived: archived)
        } else {
            key = makeKey()
            try write(key.archived, to: "signing-key")
            remove("device-id")
        }
        cachedKey = key
        return key
    }

    /// Changes whenever the key changes, so journals written under an older key stay apart.
    public func credentialRevision() throws -> String {
        SHA256.hash(data: try signingKey().publicKey.x963Representation).map { String(format: "%02x", $0) }.joined()
    }

    public func bindingSecret() throws -> Data {
        if let secret = read("binding-secret"), secret.count == 32 { return secret }
        let secret = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
        try write(secret, to: "binding-secret")
        return secret
    }

    public func deviceID() -> String? {
        read("device-id").map { String(decoding: $0, as: UTF8.self) }.flatMap { $0.isEmpty ? nil : $0 }
    }

    public func save(deviceID: String) throws {
        try write(Data(deviceID.utf8), to: "device-id")
    }

    public func forgetDeviceID() {
        remove("device-id")
    }

    private func url(_ name: String) -> URL { directory.appending(path: name) }

    private func read(_ name: String) -> Data? { try? Data(contentsOf: url(name)) }

    private func write(_ data: Data, to name: String) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        try data.write(to: url(name), options: .atomic)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url(name).path(percentEncoded: false))
    }

    private func remove(_ name: String) { try? FileManager.default.removeItem(at: url(name)) }
}
