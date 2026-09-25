import AlondraSettingsFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct SettingsFeatureTests {
    @Test
    func invalidAddressCannotStartARequestOrBeSaved() async {
        let store = TestStore(initialState: SettingsFeature.State(
            serverAddress: "https://good.example", credentialRevision: "key-a"
        )) { SettingsFeature() }
        await store.send(.draftChanged("wrong-address")) {
            $0.draft = "wrong-address"
            $0.requestRevision = 1
        }
        await store.send(.testTapped)
        await store.send(.saveTapped)
        #expect(store.state.candidateURL == nil)
        #expect(!store.state.canSave)
        await store.finish()
    }

    @Test
    func editingServerCancelsAnInFlightHealthCheck() async {
        let old = URL(string: "https://old.example")!
        let new = URL(string: "https://new.example")!
        let (started, startedContinuation) = AsyncStream<Void>.makeStream()
        let (gate, gateContinuation) = AsyncStream<Void>.makeStream()
        let (canceled, canceledContinuation) = AsyncStream<Void>.makeStream()
        let store = TestStore(initialState: SettingsFeature.State(
            serverAddress: old.absoluteString, credentialRevision: "key-a"
        )) { SettingsFeature() } withDependencies: {
            $0.settingsConnection = SettingsConnection(
                check: { _ in
                    startedContinuation.yield(())
                    await withTaskCancellationHandler {
                        for await _ in gate { return }
                    } onCancel: {
                        canceledContinuation.yield(())
                    }
                },
                save: { _, _ in .saved }
            )
        }
        await store.send(.testTapped) {
            $0.check = .checking
            $0.requestRevision = 1
        }
        var startedIterator = started.makeAsyncIterator()
        _ = await startedIterator.next()
        await store.send(.draftChanged(new.absoluteString)) {
            $0.draft = new.absoluteString
            $0.check = .idle
            $0.requestRevision = 2
        }
        var canceledIterator = canceled.makeAsyncIterator()
        _ = await canceledIterator.next()
        gateContinuation.yield(())
        gateContinuation.finish()
        startedContinuation.finish()
        canceledContinuation.finish()
        await store.send(.checked(old, "key-a", 1, nil))
        await store.finish()
    }

    @Test
    func saveConflictBeforeObservationDoesNotOverwriteOrDismiss() async {
        let store = TestStore(initialState: SettingsFeature.State(
            serverAddress: "https://old.example", credentialRevision: "key-a"
        )) { SettingsFeature() } withDependencies: {
            $0.settingsConnection = SettingsConnection(
                check: { _ in },
                save: { _, expected in
                    #expect(expected == "https://old.example")
                    return .conflict(currentAddress: "https://other-window.example")
                }
            )
        }
        await store.send(.draftChanged("https://my-draft.example")) {
            $0.draft = "https://my-draft.example"
            $0.requestRevision = 1
        }
        await store.send(.saveTapped) {
            $0.configuredAddress = "https://other-window.example"
            $0.hasExternalChange = true
            $0.requestRevision = 2
        }
        #expect(!store.state.saved)
        #expect(store.state.draft == "https://my-draft.example")
        await store.send(.saveTapped)
        await store.finish()
    }

    @Test
    func externalChangePreservesTheDraftUntilUserResolvesIt() async {
        let store = TestStore(initialState: SettingsFeature.State(
            serverAddress: "https://old.example", credentialRevision: "key-a"
        )) { SettingsFeature() }
        await store.send(.draftChanged("https://my-draft.example")) {
            $0.draft = "https://my-draft.example"
            $0.requestRevision = 1
        }
        await store.send(.configurationChanged("https://other-window.example")) {
            $0.configuredAddress = "https://other-window.example"
            $0.hasExternalChange = true
            $0.requestRevision = 2
        }
        #expect(store.state.draft == "https://my-draft.example")
        #expect(!store.state.canSave)
        await store.send(.saveTapped)
        await store.send(.testTapped)
        await store.send(.draftChanged("https://other-window.example")) {
            $0.draft = "https://other-window.example"
            $0.hasExternalChange = false
            $0.requestRevision = 3
        }
        await store.send(.draftChanged("https://my-draft.example")) {
            $0.draft = "https://my-draft.example"
            $0.requestRevision = 4
        }
        await store.send(.configurationChanged("https://third.example")) {
            $0.configuredAddress = "https://third.example"
            $0.hasExternalChange = true
            $0.requestRevision = 5
        }
        await store.send(.keepDraft) { $0.hasExternalChange = false }
        #expect(store.state.canSave)
        await store.send(.credentialChanged("key-b")) {
            $0.credentialRevision = "key-b"
            $0.check = .idle
            $0.requestRevision = 6
        }
        await store.finish()
    }
}
