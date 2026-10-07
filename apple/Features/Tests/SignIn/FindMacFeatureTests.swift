import AlondraFeatures
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct FindMacFeatureTests {
    private let studio = FoundMac(id: "Studio", name: "Studio", paired: false)
    private let code = DirectPairingCode(code: "4F2A91", macName: "Studio")

    private func client(pair: @escaping @Sendable (FindMacTarget) async throws -> DirectPairingCode,
                        approve: @escaping @Sendable () async throws -> Void) -> FindMacClient {
        FindMacClient(browse: { AsyncStream { $0.finish() } }, pair: pair, approve: approve, cancel: {},
                      describe: { _ in "The Mac didn’t allow this device." })
    }

    @Test func choosingAMacShowsTheCodeAndPairsOnlyAfterTheMacAllows() async {
        let (approvals, allow) = AsyncStream<Void>.makeStream()
        let store = TestStore(initialState: FindMacFeature.State(macs: [studio])) { FindMacFeature() } withDependencies: {
            $0.findMac = client(pair: { target in
                #expect(target == .found("Studio"))
                return DirectPairingCode(code: "4F2A91", macName: "Studio")
            }, approve: { for await _ in approvals { return } })
        }
        await store.send(.choose("Studio")) { $0.step = .connecting("Studio"); $0.generation = 1 }
        await store.receive(\.codeShown) { $0.step = .comparing(code) }
        allow.yield(())
        await store.receive(\.approved) { $0.step = .approved }
        await store.receive(\.delegate)
        allow.finish()
    }

    @Test func aDeniedRequestReturnsToTheListWithASentence() async {
        struct Denied: Error {}
        let store = TestStore(initialState: FindMacFeature.State(macs: [studio])) { FindMacFeature() } withDependencies: {
            $0.findMac = client(pair: { _ in DirectPairingCode(code: "4F2A91", macName: "Studio") }, approve: { throw Denied() })
        }
        await store.send(.choose("Studio")) { $0.step = .connecting("Studio"); $0.generation = 1 }
        await store.receive(\.codeShown) { $0.step = .comparing(code) }
        await store.receive(\.failed) { $0.step = .browsing; $0.error = "The Mac didn’t allow this device." }
    }

    @Test func anAlreadyPairedMacIsNotPairedTwice() async {
        let paired = FoundMac(id: "Studio", name: "Studio", paired: true)
        let store = TestStore(initialState: FindMacFeature.State(macs: [paired])) { FindMacFeature() }
        await store.send(.choose("Studio")) { $0.error = "This Mac is already linked. Select it in Settings." }
    }

    @Test func addingByAddressUsesWhatThePersonTyped() async {
        let store = TestStore(initialState: FindMacFeature.State()) { FindMacFeature() } withDependencies: {
            $0.findMac = client(pair: { target in
                #expect(target == .address("100.101.2.3"))
                throw CancellationError()
            }, approve: {})
        }
        await store.send(.addByAddress) { $0.addingByAddress = true }
        await store.send(.binding(.set(\.address, " 100.101.2.3 "))) { $0.address = " 100.101.2.3 " }
        await store.send(.submitAddress) { $0.step = .connecting("100.101.2.3"); $0.generation = 1 }
        await store.finish()
    }

    @Test func cancellingWhileComparingStopsTheAttempt() async {
        let cancelled = LockIsolated(false)
        var state = FindMacFeature.State(macs: [studio], step: .comparing(code))
        state.generation = 1
        let store = TestStore(initialState: state) { FindMacFeature() } withDependencies: {
            $0.findMac = FindMacClient(browse: { AsyncStream { $0.finish() } }, pair: { _ in throw CancellationError() },
                approve: {}, cancel: { cancelled.setValue(true) }, describe: { _ in "" })
        }
        await store.send(.cancel) { $0.step = .browsing; $0.generation = 2 }
        await store.finish()
        #expect(cancelled.value)
    }
}
