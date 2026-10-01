import AlondraDesign
import Foundation

enum ReviewFeedback {
    static func quote(_ source: String) -> String {
        let collapsed = source.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        guard collapsed.count > 200 else { return collapsed }
        return collapsed.prefix(199).trimmingCharacters(in: .whitespaces) + "…"
    }
}
