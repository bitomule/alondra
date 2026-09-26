import Foundation

/// Local provenance of an approved notification binding. The current pairing model
/// has no issuer field; this is not a claim of multi-issuer account isolation.
public struct NotificationBindingScope: Codable, Hashable, Sendable {
    public let relayURL: URL
    public let ownerSub: String
    public let macID: String
    public let deviceID: String
    public let credentialRevision: String

    public init(relayURL: URL, ownerSub: String, macID: String, deviceID: String, credentialRevision: String) {
        self.relayURL = relayURL
        self.ownerSub = ownerSub
        self.macID = macID
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
    }

    public var isValid: Bool {
        relayURL.scheme == "https" && relayURL.host != nil && relayURL.user == nil &&
        relayURL.password == nil && relayURL.query == nil && relayURL.fragment == nil &&
        !ownerSub.isEmpty && !macID.isEmpty && !deviceID.isEmpty && !credentialRevision.isEmpty
    }
}
