import Foundation

/// The daemon's handoff journal. This identity is unrelated to an encrypted request UUID.
public struct DeliveryAttempt: Codable, Equatable, Sendable, Identifiable {
    public let id: UInt64
    public let sessionID: String
    public let channel: String
    public let state: String
    public let version: UInt64
    public let questionID: UInt64?
    public let reason: String?
    public var needsAttention: Bool { state == "dispatching" || state == "uncertain" }
    public var canResolve: Bool { state == "uncertain" }
    enum CodingKeys: String, CodingKey {
        case id, channel, state, version, reason
        case sessionID = "session_id", questionID = "question_id"
    }
    public init(id: UInt64, sessionID: String, channel: String, state: String, version: UInt64,
                questionID: UInt64? = nil, reason: String? = nil) {
        self.id = id; self.sessionID = sessionID; self.channel = channel; self.state = state
        self.version = version; self.questionID = questionID; self.reason = reason
    }
}

/// A confirmed HTTP response can still report an unconfirmed external handoff.
/// Messages here are local text, never persisted arbitrary response bodies.
public struct DeliveryHandoffUnconfirmed: Error, LocalizedError, Equatable, Sendable {
    public static let message = "We couldn’t confirm the result."
    public var errorDescription: String? { Self.message }
    public let attemptID: UInt64?
    public init(attemptID: UInt64? = nil) { self.attemptID = attemptID }

    public static func inspect(_ body: Data, requiresJSONObject: Bool = false, status: Int = 200) throws -> Self? {
        if status == 204 && body.isEmpty { return nil }
        guard let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any] else {
            if requiresJSONObject { throw DeliveryResponseError.invalid }
            return nil
        }
        guard let value = object["delivery_state"] else { return nil }
        // Existing commands without a delivery marker retain their acceptance semantics.
        guard let state = value as? String, ["handed", "queued"].contains(state) else {
            struct Identity: Decodable {
                let attemptID: UInt64?
                enum CodingKeys: String, CodingKey { case attemptID = "delivery_attempt" }
            }
            return Self(attemptID: (try? JSONDecoder().decode(Identity.self, from: body))?.attemptID)
        }
        return nil
    }
}

public enum DeliveryResponseError: Error, LocalizedError, Sendable {
    case invalid
    public var errorDescription: String? { "The Mac response could not be verified. The command remains unconfirmed; it will not be sent again automatically." }
}

/// Captured before dispatch and persisted with the encrypted command's semantic identity.
public struct DeliveryDecision: Codable, Hashable, Sendable {
    public let id: UInt64
    public let version: UInt64
    public let sessionID: String
    public let channel: String
    public let retry: Bool
    public init(observed: DeliveryAttempt, retry: Bool) {
        self.id = observed.id; self.version = observed.version; self.sessionID = observed.sessionID
        self.channel = observed.channel; self.retry = retry
    }
    public func validate(_ body: Data) throws {
        struct Result: Decodable { let attempt: DeliveryAttempt }
        guard let result = try? JSONDecoder().decode(Result.self, from: body).attempt,
              version < UInt64.max, result.id == id, result.sessionID == sessionID,
              result.channel == channel, result.version == version + 1,
              result.state == (retry ? "retryable" : "discarded") else { throw DeliveryResponseError.invalid }
    }
}
