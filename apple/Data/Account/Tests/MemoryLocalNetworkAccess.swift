import AlondraPairing
import Synchronization

/// One test's own Local Network answer, so tests running at the same time never share one.
final class MemoryLocalNetworkAccess: Sendable {
    private let saved: Mutex<LocalNetworkAccess.State?>
    init(_ state: LocalNetworkAccess.State? = nil) { saved = Mutex(state) }

    var access: LocalNetworkAccess {
        LocalNetworkAccess(load: { self.saved.withLock { $0 } }, save: { state in self.saved.withLock { $0 = state } })
    }
}
