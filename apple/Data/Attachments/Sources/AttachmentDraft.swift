import Foundation

public struct AttachmentDraft: Equatable, Sendable, Identifiable {
    public let id: UUID
    public let name: String
    public let data: Data
    public init(id: UUID = UUID(), name: String, data: Data) {
        self.id = id; self.name = name; self.data = data
    }
}
