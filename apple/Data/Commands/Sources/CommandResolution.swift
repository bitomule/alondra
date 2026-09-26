import Foundation

/// A terminal response verified against the original encrypted operation identity.
/// Body text is not persisted; only the status needed to reconcile presentation is retained.
public struct CommandResolution: Codable, Equatable, Sendable, Identifiable {
    public var id: UUID { command.id }
    public let command: UncertainCommand
    public let status: Int
    public var succeeded: Bool { (200..<300).contains(status) }
    public var message: String { "The service rejected this operation (HTTP \(status)). Your draft has been kept." }
    public init(command: UncertainCommand, status: Int) { self.command = command; self.status = status }
}
