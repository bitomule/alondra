import Foundation

public struct EnvelopeJournalScope: Codable, Sendable, Equatable {
    public let relay: URL
    public let macID: String
    public let deviceID: String
    public let credentialRevision: String

    public init(relay: URL, macID: String, deviceID: String, credentialRevision: String) {
        self.relay = relay
        self.macID = macID
        self.deviceID = deviceID
        self.credentialRevision = credentialRevision
    }
}

public struct EnvelopeJournalEntry: Codable, Sendable, Equatable {
    public enum Phase: String, Codable, Sendable { case reserved, uncertain, acknowledged, retired }

    public let requestID: String
    public let scope: EnvelopeJournalScope
    public var envelope: Envelope?
    public var phase: Phase
    public var lastStatus: String?
    public var lastRevision: UInt64?
    public var lastResponse: Envelope?
    /// Keyed request binding; contains no plaintext and survives retirement.
    public var semanticTag: Data? = nil
}
