import AlondraFeatures
import ComposableArchitecture
import SwiftUI
import XCTest

/// The sheet Connect opens on the card of the tasks that wait for an MCP server: the page open
/// while the iPhone listens for its return, the field to paste where it came back to, sending it,
/// a sign-in that was cut, and done.
@MainActor
final class McpSignInSnapshotTests: XCTestCase {
    override func setUp() async throws { ScreenStores.pinTime() }

    private func sheet(_ phase: McpSignInFeature.Phase, edit: (inout McpSignInFeature.State) -> Void = { _ in }) -> some View {
        var state = McpSignInFeature.State(card: "mcp-sign-in-t-7", server: "Linear")
        state.phase = phase
        edit(&state)
        let store = Store(initialState: state) { McpSignInFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
        }
        return ScreenFrame(height: 420) { McpSignInSheet(store: store) }
    }

    func test_listening() {
        assertScreen([.light, .dark], inWindow: true) { sheet(.signingIn(.listening)) }
    }

    func test_paste_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) {
            sheet(.signingIn(.paste)) { $0.asksToPaste = true }
        }
    }

    func test_refusedPaste() {
        assertScreen([.light], inWindow: true) {
            sheet(.signingIn(.paste)) { $0.asksToPaste = true; $0.pasted = "https://linear.app/oops"; $0.pasteRefused = true }
        }
    }

    func test_sending_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { sheet(.sending) }
    }

    func test_cut() {
        assertScreen([.light], inWindow: true) { sheet(.failed(.cut)) }
    }

    func test_connected_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true) { sheet(.connected) }
    }

    func test_claudeAi() {
        assertScreen([.light], inWindow: true) { sheet(.claudeAi) }
    }

    func test_listening_accessibilitySize() {
        assertScreen([.ax3], inWindow: true) { sheet(.signingIn(.listening)) }
    }
}
