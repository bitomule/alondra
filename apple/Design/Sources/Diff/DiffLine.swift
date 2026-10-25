public struct DiffLine: Identifiable, Hashable, Sendable {
    public enum Kind: Sendable { case hunk, context, added, removed, note }

    public let id: String
    public let kind: Kind
    public let text: String
    public let old: Int?
    public let new: Int?
    /// `path:line` that a comment on this line is anchored to, as tuicr writes it.
    public let anchor: String?
    /// Characters of `text` that differ from the line it replaces or is replaced by.
    public internal(set) var changed: Range<Int>?

    init(id: String, kind: Kind, text: String, old: Int?, new: Int?, anchor: String?, changed: Range<Int>? = nil) {
        self.id = id
        self.kind = kind
        self.text = text
        self.old = old
        self.new = new
        self.anchor = anchor
        self.changed = changed
    }
}
