import AlondraCommands
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
@Suite struct DeliveryNoticeFeatureTests {
    private let scope = CommandScope(server: URL(string: "https://example.test")!, deviceID: "phone", credentialRevision: "key", generation: 1)
    private let start = Date(timeIntervalSince1970: 1_000)
    private let id = UUID(uuidString: "00000000-0000-0000-0000-000000000001")!

    @Test(.timeLimit(.minutes(1))) func warningDeadlineFailureAndNewPhaseRequirePersistedAcknowledgement() async {
        let clock = TestClock()
        let updates = AsyncStream<DeliveryStatus>.makeStream()
        let acknowledgements = Acknowledgements()
        let pending = snapshot(.unconfirmed)
        let store = TestStore(initialState: DeliveryNoticeFeature.State()) { DeliveryNoticeFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.date.now = start
            $0.serviceCommandRecovery = ServiceCommandRecoveryClient(observe: { AsyncStream { $0.finish() } },
                observeDelivery: { updates.stream }, acknowledgeDelivery: { phases, scope in
                    try await acknowledgements.record(phases, scope)
                })
        }
        await store.send(.appeared)
        updates.continuation.yield(pending)
        await store.receive(\.updated) { $0.status = pending; $0.now = self.start }
        #expect(store.state.pendingCount == 1)
        await clock.advance(by: .seconds(119))
        #expect(store.state.warnings.isEmpty)
        await clock.advance(by: .seconds(1))
        await store.receive(\.timeAdvanced) { $0.now = self.start.addingTimeInterval(120) }
        #expect(store.state.pendingCount == 0)
        #expect(store.state.warnings.map(\.id) == [id])

        await store.send(.dismissWarnings) { $0.isAcknowledging = true }
        await store.receive(\.acknowledgementFinished) {
            $0.isAcknowledging = false; $0.acknowledgementError = "Cannot persist acknowledgement"
        }
        #expect(store.state.warnings.count == 1)
        let changed = snapshot(.feedbackReadRejected(403))
        updates.continuation.yield(changed)
        await store.receive(\.updated) { $0.status = changed; $0.now = self.start }
        await acknowledgements.succeed()
        await store.send(.dismissWarnings) { $0.isAcknowledging = true; $0.acknowledgementError = nil }
        await store.receive(\.acknowledgementFinished) { $0.isAcknowledging = false }
        #expect(store.state.warnings.count == 1, "An ACK return value is not a persisted snapshot")
        let calls = await acknowledgements.calls
        #expect(calls.map(\.phases) == [[id: .unconfirmed], [id: .feedbackReadRejected(403)]])
        #expect(calls.allSatisfy { $0.scope == scope })
        let saved = snapshot(.feedbackReadRejected(403), acknowledged: true)
        updates.continuation.yield(saved)
        await store.receive(\.updated) { $0.status = saved }
        #expect(store.state.warnings.isEmpty)
        await store.send(.disappeared)
        await store.finish()
    }

    @Test(.timeLimit(.minutes(1))) func disappearingCancelsRealStreamTimerAndNonCooperativeAcknowledgement() async {
        let clock = TestClock()
        let first = AsyncStream<DeliveryStatus>.makeStream()
        let second = AsyncStream<DeliveryStatus>.makeStream()
        let source = ReopeningSource(first.stream, second.stream)
        let acknowledgement = SuspendedAcknowledgement()
        let warning = snapshot(.feedbackReadRejected(403))
        let store = TestStore(initialState: DeliveryNoticeFeature.State()) { DeliveryNoticeFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.date.now = start
            $0.serviceCommandRecovery = ServiceCommandRecoveryClient(observe: { AsyncStream { $0.finish() } },
                observeDelivery: { await source.open() }, acknowledgeDelivery: { _, _ in
                    await acknowledgement.waitIgnoringCancellation()
                    throw AcknowledgementFailure()
                })
        }
        await store.send(.appeared)
        first.continuation.yield(warning)
        await store.receive(\.updated) { $0.status = warning; $0.now = self.start }
        await store.send(.dismissWarnings) { $0.isAcknowledging = true }
        await acknowledgement.started()
        await store.send(.disappeared) { $0.isAcknowledging = false }
        // The same identity opens again; a scope-only check could not distinguish the old ACK.
        await store.send(.appeared)
        let pending = snapshot(.unconfirmed)
        second.continuation.yield(pending)
        await store.receive(\.updated) { $0.status = pending }
        await acknowledgement.release()
        first.continuation.yield(warning)
        first.continuation.finish()
        // The newly installed timer must also disappear without advancing presentation state.
        await store.send(.disappeared)
        second.continuation.finish()
        await clock.advance(by: .seconds(121))
        await store.finish()
        #expect(store.state.status == pending)
        #expect(store.state.now == start)
        #expect(store.state.acknowledgementError == nil)
    }

    private func snapshot(_ phase: DeliveryStatus.Phase, acknowledged: Bool = false) -> DeliveryStatus {
        DeliveryStatus(scope: scope, operations: [.init(id: id, phase: phase,
            firstObservedUncertainAt: start, acknowledged: acknowledged)])
    }
}

private struct AcknowledgementFailure: LocalizedError {
    var errorDescription: String? { "Cannot persist acknowledgement" }
}
private actor Acknowledgements {
    struct Call: Sendable { let phases: [UUID: DeliveryStatus.Phase]; let scope: CommandScope }
    private(set) var calls: [Call] = []
    private var fails = true
    func succeed() { fails = false }
    func record(_ phases: [UUID: DeliveryStatus.Phase], _ scope: CommandScope) throws {
        calls.append(Call(phases: phases, scope: scope))
        if fails { throw AcknowledgementFailure() }
    }
}
private actor ReopeningSource {
    private var streams: [AsyncStream<DeliveryStatus>]
    init(_ first: AsyncStream<DeliveryStatus>, _ second: AsyncStream<DeliveryStatus>) { streams = [first, second] }
    func open() -> AsyncStream<DeliveryStatus> { streams.removeFirst() }
}
private actor SuspendedAcknowledgement {
    private var continuation: CheckedContinuation<Void, Never>?
    private var startWaiters: [CheckedContinuation<Void, Never>] = []
    func waitIgnoringCancellation() async {
        await withCheckedContinuation { continuation in
            self.continuation = continuation
            for waiter in startWaiters { waiter.resume() }
            startWaiters = []
        }
    }
    func started() async {
        if continuation != nil { return }
        await withCheckedContinuation { startWaiters.append($0) }
    }
    func release() { continuation?.resume(); continuation = nil }
}
