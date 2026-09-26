import AlondraSettingsFeature
import ComposableArchitecture
import Foundation
import Testing

@MainActor
struct QuietHoursAutosaveIntegrationTests {
    private let original = ServiceQuietHours(enabled: false, start: 1380, end: 480)

    @Test(.timeLimit(.minutes(1)), arguments: [false, true])
    func rapidEditsCoalesceAndFinishEvenAfterSettingsCloses(close: Bool) async throws {
        let snapshots = AsyncStream<ServiceNotificationSettings>.makeStream()
        let started = AsyncStream<Int>.makeStream()
        let release = AsyncStream<Void>.makeStream()
        let finished = AsyncStream<Void>.makeStream()
        let writes = LockIsolated<[(ServiceQuietHours, ServiceQuietHours)]>([])
        let persisted = LockIsolated(original)
        var initial = ServiceSettingsFeature.State(service: "service", device: "phone", supportsNotifications: true, automaticallySavesQuietHours: true)
        initial.baseline = original; initial.draft = original
        initial.notifications = .init(permission: .authorized, delivery: .registered, quietHours: original)
        let store = Store(initialState: initial) {
            CombineReducers {
                ServiceSettingsFeature()
                Reduce<ServiceSettingsFeature.State, ServiceSettingsFeature.Action> { _, action in
                    if case .autosaveFinished = action { finished.continuation.yield(()) }
                    return .none
                }
            }
        } withDependencies: {
            $0.serviceSettings = .init(observe: { snapshots.stream }, save: { next, expected in
                let number = writes.withValue { $0.append((next, expected)); return $0.count }
                started.continuation.yield(number)
                if number == 1 { var gate = release.stream.makeAsyncIterator(); _ = await gate.next() }
                #expect(persisted.value == expected)
                persisted.setValue(next)
                snapshots.continuation.yield(.init(permission: .authorized, delivery: .registered, quietHours: next))
            }, requestPermission: {}, openSystemSettings: {}, checkConnection: {}, logout: {})
        }
        let observation = store.send(.appeared)
        let first = store.send(.quietEnabled(true))
        var calls = started.stream.makeAsyncIterator()
        #expect(await calls.next() == 1)
        await store.send(.quietStart(1320)).finish()
        await store.send(.quietEnd(420)).finish()
        #expect(writes.value.count == 1)
        #expect(store.draft.start == 1320 && store.draft.end == 420)
        if close { await store.send(.disappeared).finish() }
        release.continuation.yield(())
        var completions = finished.stream.makeAsyncIterator()
        _ = await completions.next()
        #expect(await calls.next() == 2)
        _ = await completions.next()
        await first.finish()
        let expected = ServiceQuietHours(enabled: true, start: 1320, end: 420)
        #expect(persisted.value == expected && store.draft == expected)
        #expect(writes.value.count == 2)
        #expect(writes.value[1].1 == writes.value[0].0)
        #expect(!store.saving && !store.editing && !store.conflict)
        await store.send(.autosaveFinished(original, 0, "Late error")).finish()
        #expect(store.error == nil && store.draft == expected)
        if !close { await store.send(.disappeared).finish() }
        await observation.finish()
        snapshots.continuation.finish(); started.continuation.finish()
        release.continuation.finish(); finished.continuation.finish()
    }

    @Test(.timeLimit(.minutes(1)))
    func foreignPreferencesDuringSaveAreNeverOverwrittenByQueuedEdits() async throws {
        struct Conflict: Error {}
        let started = AsyncStream<Void>.makeStream()
        let release = AsyncStream<Void>.makeStream()
        let persisted = LockIsolated(original)
        let calls = LockIsolated(0)
        var initial = ServiceSettingsFeature.State(service: "service", device: "phone", supportsNotifications: true, automaticallySavesQuietHours: true)
        initial.baseline = original; initial.draft = original
        initial.notifications = .init(permission: .authorized, delivery: .registered, quietHours: original)
        let store = Store(initialState: initial) { ServiceSettingsFeature() } withDependencies: {
            $0.serviceSettings.save = { next, expected in
                calls.withValue { $0 += 1 }
                started.continuation.yield(())
                var gate = release.stream.makeAsyncIterator(); _ = await gate.next()
                try persisted.withValue { value in
                    guard value == expected else { throw Conflict() }
                    value = next
                }
            }
        }
        let save = store.send(.quietEnabled(true))
        var callsStarted = started.stream.makeAsyncIterator(); _ = await callsStarted.next()
        await store.send(.quietStart(1320)).finish()
        let foreign = ServiceQuietHours(enabled: true, start: 1200, end: 360)
        persisted.setValue(foreign)
        await store.send(.updated(.init(permission: .authorized, delivery: .registered, quietHours: foreign))).finish()
        #expect(store.conflict)
        release.continuation.yield(())
        await save.finish()
        #expect(persisted.value == foreign && calls.value == 1)
        #expect(store.draft.start == 1320 && store.draft.enabled)
        #expect(store.conflict && !store.canSave && store.error != nil)
        await store.send(.save).finish()
        #expect(calls.value == 1)
        await store.send(.useCurrent).finish()
        #expect(store.draft == foreign && !store.conflict && !store.editing)
        started.continuation.finish(); release.continuation.finish()
    }
}
