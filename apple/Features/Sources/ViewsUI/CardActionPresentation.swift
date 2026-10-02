import AlondraContract
import AlondraDesign

public enum CardActionPresentation {
    public static func isDocumentReview(_ card: Card) -> Bool {
        (card.icon == .document || card.icon == .delivery) && card.actions.contains(where: carriesDocumentComments)
    }

    public static func isRescueComments(card: Card, action: Action) -> Bool {
        card.icon == .delivery && carriesDocumentComments(action) && carriesArtifact(card)
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
