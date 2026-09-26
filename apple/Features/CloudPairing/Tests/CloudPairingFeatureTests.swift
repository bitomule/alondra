import AlondraCloudPairingFeature
import ComposableArchitecture
import Testing

@MainActor
struct CloudPairingFeatureTests {
    @Test func candidateDoesNotUnlockContentBeforeApproval() async {
        let (approvals, continuation) = AsyncStream<Void>.makeStream()
        let store = TestStore(initialState: CloudPairingFeature.State()) { CloudPairingFeature() } withDependencies: {
            $0.cloudPairing = CloudPairingService(restore: { false }, login: {}, begin: { _ in "Ab12Cd34" }, approve: {
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
    @Test func cancelledGenerationCannotUnlockContent() async {
        var state = CloudPairingFeature.State()
        state.phase = .pending("Ab12Cd34")
        let store = TestStore(initialState: state) { CloudPairingFeature() }
        await store.send(.cancel) { $0.generation = 1; $0.phase = .signingOut }
        await store.receive(\.loggedOut) { $0.phase = .signedOut; $0.configuration = .missing }
        await store.send(.approved(0))
        await store.send(.pending(0, "stale"))
        await store.finish()
    }
}
