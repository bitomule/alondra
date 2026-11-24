import AlondraContract
import Foundation

/// What the iPhone says when the Mac has not confirmed something sent from it within
/// `Optimistic.confirmationTimeout`: what is late and why, as far as the connection knows. The
/// delivery goes on in the background either way.
public enum LateNotice: Equatable, Sendable {
    /// An action named by its button, such as «Cancel Task».
    case action(String?)
    /// The answer to a card, named by what the card asks.
    case answer(String?)

    public func message(_ connection: ConnectionState?) -> String {
        switch (self, connection) {
        case (.action(let label?), .unreachable(let problem)?) where !label.isEmpty:
            let cause = Self.sentenceStart(String(localized: problem.summary))
            return String(localized: "\(cause): still sending “\(label)”.")
        case (.action(let label?), .connecting?) where !label.isEmpty:
            return String(localized: "Connecting to your Mac: still sending “\(label)”.")
        case (.action(let label?), _) where !label.isEmpty:
            return String(localized: "Your Mac hasn’t confirmed it yet: still sending “\(label)”.")
        case (.answer(let card?), .unreachable(let problem)?) where !card.isEmpty:
            let cause = Self.sentenceStart(String(localized: problem.summary))
            return String(localized: "\(cause): still sending your answer to “\(card)”.")
        case (.answer(let card?), .connecting?) where !card.isEmpty:
            return String(localized: "Connecting to your Mac: still sending your answer to “\(card)”.")
        case (.answer(let card?), _) where !card.isEmpty:
            return String(localized: "Your Mac hasn’t confirmed it yet: still sending your answer to “\(card)”.")
        case (.action, _):
            return String(localized: "Your Mac hasn’t confirmed an action.")
        case (.answer, _):
            return String(localized: "Your Mac hasn’t confirmed an answer.")
        }
    }

    /// The cause leads the notice, so it is still read when a long name is cut.
    private static func sentenceStart(_ text: String) -> String {
        text.prefix(1).uppercased() + text.dropFirst()
    }
}
