import Foundation

/// A device asking to use this Mac, with the code its own screen shows.
public struct PairingRequest: Identifiable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let code: String

    public init(id: String, name: String, code: String) {
        self.id = id
        self.name = name
        self.code = code
    }
}

/// A device this Mac accepts, and how it reaches the Mac.
public struct TrustedDevice: Identifiable, Equatable, Sendable {
    public enum Origin: String, Sendable, Equatable { case mac, direct, relay }

    public let id: String
    public let name: String
    /// Nil from a daemon that predates the field.
    public let origin: Origin?

    public init(id: String, name: String, origin: Origin?) {
        self.id = id
        self.name = name
        self.origin = origin
    }
}
