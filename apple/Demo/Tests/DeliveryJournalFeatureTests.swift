import AlondraContracts
import AlondraRecovery
import AlondraWorkspaceFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor struct DeliveryJournalFeatureTests {
    private let scope = CommandScope(server: URL(string: "https://relay.test")!, deviceID: "phone", credentialRevision: "mac-a", generation: 1)
    private let attempt = DeliveryAttempt(id: 7, sessionID: "s", channel: "menu_decision", state: "uncertain", version: 2, questionID: 42)

    @Test func successfulRefreshClearsErrorAndKeepsUnchangedObservedDecision() async {
        let snapshot = DeliveryJournalSnapshot(scope: scope, macID: "mac-a", attempts: [attempt])
        var initial = DeliveryJournalFeature.State()
        initial.active = true; initial.epoch = 1; initial.snapshot = snapshot
        initial.selected = attempt; initial.terminalChecked = true; initial.duplicateRisk = true
        let store = TestStore(initialState: initial) { DeliveryJournalFeature() } withDependencies: {
            $0.deliveryJournal = DeliveryJournalClient(refresh: { snapshot }, resolve: { _, _, _, _, _ in throw DeliveryJournalError.obsolete })
        }
        await store.send(.failed(1, "Network unavailable")) { $0.error = "Network unavailable" }
        await store.send(.refresh) { $0.loading = true }
        await store.receive(\.loaded) { $0.loading = false; $0.error = nil }
        #expect(store.state.selected == attempt && store.state.terminalChecked && store.state.duplicateRisk)
        await store.finish()
    }

    @Test func changedVersionRevokesConfirmationAndOldLifecycleCannotPublish() async {
        let snapshot = DeliveryJournalSnapshot(scope: scope, macID: "mac-a", attempts: [attempt])
        var initial = DeliveryJournalFeature.State()
        initial.active = true; initial.epoch = 1; initial.snapshot = snapshot
        let store = TestStore(initialState: initial) { DeliveryJournalFeature() }
        await store.send(.select(7)) { $0.selected = self.attempt }
        await store.send(.terminalChecked(true)) { $0.terminalChecked = true }
        await store.send(.duplicateRisk(true)) { $0.duplicateRisk = true }
        let changed = DeliveryJournalSnapshot(scope: scope, macID: "mac-a", attempts: [DeliveryAttempt(id: 7, sessionID: "s", channel: "menu_decision", state: "uncertain", version: 3, questionID: 42)])
        await store.send(.loaded(1, changed)) {
            $0.snapshot = changed; $0.selected = nil; $0.terminalChecked = false; $0.duplicateRisk = false
            $0.error = DeliveryJournalError.changed.localizedDescription
        }
        await store.send(.resolve(retry: true))
        await store.send(.disappeared) { $0.active = false; $0.epoch = 2 }
        await store.send(.loaded(1, snapshot))
        await store.send(.resolved(1, snapshot))
        #expect(store.state.snapshot == changed)
        await store.finish()
    }

    @Test func explicitRetryCapturesVersionAndNeverMarksFailedDecisionSuccessful() async {
        let snapshot = DeliveryJournalSnapshot(scope: scope, macID: "mac-a", attempts: [attempt])
        var initial = DeliveryJournalFeature.State()
        initial.active = true; initial.epoch = 1; initial.snapshot = snapshot; initial.selected = attempt
        let store = TestStore(initialState: initial) { DeliveryJournalFeature() } withDependencies: {
            $0.deliveryJournal = DeliveryJournalClient(refresh: { snapshot }, resolve: { observed, scope, retry, checked, risk in
                #expect(observed.id == 7 && observed.version == 2 && scope.credentialRevision == "mac-a")
                #expect(retry && checked && risk)
                throw DeliveryJournalError.outcomeUnknown
            })
        }
        await store.send(.resolve(retry: true))
        await store.send(.terminalChecked(true)) { $0.terminalChecked = true }
        await store.send(.resolve(retry: true))
        await store.send(.duplicateRisk(true)) { $0.duplicateRisk = true }
        await store.send(.resolve(retry: true)) { $0.epoch = 2; $0.resolving = true }
        await store.receive(\.resolutionFailed) {
            $0.resolving = false; $0.selected = nil; $0.terminalChecked = false; $0.duplicateRisk = false
            $0.error = DeliveryJournalError.outcomeUnknown.localizedDescription
        }
        #expect(store.state.snapshot == snapshot)
        await store.finish()
    }
}
