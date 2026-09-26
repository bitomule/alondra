import ComposableArchitecture
import Foundation

public enum CloudRestoreError: Error, Sendable {
    case reauthenticationRequired(String)
    case revocationRequired(String)
}

public struct CloudPairingService: Sendable {
    public var hasSavedConfiguration: @Sendable () async throws -> Bool = { false }
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
        public enum Configuration: Equatable { case unknown, missing, saved }
        public var configuration: Configuration = .unknown
        public var restorationInFlight = false
        public var reauthenticationRequired = false
        public var logoutIncomplete = false
        public var showsWorkspace: Bool { configuration != .missing }
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
        case configurationLoaded(Int, Bool), retryRestore, reauthenticate
        case restorationFailed(Int, String, reauthenticationRequired: Bool)
        case revocationRequired(Int, String)
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
                guard state.phase == .restoring, !state.restorationInFlight else { return .none }
                state.restorationInFlight = true
                let generation = state.generation
                return .run { send in
                    do {
                        let configured = try await service.hasSavedConfiguration()
                        await send(.configurationLoaded(generation, configured))
                        if configured { await send(.restored(generation, try await service.restore())) }
                        else { await send(.restored(generation, false)) }
                    } catch CloudRestoreError.revocationRequired(let message) {
                        await send(.revocationRequired(generation, message))
                    } catch CloudRestoreError.reauthenticationRequired(let message) {
                        await send(.restorationFailed(generation, message, reauthenticationRequired: true))
                    } catch {
                        await send(.restorationFailed(generation, error.localizedDescription, reauthenticationRequired: false))
                    }
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .configurationLoaded(let generation, let saved):
                guard generation == state.generation else { return .none }
                state.configuration = saved ? .saved : .missing
                return .none
            case .revocationRequired(let generation, let message):
                guard generation == state.generation else { return .none }
                state.logoutIncomplete = true
                state.configuration = .saved
                state.phase = .restoring
                state.restorationInFlight = false
                state.reauthenticationRequired = false
                state.error = message
                return .none
            case .retryRestore:
                if state.logoutIncomplete { return .send(.logout) }
                guard !state.restorationInFlight else { return .none }
                state.phase = .restoring; state.error = nil; state.reauthenticationRequired = false
                return .send(.appeared)
            case .reauthenticate:
                guard state.configuration == .saved, !state.restorationInFlight, !state.logoutIncomplete else { return .none }
                state.restorationInFlight = true; state.error = nil
                let generation = state.generation
                return .run { send in
                    do {
                        try await service.login()
                        await send(.restored(generation, try await service.restore()))
                    } catch CloudRestoreError.revocationRequired(let message) {
                        await send(.revocationRequired(generation, message))
                    } catch CloudRestoreError.reauthenticationRequired(let message) {
                        await send(.restorationFailed(generation, message, reauthenticationRequired: true))
                    } catch {
                        await send(.restorationFailed(generation, error.localizedDescription, reauthenticationRequired: false))
                    }
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .restorationFailed(let generation, let message, let reauthenticate):
                guard generation == state.generation else { return .none }
                state.restorationInFlight = false; state.error = message
                state.reauthenticationRequired = reauthenticate
                return .none
            case .restored(let generation, let approved):
                guard generation == state.generation else { return .none }
                state.restorationInFlight = false
                state.reauthenticationRequired = false
                state.configuration = approved ? .saved : .missing
                state.phase = approved ? .approved : .signedOut
                return .none
            case .loggedOut(let generation):
                guard generation == state.generation else { return .none }
                state.configuration = .missing; state.restorationInFlight = false; state.reauthenticationRequired = false
                state.phase = .signedOut
                state.logoutIncomplete = false
                return .none
            case .login:
                guard state.phase == .signedOut, !state.logoutIncomplete else { return .none }
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
                state.configuration = .saved
                return .none
            case .failed(let generation, let message):
                guard generation == state.generation else { return .none }
                state.error = message
                switch state.phase {
                case .signingOut:
                    state.phase = .restoring
                    state.configuration = .saved
                    state.logoutIncomplete = true
                    state.reauthenticationRequired = false
                case .submitting, .pending: state.phase = .scanning
                default: state.phase = .signedOut
                }
                return .none
            case .cancel, .logout:
                guard state.phase != .signingOut else { return .none }
                state.restorationInFlight = false
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
