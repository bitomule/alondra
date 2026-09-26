import AlondraSettingsFeature
import AlondraShared
import Foundation

/// The only iOS composition boundary for the settings reducer. Transport creation stays outside View types.
@MainActor
enum SettingsLive {
    static func connection(pairing: DevicePairing) -> SettingsConnection {
        SettingsConnection(
            check: { url in
                let signer = await pairing.signer(for: url)
                try Task.checkCancellation()
                let client = AlondraClient(base: url, device: "iphone", signer: signer)
                do { try await client.health() }
                catch {
                    if error.isCancellation { throw CancellationError() }
                    throw SettingsCheckError(message: error.friendlyDescription)
                }
            },
            save: { url, expectedAddress in
                ServerSetting.compareAndSave(url, expectedAddress: expectedAddress)
            }
        )
    }
}

private struct SettingsCheckError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

enum ServerSetting {
    static let key = "serverURL"
    static let defaultURL = AlondraClient.defaultURL.absoluteString
    private static let saveLock = NSLock()

    /// Serializes Settings writes from multiple scenes; compare and write share one critical section.
    static func compareAndSave(_ url: URL, expectedAddress: String) -> SettingsSaveResult {
        saveLock.lock()
        defer { saveLock.unlock() }
        let current = UserDefaults.standard.string(forKey: key) ?? defaultURL
        guard current == expectedAddress else {
            if current == url.absoluteString { return .saved }
            return .conflict(currentAddress: current)
        }
        UserDefaults.standard.set(url.absoluteString, forKey: key)
        return .saved
    }
}
