import ComposableArchitecture
import Foundation

/// Find your Mac: the Macs Bonjour sees on this network, the code to compare, and the wait for
/// the person to allow this device on the Mac.
@Reducer
public struct FindMacFeature: Sendable {
    @ObservableState
    public struct State: Equatable {
        public enum Step: Equatable {
            case browsing
            case connecting(String)
            case comparing(DirectPairingCode)
            case approved
        }

        public var macs: [FoundMac] = []
        public var step: Step = .browsing
        public var addingByAddress = false
        public var address = ""
        public var error: String?
        public var generation = 0

        public init(macs: [FoundMac] = [], step: Step = .browsing, error: String? = nil) {
            self.macs = macs
            self.step = step
            self.error = error
        }

        public var canSubmitAddress: Bool { step == .browsing && !address.trimmingCharacters(in: .whitespaces).isEmpty }
    }

    public enum Action: BindableAction {
        case binding(BindingAction<State>)
        case appeared, disappeared
        case macsChanged([FoundMac])
        case choose(String), addByAddress, submitAddress, cancel
        case codeShown(Int, DirectPairingCode), approved(Int), failed(Int, String)
        case delegate(Delegate)

        public enum Delegate: Equatable { case paired }
    }

    @Dependency(\.findMac) var client
    private enum CancelID { case browse, pairing }

    public init() {}

    public var body: some ReducerOf<Self> {
        BindingReducer()
        Reduce { state, action in
            switch action {
            case .binding, .delegate: return .none
            case .appeared:
                return .run { send in
                    for await macs in client.browse() { await send(.macsChanged(macs)) }
                }.cancellable(id: CancelID.browse, cancelInFlight: true)
            case .disappeared:
                return .cancel(id: CancelID.browse)
            case .macsChanged(let macs):
                state.macs = macs
                return .none
            case .choose(let id):
                guard state.step == .browsing, let mac = state.macs.first(where: { $0.id == id }) else { return .none }
                guard !mac.paired else {
                    state.error = String(localized: "This Mac is already linked. Select it in Settings.")
                    return .none
                }
                state.step = .connecting(mac.name)
                return start(.found(id), state: &state)
            case .addByAddress:
                state.addingByAddress.toggle()
                return .none
            case .submitAddress:
                guard state.canSubmitAddress else { return .none }
                let address = state.address.trimmingCharacters(in: .whitespaces)
                state.step = .connecting(address)
                return start(.address(address), state: &state)
            case .codeShown(let generation, let code):
                guard generation == state.generation else { return .none }
                state.step = .comparing(code)
                return .none
            case .approved(let generation):
                guard generation == state.generation else { return .none }
                state.step = .approved
                return .send(.delegate(.paired))
            case .failed(let generation, let message):
                guard generation == state.generation else { return .none }
                state.step = .browsing
                state.error = message
                return .none
            case .cancel:
                guard state.step != .browsing, state.step != .approved else { return .none }
                state.generation += 1
                state.step = .browsing
                state.error = nil
                return .concatenate(.cancel(id: CancelID.pairing), .run { _ in await client.cancel() })
            }
        }
    }

    private func start(_ target: FindMacTarget, state: inout State) -> Effect<Action> {
        state.error = nil
        state.generation += 1
        let generation = state.generation
        return .run { send in
            do {
                let code = try await client.pair(target)
                await send(.codeShown(generation, code))
                try await client.approve()
                await send(.approved(generation))
            } catch is CancellationError {
            } catch {
                await send(.failed(generation, client.describe(error)))
            }
        }.cancellable(id: CancelID.pairing, cancelInFlight: true)
    }
}
