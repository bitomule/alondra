import AlondraContract
import AlondraFeatures
import AlondraFeed
import AlondraTransport
import ComposableArchitecture
import Foundation
import XCTest

final class McpSignInFeatureTests: XCTestCase {
    private let card = "mcp-sign-in-t-7"
    private let page = URL(string: "https://linear.app/oauth/authorize?state=s1")!
    private let back = URL(string: "http://localhost:53682/callback?code=one-time&state=s1")!

    @MainActor
    private func store(page: McpSignInPage, events: [McpSignInEvent], giveBack: @escaping @Sendable (String, URL) async throws -> Void = { _, _ in },
                       reconnect: @escaping @Sendable (String) async throws -> Void = { _ in },
                       settles: Bool = false, dismissed: LockIsolated<Int> = LockIsolated(0)) -> TestStoreOf<McpSignInFeature> {
        let store = TestStore(initialState: McpSignInFeature.State(card: card, server: "linear")) { McpSignInFeature() } withDependencies: {
            $0.mcpSignIn.connect = { _ in page }
            $0.mcpSignIn.giveBack = giveBack
            $0.mcpSignIn.reconnect = reconnect
            $0.mcpSignIn.open = { _, _ in AsyncStream { continuation in
                events.forEach { continuation.yield($0) }
                continuation.finish()
            } }
            $0.viewsClient.observe = { _ in
                AsyncThrowingStream { continuation in if settles { continuation.finish(throwing: ViewsClientError.missingView) } }
            }
            $0.dismiss = DismissEffect { dismissed.withValue { $0 += 1 } }
        }
        store.exhaustivity = .off
        return store
    }

    @MainActor
    func testTheReturnThisDevicePicksUpGoesToTheMacAndIsNotKept() async {
        let given = LockIsolated<[String]>([])
        let store = store(page: .browser(url: page, port: 53682), events: [.opened(.listening), .returned(back)],
                          giveBack: { card, address in given.withValue { $0.append("\(card) \(address.absoluteString)") } })
        let appeared = await store.send(.appeared)
        await store.receive(\.gaveBack)
        await appeared.cancel()

        XCTAssertEqual(given.value, ["\(card) \(back.absoluteString)"])
        XCTAssertFalse(store.state.asksToPaste, "nothing to paste when the device picked the return up")
        XCTAssertFalse("\(store.state)".contains("one-time"), "the one-time code is not kept once it went to the Mac")
    }

    @MainActor
    func testWhenNothingHereListensItAsksToPasteAndSendsOnlyASignInReturn() async {
        let given = LockIsolated<[URL]>([])
        let store = store(page: .browser(url: page, port: 53682), events: [.opened(.paste)],
                          giveBack: { _, address in given.withValue { $0.append(address) } })
        let appeared = await store.send(.appeared)
        await store.receive(\.page) { $0.phase = .signingIn(.paste); $0.asksToPaste = true }

        await store.send(.pasted("https://evil.example.com/callback?code=x&state=s1")) { $0.pasted = "https://evil.example.com/callback?code=x&state=s1" }
        await store.receive(\.sendPastedTapped) { $0.pasteRefused = true }
        await store.send(.pasted("http://localhost:49999/callback?code=x&state=s1"))
        await store.receive(\.sendPastedTapped) { $0.pasteRefused = true }
        XCTAssertEqual(given.value, [], "neither another page nor another sign-in's port is sent")

        await store.send(.pasted(" \(back.absoluteString)\n"))
        await store.receive(\.sendPastedTapped) { $0.pasted = ""; $0.pasteRefused = false; $0.phase = .sending }
        await store.receive(\.gaveBack)
        await appeared.cancel()
        XCTAssertEqual(given.value, [back])
    }

    @MainActor
    func testASignInThatReachesTheMacClosesTheSheetAndTellsItsHost() async {
        let dismissed = LockIsolated(0)
        let store = store(page: .browser(url: page, port: 53682), events: [.opened(.listening), .returned(back)], dismissed: dismissed)
        let appeared = await store.send(.appeared)
        await store.receive(\.gaveBack)
        await store.receive(\.delegate.signedIn)
        await appeared.cancel()
        await store.finish()
        XCTAssertEqual(dismissed.value, 1, "nothing is left to do on the sheet once the sign-in reached the Mac")
    }

    @MainActor
    func testAServerAlreadySignedInClosesTheSheetAndTellsItsHost() async {
        let dismissed = LockIsolated(0)
        let store = store(page: .connected, events: [], dismissed: dismissed)
        let appeared = await store.send(.appeared)
        await store.receive(\.connectAnswered)
        await store.receive(\.delegate.signedIn)
        await appeared.cancel()
        await store.finish()
        XCTAssertEqual(dismissed.value, 1)
    }

    @MainActor
    func testOnTheDaemonsMacClaudeTakesTheReturnAndNothingIsAskedToPaste() async {
        let store = store(page: .browser(url: page, port: 53682), events: [.opened(.claudeOnThisMac), .closed])
        let appeared = await store.send(.appeared)
        await store.receive(\.page)
        await store.receive(\.page)
        await appeared.cancel()
        XCTAssertEqual(store.state.phase, .signingIn(.claudeOnThisMac))
        XCTAssertFalse(store.state.asksToPaste)
    }

