import Foundation

/// A complete identity for one server. Keep the server URL with the signer so callers cannot
/// accidentally sign a request for a different server after switching connections.
public struct DeviceIdentity: Sendable {
    public let serverURL: URL
    public let signer: DeviceSigner

    public init(serverURL: URL, signer: DeviceSigner) {
        self.serverURL = serverURL
        self.signer = signer
    }

    /// URL spelling may differ between a QR invite and Settings. Compare the HTTP
    /// destination and base path rather than URL's raw string representation.
    public func matches(serverURL other: URL) -> Bool {
        guard let lhs = Self.serverComponents(serverURL),
              let rhs = Self.serverComponents(other) else { return false }
        return lhs == rhs
    }

    private struct ServerComponents: Equatable {
        let scheme: String
        let host: String
        let port: Int?
        let path: String
        let query: String?
        let user: String?
        let password: String?
        let fragment: String?
    }

    private static func serverComponents(_ url: URL) -> ServerComponents? {
        guard let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let scheme = parts.scheme?.lowercased(),
              let host = parts.host?.lowercased(),
              scheme == "http" || scheme == "https" else { return nil }
        var path = parts.percentEncodedPath.isEmpty ? "/" : parts.percentEncodedPath
        if path.count > 1 && path.hasSuffix("/") { path.removeLast() }
        let defaultPort = scheme == "https" ? 443 : 80
        return ServerComponents(
            scheme: scheme,
            host: host,
            port: parts.port ?? defaultPort,
            path: path,
            query: parts.percentEncodedQuery,
            user: parts.percentEncodedUser,
            password: parts.percentEncodedPassword,
            fragment: parts.percentEncodedFragment
        )
    }
}

/// Synchronous persistence for app, notification and future Intent entry points.
/// A caller owns the instance within its actor; this type does not retain UI state.
public struct DeviceIdentityStore {
    private let defaults: UserDefaults
    private let keys: any DeviceKeyStorage

    // Existing keys are intentionally unchanged. Users keep their current pairing after upgrade.
    private static let iosDeviceIDKey = "pairedDeviceID"
    private static let iosServerURLKey = "pairedURL"
    private static let serverSettingKey = "serverURL"
    private static let macDeviceIDsKey = "localDeviceIDs"

    public init(defaults: UserDefaults = .standard, keys: any DeviceKeyStorage = SystemDeviceKeyStorage()) {
        self.defaults = defaults
        self.keys = keys
    }

    public func pairedIOSIdentity() throws -> DeviceIdentity? {
        guard let id = defaults.string(forKey: Self.iosDeviceIDKey),
              let url = defaults.url(forKey: Self.iosServerURLKey) else { return nil }
        let key: DeviceSigningKey?
        do { key = try keys.load() }
        catch DeviceKeyStore.Failure.invalidStoredKey { return nil }
        guard let key else { return nil }
        return DeviceIdentity(serverURL: url, signer: DeviceSigner(deviceID: id, key: key))
    }

    /// Saves the key before the server registers it. If registration fails, restore the
    /// previous key with `restoreIOSKey(_:)` before surfacing the error.
    public func beginIOSPairing() throws -> DeviceSigningKey {
        let key = DeviceSigningKey.generate()
        try keys.save(key)
        return key
    }

    public func restoreIOSKey(_ previous: DeviceIdentity?) throws {
        if let previous {
            try keys.save(previous.signer.key)
        } else {
            keys.delete()
        }
    }

    public func completeIOSPairing(serverURL: URL, deviceID: String, key: DeviceSigningKey) -> DeviceIdentity {
        defaults.set(deviceID, forKey: Self.iosDeviceIDKey)
        defaults.set(serverURL, forKey: Self.iosServerURLKey)
        defaults.set(serverURL.absoluteString, forKey: Self.serverSettingKey)
        return DeviceIdentity(serverURL: serverURL, signer: DeviceSigner(deviceID: deviceID, key: key))
    }

    public func unpairIOS() {
        keys.delete()
        defaults.removeObject(forKey: Self.iosDeviceIDKey)
        defaults.removeObject(forKey: Self.iosServerURLKey)
    }

    public func pairedMacIdentity(at serverURL: URL) throws -> DeviceIdentity? {
        guard let id = macDeviceIDs()[serverURL.absoluteString] else { return nil }
        let key: DeviceSigningKey?
        do { key = try keys.load() }
        catch DeviceKeyStore.Failure.invalidStoredKey { return nil }
        guard let key else { return nil }
        return DeviceIdentity(serverURL: serverURL, signer: DeviceSigner(deviceID: id, key: key))
    }

    /// The Mac keeps one key across loopback servers, as before. Each server has its own ID.
    public func macSigningKey() throws -> DeviceSigningKey {
        do {
            if let key = try keys.load() { return key }
        } catch DeviceKeyStore.Failure.invalidStoredKey {
            // Old IDs cannot authenticate with a replacement key.
            defaults.removeObject(forKey: Self.macDeviceIDsKey)
        }
        let key = DeviceSigningKey.generate()
        try keys.save(key)
        return key
    }

    public func completeMacPairing(at serverURL: URL, deviceID: String, key: DeviceSigningKey) -> DeviceIdentity {
        var ids = macDeviceIDs()
        ids[serverURL.absoluteString] = deviceID
        defaults.set(ids, forKey: Self.macDeviceIDsKey)
        return DeviceIdentity(serverURL: serverURL, signer: DeviceSigner(deviceID: deviceID, key: key))
    }

    private func macDeviceIDs() -> [String: String] {
        defaults.dictionary(forKey: Self.macDeviceIDsKey) as? [String: String] ?? [:]
    }
}
