import Foundation

public struct RelayBootstrap: Decodable, Sendable {
    public let protocolVersion: Int
    public let epoch: String
    /// The iroh relay this relay admits endpoints to; nil while it has none.
    public let irohRelay: URL?
    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol", epoch, irohRelay = "iroh_relay"
    }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        protocolVersion = try values.decode(Int.self, forKey: .protocolVersion)
        epoch = try values.decode(String.self, forKey: .epoch)
        let announced: String? = try? values.decodeIfPresent(String.self, forKey: .irohRelay)
        let url = announced.flatMap { URL(string: $0) }
        irohRelay = url?.scheme == "https" && url?.host != nil ? url : nil
    }
}

/// One of the owner's Macs as the relay knows it, with the iroh endpoint it bound, if any.
public struct RelayMac: Decodable, Sendable, Equatable {
    public let macID: String
    public let irohEndpoint: String?
    enum CodingKeys: String, CodingKey { case macID = "mac_id", irohEndpoint = "iroh_endpoint" }

    public init(macID: String, irohEndpoint: String?) {
        self.macID = macID
        self.irohEndpoint = irohEndpoint
    }
}

/// What a phone tells the relay about its Live Activity, through each of its pairings.
public struct RelayActivityRegistration: Encodable, Equatable, Sendable {
    public let phoneID: String
    public let startToken: String?
    public let updateToken: String?
    public let activityID: String?
    public let startedAtMs: UInt64?
    public let environment: String
    public let frequentUpdates: Bool

    enum CodingKeys: String, CodingKey {
        case environment
        case phoneID = "phone_id", startToken = "start_token", updateToken = "update_token"
        case activityID = "activity_id", startedAtMs = "started_at_ms", frequentUpdates = "frequent_updates"
    }

    public init(phoneID: String, startToken: String?, updateToken: String?, activityID: String?, startedAtMs: UInt64?,
                environment: String, frequentUpdates: Bool) throws {
        guard (16...64).contains(phoneID.utf8.count), ["sandbox", "production"].contains(environment),
              (updateToken != nil) || (activityID == nil && startedAtMs == nil) else { throw RelayWireError.invalidField }
        self.phoneID = phoneID; self.startToken = startToken; self.updateToken = updateToken; self.activityID = activityID
        self.startedAtMs = startedAtMs; self.environment = environment; self.frequentUpdates = frequentUpdates
    }
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
