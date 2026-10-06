import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import SwiftUI

/// Stores for the screens under test: each starts in the state a screen reaches after its view
/// arrived, and its feed stays quiet so nothing changes while the picture is taken.
private let referenceNow = Date(timeIntervalSince1970: 1_790_000_000)

@MainActor
enum ScreenStores {
    static func pinTime() {
        RelativeClock.now = { referenceNow }
    }

    private static func quiet(_ dependencies: inout DependencyValues) {
        dependencies.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
    }

    static func cardState(_ document: ViewDocument) -> TriageCardFeature.State {
        let id = document.key.replacingOccurrences(of: "card:", with: "")
        var state = TriageCardFeature.State(id: id, dataEpoch: document.dataEpoch)
        state.screen.document = document
        state.screen.phase = .loaded
        return state
    }

    static func triage(
        queue: [String], cards: [ViewDocument] = [], home: ViewDocument? = nil,
        edit: (inout TriageFeature.State) -> Void = { _ in }
    ) throws -> StoreOf<TriageFeature> {
        var state = TriageFeature.State()
        let document = try home ?? Fixtures.home(queue: queue)
        state.screen.document = document
        state.screen.phase = .loaded
        state.orderedIDs = queue
        state.cards = IdentifiedArray(uniqueElements: cards.map(cardState))
        edit(&state)
        return triage(state: state)
    }

    /// Waiting for You starts its timers and reloads its cards from `.task`, and every variant of a
    /// test draws the same store: a store that ignores its actions keeps each variant on the state
    /// the test built, however long the previous one took to render.
    static func triage(state: TriageFeature.State) -> StoreOf<TriageFeature> {
        Store(initialState: state) { EmptyReducer() }
    }

    static func cardDetail(
        _ document: ViewDocument, edit: (inout CardDetailFeature.State) -> Void = { _ in }
    ) -> StoreOf<CardDetailFeature> {
        var state = CardDetailFeature.State(key: ViewKey(rawValue: document.key))
        state.screen.document = document
        state.screen.phase = .loaded
        state.actions.dataEpoch = document.dataEpoch
        edit(&state)
        return Store(initialState: state) { CardDetailFeature() } withDependencies: { quiet(&$0) }
    }

    static func cardDetail(state: CardDetailFeature.State) -> StoreOf<CardDetailFeature> {
        Store(initialState: state) { CardDetailFeature() } withDependencies: { quiet(&$0) }
    }
}
