import Foundation

/// Stored atomically in one Keychain item. A pending or uncertain credential must
/// never be used to restore application services after a process restart.
public struct RefreshCredential: Codable, Equatable, Sendable {
    public enum Status: String, Codable, Sendable { case active, pendingRevocation, rotationUncertain }
    public let token: String
    public var status: Status
    private let version: Int

    public init(token: String, status: Status = .active) {
        self.token = token
        self.status = status
        self.version = 1
    }

    /// The previous Keychain value was an unquoted UTF-8 refresh token.
    public static func decodePersisted(_ data: Data) throws -> Self {
        guard let value = String(data: data, encoding: .utf8), !value.isEmpty else {
            throw AuthError.invalidToken
        }
        if value.hasPrefix("{") {
            let record = try JSONDecoder().decode(Self.self, from: data)
            guard record.version == 1, !record.token.isEmpty else { throw AuthError.invalidToken }
            return record
        }
        return Self(token: value)
    }

    public func persistedData() throws -> Data {
        guard !token.isEmpty else { throw AuthError.invalidToken }
        return try JSONEncoder().encode(self)
    }
}
