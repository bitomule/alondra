import AlondraNotifications
import AlondraTransport
import CryptoKit
import Foundation
import os

/// The keys that open the Live Activity's reports, in a file of the App Group: the process that
/// draws a Live Activity has no Keychain, on a locked phone or an unlocked one. Each pairing has a
/// key of its own, never the pairing's agreement key, so this file reveals the counts and the
/// Mac's name at most. It stays readable after the first unlock and out of backups.
public struct ActivityKeyFile: Sendable {
    private static let log = Logger(subsystem: Bundle.main.bundleIdentifier ?? "AlondraLiveActivity", category: "keys")
    private let url: URL

    public init?(bundle: Bundle = .main) {
        guard let group = bundle.object(forInfoDictionaryKey: "AlondraActivityAppGroup") as? String, !group.contains("$("),
              let container = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group) else { return nil }
        self.init(url: container.appendingPathComponent("live-activity-keys.json"))
    }

    public init(url: URL) { self.url = url }

    public func contexts() throws -> [NotificationDecryptionContext] {
        guard FileManager.default.fileExists(atPath: url.path) else { return [] }
        return try JSONDecoder().decode([NotificationDecryptionContext].self, from: Data(contentsOf: url))
    }

    /// Leaves one key for each of these pairings, keeping the ones it had, and returns the public
    /// halves by binding for the Macs to seal with. With no pairings no key is left.
    public func update(_ pairings: [NotificationDecryptionContext]) throws -> [String: String] {
        let known = (try? contexts()) ?? []
        let keys = pairings.map { pairing in
            let key = known.first { $0.binding == pairing.binding && $0.macID == pairing.macID && $0.deviceID == pairing.deviceID }?
                .agreementPrivateKey ?? P256.KeyAgreement.PrivateKey().rawRepresentation
            return NotificationDecryptionContext(macID: pairing.macID, deviceID: pairing.deviceID, binding: pairing.binding,
                                                 macSigningKey: pairing.macSigningKey, agreementPrivateKey: key)
        }
        try save(keys)
        return try Dictionary(uniqueKeysWithValues: keys.map { key in
            let publicKey = try P256.KeyAgreement.PrivateKey(rawRepresentation: key.agreementPrivateKey).publicKey.x963Representation
            return (key.binding, Self.base64URL(publicKey))
        })
    }

    public func opened(_ state: ActivityContentState) -> [ActivityReport] {
        guard !state.macs.isEmpty else { return [] }
        do {
            let reports = ActivityEnvelopes.open(state.macs, with: try contexts())
            Self.log.notice("opened \(reports.count, privacy: .public) of \(state.macs.count, privacy: .public) reports")
            return reports
        } catch {
            Self.log.error("the key file cannot be read: \((error as NSError).code, privacy: .public)")
            return []
        }
    }

    private func save(_ keys: [NotificationDecryptionContext]) throws {
        try JSONEncoder().encode(keys).write(to: url, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        var file = url
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try file.setResourceValues(values)
    }

    private static func base64URL(_ data: Data) -> String {
        data.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
