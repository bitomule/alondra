import AlondraContract
import ComposableArchitecture
import Foundation

public enum RestoreError: Error, Sendable {
    case reauthenticationRequired
    case revocationRequired
}

public struct SignInClient: Sendable {
    public var hasSavedConfiguration: @Sendable () async throws -> Bool = { false }
    /// After signing out: brings back the Macs paired on the local network, if any are left.
    public var keepsLocalMacs: @Sendable () async -> Bool = { false }
    public var prepareCamera: @Sendable () async -> Bool = { false }
    public var classify: @Sendable (any Error) -> ConnectionProblem = { _ in .unexpected }
    /// What a failed sign-in, pairing or sign-out step tells the person, in their language.
    public var describe: @Sendable (any Error) -> String = { _ in String(localized: "Something went wrong. Try again.") }
    public var restore: @Sendable () async throws -> Bool
    public var login: @Sendable () async throws -> Void
    public var begin: @Sendable (String) async throws -> String
    public var approve: @Sendable () async throws -> Void
    public var cancel: @Sendable () async -> Void
    public var logout: @Sendable () async throws -> Void
    public var alerts = Alerts.unavailable
    public init(restore: @escaping @Sendable () async throws -> Bool, login: @escaping @Sendable () async throws -> Void,
                begin: @escaping @Sendable (String) async throws -> String, approve: @escaping @Sendable () async throws -> Void,
                cancel: @escaping @Sendable () async -> Void, logout: @escaping @Sendable () async throws -> Void) {
        self.restore = restore; self.login = login; self.begin = begin; self.approve = approve; self.cancel = cancel; self.logout = logout
    }
}
public extension SignInClient {
    /// The step that offers alerts after a Mac is linked through the account. Where alerts
    /// cannot be offered (the Mac app), the permission reads as allowed and the step never shows.
    struct Alerts: Sendable {
        public var permission: @Sendable () async -> NotificationSettings.Permission
        public var request: @Sendable () async -> Void
        public var openSettings: @Sendable () async -> Void
        public init(permission: @escaping @Sendable () async -> NotificationSettings.Permission,
                    request: @escaping @Sendable () async -> Void, openSettings: @escaping @Sendable () async -> Void) {
            self.permission = permission; self.request = request; self.openSettings = openSettings
        }
        public static let unavailable = Alerts(permission: { .authorized }, request: {}, openSettings: {})
    }
}
private enum SignInKey: DependencyKey {
    static let liveValue = SignInClient(restore: { false }, login: { throw CancellationError() }, begin: { _ in throw CancellationError() }, approve: { throw CancellationError() }, cancel: {}, logout: {})
}
public extension DependencyValues {
    var cloudPairing: SignInClient {
        get { self[SignInKey.self] }
        set { self[SignInKey.self] = newValue }
    }
}
@Reducer
public struct SignInFeature: Sendable {
    @ObservableState
    public struct State: Equatable {
        public enum Configuration: Equatable { case unknown, missing, saved }
        public let additionalMac: Bool
        /// Settings signs in to the account here, then links the Mac through it.
        public let linksAccount: Bool
        public var signedIn = false
        public var configuration: Configuration = .unknown
        public var restorationInFlight = false
        /// Why the saved workspace could not be restored. Set only after a failure, and kept while
        /// the automatic retries run so the connection screen does not flicker.
        public var restoreProblem: ConnectionProblem?
        public var restoreFailures = 0
        public var reauthenticationRequired = false
        public var logoutIncomplete = false
        public var showsWorkspace: Bool { configuration != .missing }
        public var phase: Phase = .restoring
        public var cameraReady = false
        public var payload = ""
        public var error: String?
        public var generation = 0
        public var findMac = FindMacFeature.State()
        /// The way in the person picked on the first screen; `nil` until they pick one.
        public var path: Path?
        public var showsAccount: Bool { path == .account }
        /// Linking through the account finishes on its own screens: the workspace waits for them.
        public var finishingPairing: Bool {
            switch phase {
            case .pending, .alerts: !additionalMac
            default: false
            }
        }
        public init(additionalMac: Bool = false, linksAccount: Bool = false) {
            self.additionalMac = additionalMac || linksAccount
            self.linksAccount = linksAccount
            if linksAccount { configuration = .saved; phase = .signedOut; path = .account }
            else if additionalMac { configuration = .saved; phase = .scanning }
        }
    }
    /// With the account this iPhone gets alerts and reaches the Mac from anywhere; on the local
    /// network it needs no account and gets no alerts.
    public enum Path: Equatable, Sendable { case account, localNetwork }
    public enum Phase: Equatable { case restoring, signedOut, signingOut, loggingIn, scanning, submitting, cancelling, pending(String), alerts(NotificationSettings.Permission), approved }
    public enum Action: BindableAction {
        case binding(BindingAction<State>), appeared, login, submit, scanned(String), cancel, logout, camera, cameraReady(Bool)
        case configurationLoaded(Int, Bool), retryRestore, retryNow, reauthenticate
        case restorationFailed(Int, ConnectionProblem)
        case revocationRequired(Int)
        case cancelled(Int)
        case restored(Int, Bool), loggedOut(Int), loggedIn(Int), pending(Int, String), approved(Int, NotificationSettings.Permission), failed(Int, String?)
        case turnOnAlerts, skipAlerts, alertsAnswered(Int), offerAlerts(NotificationSettings.Permission)
        case findMac(FindMacFeature.Action), showAccount(Bool), choose(Path)
    }
    @Dependency(\.cloudPairing) var service
    @Dependency(\.continuousClock) var clock
    @Dependency(\.withRandomNumberGenerator) var random
    private enum CancelID { case operation, restoreRetry }
    public init() {}
    public var body: some ReducerOf<Self> {
        BindingReducer()
        Scope(state: \.findMac, action: \.findMac) { FindMacFeature() }
        Reduce { state, action in
            switch action {
            case .binding: return .none
            case .findMac(.delegate(.paired)):
                state.phase = .approved
                state.configuration = .saved
                return .none
            case .findMac: return .none
            case .showAccount(let shows):
                return .send(.choose(shows ? .account : .localNetwork))
            case .choose(let path):
                guard state.findMac.step == .browsing else { return .none }
                state.path = path
                state.error = nil
                return .none
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
                    } catch RestoreError.revocationRequired {
                        await send(.revocationRequired(generation))
                    } catch RestoreError.reauthenticationRequired {
                        AccountDiagnostics.personSees(.sessionExpired, after: "RestoreError.reauthenticationRequired")
                        await send(.restorationFailed(generation, .sessionExpired))
                    } catch {
                        await send(.restorationFailed(generation, service.classify(error)))
                    }
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .configurationLoaded(let generation, let saved):
                guard generation == state.generation else { return .none }
                state.configuration = saved ? .saved : .missing
                return .none
            case .revocationRequired(let generation):
                guard generation == state.generation else { return .none }
                state.logoutIncomplete = true
                state.configuration = .saved
                state.phase = .restoring
                state.restorationInFlight = false
                state.reauthenticationRequired = false
                state.restoreProblem = .signOutIncomplete
                state.error = nil
                return .none
            case .retryRestore:
                if state.logoutIncomplete { return .send(.logout) }
                guard !state.restorationInFlight else { return .none }
                state.phase = .restoring; state.error = nil; state.reauthenticationRequired = false
                return .concatenate(.cancel(id: CancelID.restoreRetry), .send(.appeared))
            case .retryNow:
                guard let problem = state.restoreProblem, !problem.needsAction,
                      !state.restorationInFlight, !state.logoutIncomplete else { return .none }
                return .send(.retryRestore)
            case .reauthenticate:
                guard state.configuration == .saved, !state.restorationInFlight, !state.logoutIncomplete else { return .none }
                state.restorationInFlight = true; state.error = nil
                let generation = state.generation
                return .run { send in
                    do {
                        try await service.login()
                        await send(.restored(generation, try await service.restore()))
                    } catch RestoreError.revocationRequired {
                        await send(.revocationRequired(generation))
                    } catch RestoreError.reauthenticationRequired {
                        AccountDiagnostics.personSees(.sessionExpired, after: "RestoreError.reauthenticationRequired")
                        await send(.restorationFailed(generation, .sessionExpired))
                    } catch {
                        await send(.restorationFailed(generation, service.classify(error)))
                    }
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .restorationFailed(let generation, let problem):
                guard generation == state.generation else { return .none }
                state.restorationInFlight = false
                state.restoreProblem = problem
                state.reauthenticationRequired = problem == .sessionExpired
                guard !problem.needsAction else { return .none }
                let delay = ConnectionBackoff.standard.delay(attempt: state.restoreFailures,
                    random: random { Double($0.next() >> 11) / Double(1 << 53) })
                state.restoreFailures += 1
                return .run { send in
                    try await clock.sleep(for: delay)
                    await send(.retryRestore)
                }.cancellable(id: CancelID.restoreRetry, cancelInFlight: true)
            case .restored(let generation, let approved):
                guard generation == state.generation else { return .none }
                state.restorationInFlight = false
                state.reauthenticationRequired = false
                state.restoreProblem = nil
                state.restoreFailures = 0
                if state.phase == .signingOut { state.logoutIncomplete = false }
                state.configuration = approved ? .saved : .missing
                state.phase = approved ? .approved : .signedOut
                return .none
            case .loggedOut(let generation):
                guard generation == state.generation else { return .none }
                state.configuration = .missing; state.restorationInFlight = false; state.reauthenticationRequired = false
                state.phase = .signedOut
                state.path = nil
                state.logoutIncomplete = false
                return .none
            case .login:
                guard state.phase == .signedOut, !state.logoutIncomplete else { return .none }
                state.error = nil; state.phase = .loggingIn
                let generation = state.generation
                return .run { send in
                    do { try await service.login(); await send(.loggedIn(generation)) }
                    catch { await send(.failed(generation, error.isCancellation ? nil : service.describe(error))) }
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .loggedIn(let generation):
                guard generation == state.generation else { return .none }
                state.signedIn = true
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
                        await send(.approved(generation, await service.alerts.permission()))
                    } catch { await send(.failed(generation, error.isCancellation ? nil : service.describe(error))) }
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .pending(let generation, let code):
                guard generation == state.generation else { return .none }
                state.phase = .pending(code)
                return .none
            case .approved(let generation, let permission):
                guard generation == state.generation else { return .none }
                state.phase = permission == .notDetermined || permission == .denied ? .alerts(permission) : .approved
                state.configuration = .saved
                return .none
            case .turnOnAlerts:
                guard case .alerts(let permission) = state.phase else { return .none }
                let generation = state.generation
                return .run { send in
                    if permission == .denied { await service.alerts.openSettings() } else { await service.alerts.request() }
                    await send(.alertsAnswered(generation))
                }.cancellable(id: CancelID.operation, cancelInFlight: true)
            case .offerAlerts(let permission):
                guard !state.additionalMac, state.phase == .approved,
                      permission == .notDetermined || permission == .denied else { return .none }
                state.phase = .alerts(permission)
                return .none
            case .skipAlerts:
                guard case .alerts = state.phase else { return .none }
                state.phase = .approved
                return .none
            case .alertsAnswered(let generation):
                guard generation == state.generation, case .alerts = state.phase else { return .none }
                state.phase = .approved
                return .none
            case .failed(let generation, let message):
                guard generation == state.generation else { return .none }
                state.error = message
                switch state.phase {
                case .signingOut:
                    // The restore screen names this state; the cleanup error stays out of it.
                    state.error = nil
                    state.phase = .restoring
                    state.configuration = .saved
                    state.logoutIncomplete = true
                    state.reauthenticationRequired = false
                case .submitting, .pending:
                    // The Mac still shows the QR this attempt consumed; a live camera would
                    // submit it again at once. Scanning again is the person's choice.
                    state.phase = .scanning
                    state.cameraReady = false
                default: state.phase = .signedOut
                }
                return .none
            case .cancelled(let generation):
                guard generation == state.generation, state.additionalMac, state.phase == .cancelling else { return .none }
                state.phase = state.linksAccount && !state.signedIn ? .signedOut : .scanning
                return .none
            case .cancel, .logout:
                if state.additionalMac {
                    guard state.phase != .cancelling else { return .none }
                    // Closing/retrying a pairing sheet never signs out the account or
                    // destroys the existing Mac's session and notification contexts.
                    state.generation += 1; state.phase = .cancelling; state.payload = ""; state.error = nil
                    let generation = state.generation
                    return .concatenate(.cancel(id: CancelID.operation), .run { send in
                        await service.cancel()
                        await send(.cancelled(generation))
                    })
                }
                guard state.phase != .signingOut else { return .none }
                state.restorationInFlight = false
                state.restoreProblem = nil
                state.restoreFailures = 0
                state.generation += 1; state.phase = .signingOut; state.payload = ""; state.error = nil
                let generation = state.generation
                return .concatenate(.cancel(id: CancelID.operation), .cancel(id: CancelID.restoreRetry), .run { send in
                    await service.cancel()
                    do { try await service.logout() }
                    catch { await send(.failed(generation, service.describe(error))); return }
                    // Macs paired on the local network never depended on the account: the
                    // person stays in the workspace with them.
                    if await service.keepsLocalMacs() { await send(.restored(generation, true)) }
                    else { await send(.loggedOut(generation)) }
                })
            }
        }
    }
}
