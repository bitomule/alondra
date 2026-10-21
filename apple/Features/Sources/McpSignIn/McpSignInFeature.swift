import AlondraContract
import AlondraFeed
import AlondraTransport
import ComposableArchitecture
import Foundation

/// Signing in to the MCP server the tasks of a card wait for, from this device. Connect asks the
/// Mac for the sign-in page and opens it; the browser then comes back to `localhost:<port>`.
/// On the daemon's Mac that is Claude's own listener; anywhere else this device listens on that
/// port, or the user pastes the address of the page that did not load. Either way the address goes
/// to the Mac and is never kept here: not in a draft, a journal or a log.
@Reducer
public struct McpSignInFeature: Sendable {
    public init() {}

    @ObservableState
    public struct State: Equatable, Identifiable {
        public let card: String
        public let server: String
        public var phase = Phase.opening
        var page: URL?
        var port: UInt16?
        /// Whether the sheet asks to paste where the browser came back to.
        public var asksToPaste = false
        public var pasted = ""
        public var pasteRefused = false
        public var id: String { card }

        public init(card: String, server: String) {
            self.card = card
            self.server = server
        }
    }

    public enum Phase: Equatable, Sendable {
        case opening
        /// The sign-in page is open, and `route` says where the browser goes once the user signed in.
        case signingIn(McpReturnRoute)
        /// A claude.ai connector: it is connected on claude.ai, then reconnected here.
        case claudeAi
        case sending
        case reconnecting
        case connected
        case failed(Failure)
    }

    public enum Failure: Equatable, Sendable {
        /// What was pasted is not where a sign-in comes back to.
        case notAReturn
        /// Claude no longer waits for this sign-in: its node stopped, or another sign-in replaced it.
        case cut
        /// No node that waits for the server runs now.
        case noNode
        case unreachable
    }

    public enum Action: BindableAction, Sendable {
        case appeared
        case binding(BindingAction<State>)
        case connectAnswered(McpSignInPage?, Failure?)
        case page(McpSignInEvent)
        case pasted(String)
        case sendPastedTapped
        case gaveBack(Failure?)
        case connectAgainTapped
        case reconnectTapped
        case reconnected(Failure?)
        case cardSettled
        case closeTapped
    }

    @Dependency(\.mcpSignIn) var client
    @Dependency(\.viewsClient) var viewsClient
    @Dependency(\.dismiss) var dismiss
    private enum CancelID { case page, settled }

    public var body: some ReducerOf<Self> {
        BindingReducer()
        Reduce { state, action in
            switch action {
            case .appeared:
                return .merge(state.phase == .opening ? connect(&state) : .none, watchCard(state.card))
            case .binding(\.pasted):
                state.pasteRefused = false
                return .none
            case .binding:
                return .none
            case .connectAgainTapped:
                return connect(&state)
            case let .connectAnswered(_, failure?):
                state.phase = .failed(failure)
                return .none
            case .connectAnswered(.connected?, nil):
                state.phase = .connected
                return .none
            case let .connectAnswered(.browser(url, port)?, nil):
                state.page = url
                state.port = port
                state.phase = port == nil ? .claudeAi : .signingIn(.paste)
                return .run { [client] send in
                    for await event in client.open(url, port) { await send(.page(event)) }
                }.cancellable(id: CancelID.page, cancelInFlight: true)
            case .connectAnswered(nil, nil):
                state.phase = .failed(.noNode)
                return .none
            case let .page(.opened(route)):
                guard case .signingIn = state.phase else { return .none }
                state.phase = .signingIn(route)
                if route == .paste { state.asksToPaste = true }
                return .none
            case let .page(.returned(address)):
                guard case .signingIn = state.phase else { return .none }
                return giveBack(&state, address)
            case .page(.closed):
                if case .signingIn(let route) = state.phase, route != .claudeOnThisMac { state.asksToPaste = true }
                return .none
            case let .pasted(text):
                state.pasted = text
                return .send(.sendPastedTapped)
            case .sendPastedTapped:
                guard let address = McpReturnAddress.parse(state.pasted, port: state.port) else {
                    state.pasteRefused = true
                    return .none
                }
                return giveBack(&state, address)
            case let .gaveBack(failure):
                state.phase = failure.map(Phase.failed) ?? .connected
                if failure == .notAReturn { state.asksToPaste = true }
                return .none
            case .reconnectTapped:
                state.phase = .reconnecting
                let card = state.card
                return .run { [client] send in
                    do {
                        try await client.reconnect(card)
                        await send(.reconnected(nil))
                    } catch { await send(.reconnected(Self.failure(of: error, opening: true))) }
                }
            case let .reconnected(failure):
                if let failure { state.phase = .failed(failure) }
                return .none
            case .cardSettled, .closeTapped:
                return .run { [dismiss] _ in await dismiss() }
            }
        }
    }

    private func connect(_ state: inout State) -> Effect<Action> {
        state.phase = .opening
        state.asksToPaste = false
        state.pasted = ""
        state.pasteRefused = false
        let card = state.card
        return .concatenate(.cancel(id: CancelID.page), .run { [client] send in
            do { await send(.connectAnswered(try await client.connect(card), nil)) }
            catch { await send(.connectAnswered(nil, Self.failure(of: error, opening: true))) }
        })
    }

    private func giveBack(_ state: inout State, _ address: URL) -> Effect<Action> {
        state.phase = .sending
        state.pasted = ""
        state.pasteRefused = false
        let card = state.card
        return .concatenate(.cancel(id: CancelID.page), .run { [client] send in
            do {
                try await client.giveBack(card, address)
                await send(.gaveBack(nil))
            } catch { await send(.gaveBack(Self.failure(of: error, opening: false))) }
        })
    }

    /// The card leaves the queue once every task that waited goes on: the sheet has nothing left to do.
    private func watchCard(_ card: String) -> Effect<Action> {
        .run { [viewsClient] send in
            do {
                for try await fetch in viewsClient.observe(.card(card)) {
                    guard case .document(let document) = fetch, document.settledNotice == nil else {
                        return await send(.cardSettled)
                    }
                }
            } catch ViewsClientError.missingView {
                await send(.cardSettled)
            }
        } catch: { _, _ in }.cancellable(id: CancelID.settled, cancelInFlight: true)
    }

    /// The toast after Reconnect on the card itself.
    public static func reconnectNotice(_ failure: Failure?) -> WorkspaceNotice.Event {
        switch failure {
        case nil: .actionCompleted(String(localized: "The waiting tasks read their sign-in again"))
        case .unreachable?: .failure(String(localized: "Alondra could not reach your Mac. Try again."))
        case _?: .failure(String(localized: "No task that waits for this server runs now. Try again in a moment."))
        }
    }

    static func failure(of error: any Error, opening: Bool) -> Failure {
        switch error {
        case APIError.server(422, _, _, _): .notAReturn
        case APIError.conflict, APIError.server(404, _, _, _): opening ? .noNode : .cut
        case APIError.server(503, _, _, _), APIError.unavailable: opening ? .noNode : .cut
        default: .unreachable
        }
    }
}
