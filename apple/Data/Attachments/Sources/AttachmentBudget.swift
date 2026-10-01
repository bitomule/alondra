import Foundation

/// Maximum retained attachment bytes and count for one composer draft.
public struct AttachmentBudget: Equatable, Sendable {
    public let maximumBytes: Int
    public let maximumCount: Int
    public init(maximumBytes: Int = 40 * 1024 * 1024, maximumCount: Int = 10) {
        self.maximumBytes = max(0, maximumBytes)
        self.maximumCount = max(0, maximumCount)
    }
    public func accepts(_ draft: AttachmentDraft, retained: [AttachmentDraft]) -> Bool {
        guard retained.count < maximumCount else { return false }
        var remaining = maximumBytes
        for entry in retained {
            guard entry.data.count <= remaining else { return false }
            remaining -= entry.data.count
        }
        return draft.data.count <= remaining
    }
}
