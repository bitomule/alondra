import SwiftUI

public extension View {
    /// Calls `retry` when the network comes back, on every `hints` (another network, a push) and
    /// when the app returns to the foreground, so nobody waits out a backoff that no longer applies.
    func retriesConnection(networkChanges: @escaping @Sendable () -> AsyncStream<Bool>,
                           hints: @escaping @Sendable () -> AsyncStream<Void> = { AsyncStream { _ in } },
                           retry: @escaping @MainActor () -> Void) -> some View {
        modifier(ConnectionRetryTriggers(networkChanges: networkChanges, hints: hints, retry: retry))
    }
}

private struct ConnectionRetryTriggers: ViewModifier {
    let networkChanges: @Sendable () -> AsyncStream<Bool>
    let hints: @Sendable () -> AsyncStream<Void>
    let retry: @MainActor () -> Void
    @Environment(\.scenePhase) private var phase

    func body(content: Content) -> some View {
        content
            .task {
                var wasOnline = true
                for await online in networkChanges() {
                    if online && !wasOnline { retry() }
                    wasOnline = online
                }
            }
            .task {
                for await _ in hints() { retry() }
            }
            .onChange(of: phase) { old, new in
                if new == .active && old != .active { retry() }
            }
    }
}
