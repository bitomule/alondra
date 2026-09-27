import Foundation
import AlondraContracts

public struct CommandResponse: Sendable {
    public let status: Int
    public let body: Data
    public init(status: Int, body: Data = Data()) { self.status = status; self.body = body }
}

/// A terminal response verified against the original encrypted operation identity.
/// Body text is not persisted; only the status needed to reconcile presentation is retained.
public struct CommandResolution: Codable, Equatable, Sendable, Identifiable {
    public var id: UUID { command.id }
    public let command: UncertainCommand
    public let status: Int
    public let deliveryUnconfirmed: Bool?
    public let deliveryAttemptID: UInt64?
    public var succeeded: Bool { (200..<300).contains(status) && deliveryUnconfirmed != true }
    public var message: String { deliveryUnconfirmed == true ? DeliveryHandoffUnconfirmed.message : "The service rejected this operation (HTTP \(status)). Your draft has been kept." }
    public init(command: UncertainCommand, status: Int, body: Data = Data()) {
        self.command = command; self.status = status
        let issue = try? DeliveryHandoffUnconfirmed.inspect(body, status: status)
        self.deliveryUnconfirmed = (200..<300).contains(status) && issue != nil
        self.deliveryAttemptID = issue?.attemptID
    }
    public static func checked(command: UncertainCommand, response: CommandResponse) throws -> Self {
        if (200..<300).contains(response.status) {
            switch command.kind {
            case .answer:
                _ = try DeliveryHandoffUnconfirmed.inspect(response.body, requiresJSONObject: true, status: response.status)
            case .resolveDelivery(let decision): try decision.validate(response.body)
            default: break
            }
        }
        return Self(command: command, status: response.status, body: response.body)
    }
}
