import AlondraContract
import Foundation

public extension WireAction {
    /// What a reply field says while it is empty: the server's words, on every Apple client.
    var replyPlaceholder: String {
        if case .text(let placeholder, _, _) = input, let placeholder { return placeholder }
        return String(localized: "Reply to the node…")
    }
}
