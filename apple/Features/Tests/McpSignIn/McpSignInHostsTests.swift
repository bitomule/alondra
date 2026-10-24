import AlondraFeatures
import ComposableArchitecture
import XCTest

/// The sign-in sheet closes by itself once it reaches the Mac; each place that opens it says so.
@MainActor
final class McpSignInHostsTests: XCTestCase {
    private let signedIn = McpSignInFeature.Action.delegate(.signedIn(server: "Linear"))

    func testTheIPhoneSaysTheSignInWentThrough() async {
        var state = AppFeature.State()
        state.mcpSignIn = McpSignInFeature.State(card: "mcp-sign-in-t-7", server: "Linear")
        let store = TestStore(initialState: state) { AppFeature() } withDependencies: {
            $0.continuousClock = TestClock()
        }
        store.exhaustivity = .off
        await store.send(.mcpSignIn(.presented(signedIn)))
        XCTAssertEqual(store.state.notice?.event, .actionCompleted("Signed in to Linear: the waiting tasks go on."))
    }

    func testTheMacSaysTheSignInWentThrough() async {
        var state = MacPresentationFeature.State()
        state.mcpSignIn = McpSignInFeature.State(card: "mcp-sign-in-t-7", server: "Linear")
        let store = TestStore(initialState: state) { MacPresentationFeature() }
        store.exhaustivity = .off
        await store.send(.mcpSignIn(.presented(signedIn)))
        XCTAssertEqual(store.state.mcpNotice, .actionCompleted("Signed in to Linear: the waiting tasks go on."))
    }

    func testTheServersScreenSaysTheSignInWentThrough() async {
        var state = ServerSettingsFeature.State(key: .mcpServers, client: .ios, lang: "en")
        state.signIn = McpSignInFeature.State(card: "mcp-server-linear", server: "Linear")
        let store = TestStore(initialState: state) { ServerSettingsFeature() }
        store.exhaustivity = .off
        await store.send(.signIn(.presented(signedIn)))
        XCTAssertEqual(store.state.notice, "Signed in to Linear: the waiting tasks go on.")
    }
}