    @MainActor
    func testTheSignInSheetClosedWithoutAReturnAsksToPaste() async {
        let store = store(page: .browser(url: page, port: 53682), events: [.opened(.listening), .closed])
        let appeared = await store.send(.appeared)
        await store.receive(\.page)
        await store.receive(\.page) { $0.asksToPaste = true }
        await appeared.cancel()
    }

    @MainActor
    func testAClaudeAiConnectorIsConnectedThereAndThenReconnected() async {
        let reconnected = LockIsolated<[String]>([])
        let store = store(page: .browser(url: URL(string: "https://claude.ai/customize/connectors")!, port: nil), events: [],
                          reconnect: { card in reconnected.withValue { $0.append(card) } })
        let appeared = await store.send(.appeared)
        await store.receive(\.connectAnswered) { $0.phase = .claudeAi }
        await store.send(.reconnectTapped) { $0.phase = .reconnecting }
        await store.receive(\.reconnected)
        await appeared.cancel()
        XCTAssertEqual(reconnected.value, [card])
        XCTAssertEqual(store.state.phase, .reconnecting)
    }

    @MainActor
    func testARefusedReturnAsksToPasteAgainAndACutSignInOffersConnectAgain() async {
        let refusal = store(page: .browser(url: page, port: 53682), events: [.opened(.listening), .returned(back)],
                            giveBack: { _, _ in throw APIError.server(status: 422, kind: nil, detail: "Paste the whole address", reasons: []) })
        let first = await refusal.send(.appeared)
        await refusal.receive(\.gaveBack) { $0.phase = .failed(.notAReturn); $0.asksToPaste = true }
        await first.cancel()

        let cut = store(page: .browser(url: page, port: 53682), events: [.opened(.listening), .returned(back)],
                        giveBack: { _, _ in throw APIError.conflict(kind: nil, detail: nil, reasons: []) })
        let second = await cut.send(.appeared)
        await cut.receive(\.gaveBack) { $0.phase = .failed(.cut) }
        await second.cancel()
    }

    @MainActor
    func testTheSheetClosesOnceItsCardNoLongerWaits() async {
        let dismissed = LockIsolated(0)
        let store = store(page: .browser(url: page, port: 53682), events: [], settles: true, dismissed: dismissed)
        let appeared = await store.send(.appeared)
        await store.receive(\.cardSettled)
        await appeared.cancel()
        XCTAssertEqual(dismissed.value, 1)
    }

    /// A server of the MCP servers screen: the sheet closes once no row of it offers Connect.
    @MainActor
    func testASheetOpenedFromTheServersScreenClosesOnceTheServerNeedsNoSignIn() async throws {
        let servers = try Fixtures.document("mcp-servers-ios")
        let rows = servers.sections.flatMap(\.items).compactMap { item -> AlondraContract.Row? in
            if case .row(let row) = item { row } else { nil }
        }
        let target = { (title: String) in try XCTUnwrap(rows.first { $0.title == title }?.id) }
        for (title, closes) in [("linear", false), ("claude.ai Gmail", true)] {
            let dismissed = LockIsolated(0)
            let observed = LockIsolated<[String]>([])
            let card = try target(title)
            let store = TestStore(initialState: McpSignInFeature.State(card: card, server: title)) { McpSignInFeature() } withDependencies: {
                $0.mcpSignIn.connect = { _ in throw APIError.unavailable }
                $0.viewsClient.observe = { key in
                    observed.withValue { $0.append(key.rawValue) }
                    return AsyncThrowingStream { continuation in
                        continuation.yield(.document(servers))
                        continuation.finish()
                    }
                }
                $0.dismiss = DismissEffect { dismissed.withValue { $0 += 1 } }
            }
            store.exhaustivity = .off
            await store.send(.appeared)
            await store.finish()
            XCTAssertEqual(observed.value, ["mcp_servers"], "the screen's server is watched on the screen, never as a card")
            XCTAssertEqual(dismissed.value, closes ? 1 : 0, "\(title) closes the sheet only once no project needs its sign-in")
        }
    }

    func testTheDaemonsReplyOpensOnlyHttpsOrThisDevicesLocalhost() {
        let reply = { (object: [String: Any]) in McpSignInPage(daemonReply: try! JSONSerialization.data(withJSONObject: object)) }
        XCTAssertEqual(reply(["url": page.absoluteString, "returns_to_claude": true, "port": 53682]), .browser(url: page, port: 53682))
        XCTAssertEqual(reply(["url": "https://claude.ai/x", "returns_to_claude": false]), .browser(url: URL(string: "https://claude.ai/x")!, port: nil))
        XCTAssertEqual(reply(["server": "linear", "connected": true]), .connected)
        XCTAssertNil(reply(["url": "http://evil.example.com/", "returns_to_claude": true, "port": 53682]))
        XCTAssertNil(reply(["url": "javascript:alert(1)", "returns_to_claude": true, "port": 53682]))
        XCTAssertNil(reply(["url": page.absoluteString, "returns_to_claude": true]), "a return to Claude says where")
    }
}
