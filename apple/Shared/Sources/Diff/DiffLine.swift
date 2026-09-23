public struct DiffLine: Identifiable, Hashable, Sendable {
    public enum Kind: Sendable { case hunk, context, added, removed, note }

    public let id: String
    public let kind: Kind
    public let text: String
    public let old: Int?
    public let new: Int?
    /// `path:line` that a comment on this line is anchored to, as tuicr writes it.
    public let anchor: String?
}
