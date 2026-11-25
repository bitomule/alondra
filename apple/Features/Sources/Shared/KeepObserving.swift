import AlondraContract
import AlondraFeed
import ComposableArchitecture
import Foundation

extension ViewsClient {
    /// Observes one view for as long as the calling effect lives. When the observation fails or ends
    /// (the connection was not ready yet, or dropped), it waits for the app to retry the connection,
    /// for the Mac to answer again, or for a backoff that grows with each failure in a row, and
    /// observes again, so a screen that keeps no state of its own never stays empty for good.
    /// A key the Mac no longer serves ends it after `onMissing`.
    func keepObserving(
        _ key: ViewKey,
        onMissing: @Sendable () async -> Void = {},
        onDocument: @Sendable (ViewDocument) async -> Void
    ) async {
        await run(key, onMissing: onMissing) { await onDocument($0); return false }
    }

    /// Like `keepObserving`, for a watcher that is done once `finished` says a document is the one
    /// it waited for: that ends the observation and releases the key.
    func watch(
        _ key: ViewKey,
        onMissing: @Sendable () async -> Void = {},
        until finished: @Sendable (ViewDocument) async -> Bool
    ) async {
        await run(key, onMissing: onMissing, onDocument: finished)
    }

    private func run(
        _ key: ViewKey,
        onMissing: @Sendable () async -> Void,
        onDocument: @Sendable (ViewDocument) async -> Bool
    ) async {
        @Dependency(\.continuousClock) var clock
        @Dependency(\.withRandomNumberGenerator) var withRandomNumberGenerator
        var failures = 0
        while !Task.isCancelled {
            do {
                for try await fetch in observe(key) {
                    if case .document(let document) = fetch {
                        failures = 0
                        if await onDocument(document) { return }
                    }
                }
            } catch ViewsClientError.missingView {
                return await onMissing()
            } catch {}
            guard !Task.isCancelled else { return }
            let random = withRandomNumberGenerator { Double.random(in: 0...1, using: &$0) }
            await waitToRetry(after: ConnectionBackoff.standard.delay(attempt: failures, random: random), clock: clock)
            failures += 1
        }
    }

    /// Returns when the app retries the connection, when the Mac starts answering again, or after
    /// `delay`, whichever comes first.
    func waitToRetry(after delay: Duration, clock: any Clock<Duration>) async {
        let reconnections = await reconnections()
        let connection = await connection()
        await withTaskGroup(of: Void.self) { group in
            group.addTask { try? await clock.sleep(for: delay) }
            group.addTask { for await _ in reconnections { return } }
            group.addTask {
                var answering = true
                for await state in connection {
                    if state.isConnected && !answering { return }
                    answering = state.isConnected
                }
                try? await clock.sleep(for: delay)
            }
            await group.next()
            group.cancelAll()
        }
    }
}
