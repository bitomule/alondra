import AlondraContracts
import Foundation

public struct TriageIdentity: Equatable, Sendable {
    public let server: URL
    public let deviceID: String
    public let credentialRevision: String
    public let generation: UInt64

    public init(server: URL, deviceID: String, credentialRevision: String, generation: UInt64) {
        self.server = server
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
        self.generation = generation
    }
}

public enum TriageConnection: Equatable, Sendable {
    case connecting, online
    case offline(ConnectionProblem)

    public var problem: ConnectionProblem? {
        if case .offline(let problem) = self { return problem }
        return nil
    }
}
