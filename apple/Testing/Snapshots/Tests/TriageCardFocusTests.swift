import AlondraContract
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SwiftUI
import UIKit
import XCTest

/// The reply field of a card in Waiting for You keeps the keyboard while the person writes: when
/// the keyboard shrinks the card, and when the card's view arrives again from the Mac.
@MainActor
final class TriageCardFocusTests: XCTestCase {
    private struct Host: View {
        let store: StoreOf<TriageFeature>
        @Namespace private var zoom
        var body: some View { TriageView(store: store, zoom: zoom) }
    }

    func test_replyFieldKeepsFocusWhenTheKeyboardShrinksTheCardAndTheCardRefreshes() async throws {
        let id = Fixtures.questionID
        var state = TriageFeature.State()
        state.screen.document = try Fixtures.home(queue: [id])
        state.screen.phase = .loaded
        state.orderedIDs = [id]
        state.cards = [ScreenStores.cardState(try Fixtures.card(id))]
        let store = Store(initialState: state) {
            Reduce<TriageFeature.State, TriageFeature.Action> { _, _ in .none }
                .forEach(\.cards, action: \.cards) { TriageCardFeature() }
        } withDependencies: { idleViewsClient(&$0.viewsClient) }

        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = CGRect(x: 0, y: 0, width: 393, height: 852)
        let host = UIHostingController(rootView: Host(store: store))
        window.rootViewController = host
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        try await settle()

        let field = try XCTUnwrap(textInputs(in: window).first as? UIView & UITextInput)
        XCTAssertTrue(field.becomeFirstResponder())
        try await settle()
        field.insertText("Only if the title stays on one line")
        try await settle()
        XCTAssertTrue(responder(in: window) === field, "the field lost the keyboard as soon as it got it")

        NotificationCenter.default.post(name: UIResponder.keyboardWillShowNotification, object: nil)
        host.additionalSafeAreaInsets.bottom = 336
        try await settle()
        XCTAssertTrue(responder(in: window) === field, "the field lost the keyboard when the card shrank")

        let refreshed = try Fixtures.card("card-question") { $0["meta"] = "Alondra · iOS · refreshed" }
        store.send(.cards(.element(id: id, action: .screen(.loaded(refreshed)))))
        try await settle()
        XCTAssertEqual(store.cards[id: id]?.card?.meta, "Alondra · iOS · refreshed")
        XCTAssertTrue(responder(in: window) === field, "the field lost the keyboard when the card refreshed")
        XCTAssertEqual(store.cards[id: id]?.replyDraft, "Only if the title stays on one line")
        print("[focus-proof] after keyboard and refresh: responder=\(responder(in: window).map { "\(type(of: $0))" } ?? "none"), same field=\(responder(in: window) === field), draft=\"\(store.cards[id: id]?.replyDraft ?? "")\"")
    }

    private func settle() async throws {
        try await Task.sleep(for: .seconds(1))
    }

    private func textInputs(in view: UIView) -> [UIView] {
        ((view is UITextView || view is UITextField) ? [view] : []) + view.subviews.flatMap(textInputs)
    }

    private func responder(in view: UIView) -> UIView? {
        if view.isFirstResponder { return view }
        return view.subviews.lazy.compactMap(responder).first
    }
}
