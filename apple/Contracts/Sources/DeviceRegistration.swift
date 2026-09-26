import Foundation

public struct PairingInvite: Decodable, Sendable {
    public let code: String
    public let expiresIn: Int
    public let url: String?
}

public struct PairedDevice: Decodable, Identifiable, Hashable, Sendable {
    public let id: String
    public let name: String
    public let createdAt: Int64
    public let lastSeen: Int64?
}

public struct PushRegistration: Encodable, Hashable, Sendable {
    public enum Environment: String, Encodable, Sendable {
        case sandbox, production
    }

    public let token: String
    public let environment: Environment
    public let quietStart: String?
    public let quietEnd: String?
    public let pushBinding: String?

    public init(token: String, environment: Environment, quietStart: String?, quietEnd: String?, pushBinding: String? = nil) {
        self.token = token
        self.environment = environment
        self.quietStart = quietStart
        self.quietEnd = quietEnd
        self.pushBinding = pushBinding
    }

    private enum CodingKeys: String, CodingKey {
        case token, environment
        case quietStart = "quiet_start"
        case quietEnd = "quiet_end"
        case pushBinding = "push_binding"
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(token, forKey: .token)
        try container.encode(environment, forKey: .environment)
        try container.encode(quietStart, forKey: .quietStart)
        try container.encode(quietEnd, forKey: .quietEnd)
        try container.encodeIfPresent(pushBinding, forKey: .pushBinding)
    }
}
