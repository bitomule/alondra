import AlondraContracts
import Foundation

/// The caller must change `credentialRevision` when a signing key changes, including rotation
/// of a key whose device ID stays the same. It is local identity metadata, never sent on the wire.
public struct SyncIdentity: Hashable, Sendable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String

    public init(server: URL, deviceID: String, credentialRevision: String) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
    }
}

public enum SyncConnection: Equatable, Sendable {
    case connecting
    case online
    case offline(ConnectionProblem)
    case invalidConfiguration(String)
}
