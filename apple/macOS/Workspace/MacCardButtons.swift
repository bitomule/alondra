import AlondraContract
import AlondraFeatures

/// The buttons a card or action bar shows, and what goes behind "More…". A card's come from the
/// server already split; an action bar shows its first three and keeps the rest for "More…".
public struct MacCardButtons: Equatable {
    static let barLimit = 3

    public let visible: [AlondraContract.Action]
    public let more: [AlondraContract.Action]

    public init(bar actions: [AlondraContract.Action]) {
        visible = Array(actions.prefix(Self.barLimit))
        more = Array(actions.dropFirst(Self.barLimit))
    }

    /// A button an optimistic send already hid stays out until the card comes back.
    public init(_ card: Card, excluding hidden: Set<String> = []) {
        visible = card.actions.filter { $0.id.map(hidden.contains) != true }
        more = card.more
    }
}
