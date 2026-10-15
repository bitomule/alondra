import Synchronization

/// The Mac's iroh endpoint ID as this connection knows it: saved with the pairing, then whatever
/// the Mac's sealed `/routes` says.
final class IrohEndpointHolder: Sendable {
    private let endpoint: Mutex<String?>

    init(_ endpoint: String?) { self.endpoint = Mutex(endpoint) }

    var value: String? { endpoint.withLock { $0 } }

    func learn(_ learned: String) { endpoint.withLock { $0 = learned } }
}
