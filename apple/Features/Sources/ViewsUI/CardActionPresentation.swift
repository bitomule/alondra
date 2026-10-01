import AlondraContract

public enum CardActionPresentation {
    public static func isRescueComments(card: Card, action: Action) -> Bool {
        card.icon == .delivery && action.id?.hasPrefix("changes:") == true && action.comments != nil
    }

    public static func isOpenOnGitHub(_ action: Action) -> Bool {
        guard action.id?.isEmpty ?? true, case .url = action.link else { return false }
        return true
    }
}
