import AlondraFeatures
import ComposableArchitecture
import SwiftUI
import XCTest

/// The sheet Connect opens on the card of the tasks that wait for an MCP server, one capture per
/// phase: each shows only its own status and buttons. A sign-in that goes through closes the
/// sheet, so there is no capture of it.
@MainActor
final class McpSignInSnapshotTests: XCTestCase {
    override func setUp() async throws { ScreenStores.pinTime() }

    /// The glass close button samples what is behind it and differs by a few pixels between runs.
    private static let glass: Float = 0.999

    private func sheet(_ phase: McpSignInFeature.Phase, edit: (inout McpSignInFeature.State) -> Void = { _ in }) -> some View {
        var state = McpSignInFeature.State(card: "mcp-sign-in-t-7", server: "Linear")
        state.phase = phase
        edit(&state)
        let store = Store(initialState: state) { McpSignInFeature() } withDependencies: {
            $0.viewsClient.observe = { _ in AsyncThrowingStream { _ in } }
            $0.mcpSignIn.connect = { _ in
                for await _ in AsyncStream<Never> { _ in } {}
                throw CancellationError()
            }
        }
        return ScreenFrame(height: 420) { McpSignInSheet(store: store) }
    }

    func test_opening_spanish() {
        assertScreen([.light, .dark], language: .spanish, inWindow: true, pixelPrecision: Self.glass) { sheet(.opening) }
    }

    func test_listening() {
        assertScreen([.light, .dark], inWindow: true, pixelPrecision: Self.glass) { sheet(.signingIn(.listening)) }
    }

    func test_listening_spanish() {
        assertScreen([.light, .dark], language: .spanish, inWindow: true, pixelPrecision: Self.glass) { sheet(.signingIn(.listening)) }
    }

    func test_listeningPageClosed_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true, pixelPrecision: Self.glass) {
            sheet(.signingIn(.listening)) { $0.asksToPaste = true }
        }
    }

    func test_paste_spanish() {
        assertScreen([.light, .dark], language: .spanish, inWindow: true, pixelPrecision: Self.glass) {
            sheet(.signingIn(.paste)) { $0.asksToPaste = true }
        }
    }

    func test_refusedPaste() {
        assertScreen([.light], inWindow: true, pixelPrecision: Self.glass) {
            sheet(.signingIn(.paste)) { $0.asksToPaste = true; $0.pasted = "https://linear.app/oops"; $0.pasteRefused = true }
        }
    }

    func test_sending_spanish() {
        assertScreen([.light, .dark], language: .spanish, inWindow: true, pixelPrecision: Self.glass) { sheet(.sending) }
    }

    func test_reconnecting_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true, pixelPrecision: Self.glass) { sheet(.reconnecting) }
    }

    func test_cut_spanish() {
        assertScreen([.light, .dark], language: .spanish, inWindow: true, pixelPrecision: Self.glass) { sheet(.failed(.cut)) }
    }

    func test_notAReturn_spanish() {
        assertScreen([.light], language: .spanish, inWindow: true, pixelPrecision: Self.glass) {
            sheet(.failed(.notAReturn)) { $0.asksToPaste = true }
        }
    }

    func test_claudeAi_spanish() {
        assertScreen([.light, .dark], language: .spanish, inWindow: true, pixelPrecision: Self.glass) { sheet(.claudeAi) }
    }

    func test_listening_accessibilitySize() {
        assertScreen([.ax3], inWindow: true, pixelPrecision: Self.glass) { sheet(.signingIn(.listening)) }
    }
}
