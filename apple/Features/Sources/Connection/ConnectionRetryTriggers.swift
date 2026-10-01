import SwiftUI

public extension View {
    /// Calls `retry` when the network comes back and when the app returns to the foreground, so
    /// nobody waits out a backoff that no longer applies.
    func retriesConnection(networkChanges: @escaping @Sendable () -> AsyncStream<Bool>,
                           retry: @escaping @MainActor () -> Void) -> some View {
        modifier(ConnectionRetryTriggers(networkChanges: networkChanges, retry: retry))
    }
}

private struct ConnectionRetryTriggers: ViewModifier {
    let networkChanges: @Sendable () -> AsyncStream<Bool>
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
            .onChange(of: phase) { old, new in
                if new == .active && old != .active { retry() }
            }
    }
}
