import AlondraContract
import AlondraDesign

public enum CardActionPresentation {
    public static func isDocumentReview(_ card: Card) -> Bool {
        (card.icon == .document || card.icon == .delivery) && card.actions.contains(where: carriesDocumentComments)
    }

    public static func isRescueComments(card: Card, action: Action) -> Bool {
        card.icon == .delivery && carriesDocumentComments(action) && carriesArtifact(card)
    }

    /// The buttons under the reply field. The field already sends `reply`; on a document that
    /// action stays as a button to open its comments, but on a pull request, as in 8ccd131, the
    /// field is Request changes and its line comments open from the card.
    public static func showsAsControl(_ action: Action, in card: Card, reply: Action?) -> Bool {
        guard action.presentation != .options else { return false }
        guard action.id == reply?.id else { return true }
        return card.icon != .pullRequest && action.comments != nil && action.id?.hasPrefix("reply:") != true
    }

    public static func isOpenOnGitHub(_ action: Action) -> Bool {
        guard action.id?.isEmpty ?? true, case .url = action.link else { return false }
        return true
    }

    private static func carriesDocumentComments(_ action: Action) -> Bool {
        action.id?.hasPrefix("changes:") == true && action.comments != nil
    }

    private static func carriesArtifact(_ card: Card) -> Bool {
        card.body.map { !Artifacts.urls(in: $0.text).isEmpty } ?? false
    }
}
