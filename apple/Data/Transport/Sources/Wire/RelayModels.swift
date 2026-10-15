import Foundation

public struct RelayBootstrap: Decodable, Sendable {
    public let protocolVersion: Int
    public let epoch: String
    public let requestWait: Bool
    public let resultAcknowledgment: Bool
    /// The iroh relay this relay admits endpoints to; nil while it has none.
    public let irohRelay: URL?
    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol", epoch, requestWait = "request_wait", resultAcknowledgment = "result_ack", irohRelay = "iroh_relay"
    }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        protocolVersion = try values.decode(Int.self, forKey: .protocolVersion)
        epoch = try values.decode(String.self, forKey: .epoch)
        requestWait = try values.decodeIfPresent(Bool.self, forKey: .requestWait) ?? false
        resultAcknowledgment = try values.decodeIfPresent(Bool.self, forKey: .resultAcknowledgment) ?? false
        let announced: String? = try? values.decodeIfPresent(String.self, forKey: .irohRelay)
        let url = announced.flatMap { URL(string: $0) }
        irohRelay = url?.scheme == "https" && url?.host != nil ? url : nil
    }
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
