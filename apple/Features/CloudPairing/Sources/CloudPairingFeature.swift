import ComposableArchitecture
import Foundation

public struct CloudPairingService: Sendable {
    public var prepareCamera: @Sendable () async -> Bool = { false }
    public var restore: @Sendable () async throws -> Bool
    public var login: @Sendable () async throws -> Void
    public var begin: @Sendable (String) async throws -> String
    public var approve: @Sendable () async throws -> Void
    public var cancel: @Sendable () async -> Void
    public var logout: @Sendable () async throws -> Void
    public init(restore: @escaping @Sendable () async throws -> Bool, login: @escaping @Sendable () async throws -> Void,
                begin: @escaping @Sendable (String) async throws -> String, approve: @escaping @Sendable () async throws -> Void,
                cancel: @escaping @Sendable () async -> Void, logout: @escaping @Sendable () async throws -> Void) {
        self.restore = restore; self.login = login; self.begin = begin; self.approve = approve; self.cancel = cancel; self.logout = logout
    }
}
private enum CloudPairingKey: DependencyKey {
    static let liveValue = CloudPairingService(restore: { false }, login: { throw CancellationError() }, begin: { _ in throw CancellationError() }, approve: { throw CancellationError() }, cancel: {}, logout: {})
}
public extension DependencyValues {
    var cloudPairing: CloudPairingService {
        get { self[CloudPairingKey.self] }
        set { self[CloudPairingKey.self] = newValue }
    }
}
@Reducer
public struct CloudPairingFeature {
    @ObservableState
    public struct State: Equatable {
        public var phase: Phase = .restoring
        public var cameraReady = false
        public var payload = ""
        public var error: String?
        public var generation = 0
        public init() {}
    }
    public enum Phase: Equatable { case restoring, signedOut, signingOut, loggingIn, scanning, submitting, pending(String), approved }
    public enum Action: BindableAction {
        case binding(BindingAction<State>), appeared, login, submit, scanned(String), cancel, logout, camera, cameraReady(Bool)
        case restored(Int, Bool), loggedOut(Int), loggedIn(Int), pending(Int, String), approved(Int), failed(Int, String)
    }
    @Dependency(\.cloudPairing) var service
    private enum CancelID { case operation }
    public init() {}
    public var body: some ReducerOf<Self> {
        BindingReducer()
        Reduce { state, action in
            switch action {
            case .binding: return .none
            case .camera:
                guard state.phase == .scanning else { return .none }
                return .run { send in await send(.cameraReady(await service.prepareCamera())) }
            case .cameraReady(let ready):
                guard state.phase == .scanning else { return .none }
                state.cameraReady = ready
                if !ready { state.error = "Camera unavailable. Paste the QR contents or enable camera access in Settings." }
                return .none
            case .appeared:
                guard state.phase == .restoring else { return .none }
                let generation = state.generation
                return .run { send in
                    do { await send(.restored(generation, try await service.restore())) }
                    catch { await send(.failed(generation, error.localizedDescription)) }
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .restored(let generation, let approved):
                guard generation == state.generation else { return .none }
                state.phase = approved ? .approved : .signedOut
                return .none
            case .loggedOut(let generation):
                guard generation == state.generation else { return .none }
                state.phase = .signedOut
                return .none
            case .login:
                guard state.phase == .signedOut else { return .none }
                state.error = nil; state.phase = .loggingIn
                let generation = state.generation
                return .run { send in
                    do { try await service.login(); await send(.loggedIn(generation)) }
                    catch { await send(.failed(generation, error.localizedDescription)) }
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .loggedIn(let generation):
                guard generation == state.generation else { return .none }
                state.phase = .scanning
                return .none
            case .scanned(let payload):
                guard state.phase == .scanning else { return .none }
                state.payload = payload
                return .send(.submit)
            case .submit:
                guard state.phase == .scanning, !state.payload.isEmpty else { return .none }
                state.phase = .submitting; state.error = nil
                let generation = state.generation, payload = state.payload
                state.payload = ""
                return .run { send in
                    do {
                        let code = try await service.begin(payload)
                        await send(.pending(generation, code))
                        try await service.approve()
                        await send(.approved(generation))
                    } catch { await send(.failed(generation, error.localizedDescription)) }
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .pending(let generation, let code):
                guard generation == state.generation else { return .none }
                state.phase = .pending(code)
                return .none
            case .approved(let generation):
                guard generation == state.generation else { return .none }
                state.phase = .approved
                return .none
            case .failed(let generation, let message):
                guard generation == state.generation else { return .none }
                state.error = message
                switch state.phase {
                case .submitting, .pending: state.phase = .scanning
                default: state.phase = .signedOut
                }
                return .none
            case .cancel, .logout:
                guard state.phase != .signingOut else { return .none }
                state.generation += 1; state.phase = .signingOut; state.payload = ""; state.error = nil
                let generation = state.generation
                return .concatenate(.cancel(id: CancelID.operation), .run { send in
                    await service.cancel()
                    do { try await service.logout(); await send(.loggedOut(generation)) }
                    catch { await send(.failed(generation, error.localizedDescription)) }
                })
            }
        }
    }
}
