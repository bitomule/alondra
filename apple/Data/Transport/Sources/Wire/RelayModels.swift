import Foundation

public struct RelayBootstrap: Decodable, Sendable {
    public let protocolVersion: Int
    public let epoch: String
    enum CodingKeys: String, CodingKey { case protocolVersion = "protocol", epoch }
}

public struct RelayJob: Decodable, Sendable {
    public let requestID: String
    public let status: String
    public let revision: UInt64
    public let response: Envelope?
    enum CodingKeys: String, CodingKey { case requestID = "request_id", status, revision, response }
}

public struct RelayChanges: Decodable, Sendable {
    public let epoch: String
    public let revision: UInt64
    public let changes: [RelayJob]
}

public struct RelayPushRegistration: Encodable, Sendable {
    public let token: String
    public let environment: String
    public let pushBinding: String
    enum CodingKeys: String, CodingKey { case token, environment; case pushBinding = "push_binding" }

    public init(token: String, environment: String, pushBinding: String) throws {
        guard token.utf8.count == 64, token.utf8.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }),
              ["sandbox", "production"].contains(environment),
              try Base64URL.decode(pushBinding).count == 32 else { throw RelayWireError.invalidField }
        self.token = token; self.environment = environment; self.pushBinding = pushBinding
    }
}
