import AlondraContract
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

extension Store where State == TriageCardFeature.State, Action == TriageCardFeature.Action {
    /// A button that leads to a web page opens it; any other is the card feature's to handle.
    func perform(_ action: AlondraContract.Action, openURL: OpenURLAction, mcpLinks: McpLinks) {
        if mcpLinks(action.link) { return }
        if case .url(let url) = action.link, let destination = URL(string: url) {
            openURL(destination)
            return
        }
        send(.actionTapped(action))
    }

    func open(_ link: AlondraContract.Link, openURL: OpenURLAction) {
        switch link {
        case let .view(key): send(.opened(ViewKey(rawValue: key)))
        case let .url(url): if let destination = URL(string: url) { openURL(destination) }
        case .local: break
        }
    }

    func openDetail(_ card: Card) { send(.opened(.card(card.id))) }
}
