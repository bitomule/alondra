import AlondraFeatures
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct SignInFeatureTests {
    @Test func candidateDoesNotUnlockContentBeforeApproval() async {
        let (approvals, continuation) = AsyncStream<Void>.makeStream()
        let store = TestStore(initialState: SignInFeature.State()) { SignInFeature() } withDependencies: {
            $0.cloudPairing = SignInClient(restore: { false }, login: {}, begin: { _ in "Ab12Cd34" }, approve: {
                for await _ in approvals { return }
            }, cancel: {}, logout: {})
        }
        await store.send(.appeared) { $0.restorationInFlight = true }
        await store.receive(\.configurationLoaded) { $0.configuration = .missing }
        await store.receive(\.restored) { $0.phase = .signedOut; $0.restorationInFlight = false }
        await store.send(.login) { $0.phase = .loggingIn }
        await store.receive(\.loggedIn) { $0.phase = .scanning }
        await store.send(.scanned("QR")) { $0.payload = "QR" }
        await store.receive(\.submit) { $0.payload = ""; $0.phase = .submitting }
        await store.receive(\.pending) { $0.phase = .pending("Ab12Cd34") }
        continuation.yield(())
        await store.receive(\.approved) { $0.phase = .approved; $0.configuration = .saved }
        continuation.finish()
        await store.finish()
    }
    @Test func failedScanStopsTheCameraInsteadOfResubmittingTheSameQR() async {
        struct Refused: LocalizedError { var errorDescription: String? { "Pairing refused (409)" } }
        var state = SignInFeature.State()
        state.phase = .scanning
        state.configuration = .missing
        state.cameraReady = true
        let store = TestStore(initialState: state) { SignInFeature() } withDependencies: {
            $0.cloudPairing = SignInClient(restore: { false }, login: {}, begin: { _ in throw Refused() },
                approve: {}, cancel: {}, logout: {})
            $0.cloudPairing.describe = { error in
                #expect(error is Refused)
                return "Linking didn’t finish."
            }
        }
        await store.send(.scanned("QR")) { $0.payload = "QR" }
        await store.receive(\.submit) { $0.payload = ""; $0.phase = .submitting }
        await store.receive(\.failed) {
            $0.phase = .scanning; $0.cameraReady = false; $0.error = "Linking didn’t finish."
        }
        await store.finish()
    }
    @Test func cancelledGenerationCannotUnlockContent() async {
        var state = SignInFeature.State()
        state.phase = .pending("Ab12Cd34")
        let store = TestStore(initialState: state) { SignInFeature() }
        await store.send(.cancel) { $0.generation = 1; $0.phase = .signingOut }
        await store.receive(\.loggedOut) { $0.phase = .signedOut; $0.configuration = .missing }
        await store.send(.approved(0))
        await store.send(.pending(0, "stale"))
        await store.finish()
    }
    @Test func theFirstScreenAsksForAPathAndEitherCanSwitchToTheOther() async {
        var state = SignInFeature.State()
        state.phase = .signedOut
        state.configuration = .missing
        let store = TestStore(initialState: state) { SignInFeature() }
        #expect(store.state.path == nil)
        await store.send(.choose(.localNetwork)) { $0.path = .localNetwork }
        await store.send(.showAccount(true))
        await store.receive(\.choose) { $0.path = .account }
        #expect(store.state.showsAccount)
        await store.send(.showAccount(false))
        await store.receive(\.choose) { $0.path = .localNetwork }
    }
    @Test func aPathCannotChangeWhileAMacIsBeingLinkedOnThisNetwork() async {
        var state = SignInFeature.State()
        state.phase = .signedOut
        state.path = .localNetwork
        state.findMac.step = .connecting("Studio")
        let store = TestStore(initialState: state) { SignInFeature() }
        await store.send(.choose(.account))
    }
    @Test func signingOutGoesBackToTheChoiceOfPath() async {
        var state = SignInFeature.State()
        state.phase = .approved
        state.configuration = .saved
        state.path = .account
        let store = TestStore(initialState: state) { SignInFeature() } withDependencies: {
            $0.cloudPairing = SignInClient(restore: { false }, login: {}, begin: { _ in "" }, approve: {}, cancel: {}, logout: {})
        }
        await store.send(.logout) { $0.generation = 1; $0.phase = .signingOut }
        await store.receive(\.loggedOut) { $0.phase = .signedOut; $0.configuration = .missing; $0.path = nil }
    }
}
