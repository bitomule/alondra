import AlondraTransport
import Foundation
import Network

/// A Mac advertising Alondra on this network. Its TXT record is only a hint: identity is the key
/// `hello` returns, checked at pairing and on every route.
public struct DiscoveredMac: Sendable, Hashable, Identifiable {
    public let id: String
    public let name: String
    public let macID: String?
    let endpoint: NWEndpoint

    public init(id: String, name: String, macID: String?, endpoint: NWEndpoint) {
        self.id = id
        self.name = name
        self.macID = macID
        self.endpoint = endpoint
    }
}

/// Bonjour browsing for `_alondra._tcp` through Network, which asks for Local Network access.
public enum MacBrowser {
    /// The sandbox Macs of `make e2e-sandbox` advertise their own type, so a test build never
    /// lists the real Mac.
    public static var serviceType: String {
        ProcessInfo.processInfo.environment["ALONDRA_SANDBOX"] == "1" ? "_alondra-sandbox._tcp" : "_alondra._tcp"
    }

    public static func browse(type: String = serviceType) -> AsyncStream<[DiscoveredMac]> {
        AsyncStream { continuation in
            let browser = NWBrowser(for: .bonjourWithTXTRecord(type: type, domain: nil), using: .tcp)
            browser.browseResultsChangedHandler = { results, _ in
                let macs = results.compactMap { result -> DiscoveredMac? in
                    guard case let .service(service, _, _, _) = result.endpoint else { return nil }
                    var txt: [String: String] = [:]
                    if case let .bonjour(record) = result.metadata { txt = record.dictionary }
                    return DiscoveredMac(id: service, name: txt["name"].flatMap { $0.isEmpty ? nil : $0 } ?? service,
                                         macID: txt["mac_id"], endpoint: result.endpoint)
                }
                continuation.yield(macs.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending })
            }
            browser.stateUpdateHandler = { state in
                if case .failed = state { continuation.finish() }
            }
            continuation.onTermination = { _ in browser.cancel() }
            browser.start(queue: .global(qos: .userInitiated))
        }
    }

    /// Network cannot resolve a service without connecting, so a short TCP connection gives the
    /// address and port. IPv4 keeps link-local zones out of the URL.
    public static func resolve(_ mac: DiscoveredMac, timeout: Duration = .seconds(5)) async throws -> DirectEndpoint {
        let parameters = NWParameters.tcp
        if let ip = parameters.defaultProtocolStack.internetProtocol as? NWProtocolIP.Options { ip.version = .v4 }
        let connection = NWConnection(to: mac.endpoint, using: parameters)
        let once = ResolveOnce()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                once.set(continuation)
                connection.stateUpdateHandler = { state in
                    switch state {
                    case .ready:
                        if case let .hostPort(.ipv4(address), port) = connection.currentPath?.remoteEndpoint {
                            // The description carries the interface zone ("…%en0"); the bytes do not.
                            let host = address.rawValue.map(String.init).joined(separator: ".")
                            once.resume(.success(DirectEndpoint(host: host, port: port.rawValue, kind: .localNetwork)))
                        } else {
                            once.resume(.failure(PairingError.macUnreachable))
                        }
                        connection.cancel()
                    case .failed, .waiting:
                        once.resume(.failure(PairingError.macUnreachable))
                        connection.cancel()
                    default: break
                    }
                }
                connection.start(queue: .global(qos: .userInitiated))
                Task {
                    try? await Task.sleep(for: timeout)
                    once.resume(.failure(PairingError.macUnreachable))
                    connection.cancel()
                }
            }
        } onCancel: {
            once.resume(.failure(CancellationError()))
            connection.cancel()
        }
    }
}

/// Network calls back on its own queue; the first outcome wins.
private final class ResolveOnce: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<DirectEndpoint, any Error>?
    private var pending: Result<DirectEndpoint, any Error>?

    func set(_ continuation: CheckedContinuation<DirectEndpoint, any Error>) {
        lock.lock()
        if let pending {
            lock.unlock()
            continuation.resume(with: pending)
            return
        }
        self.continuation = continuation
        lock.unlock()
    }

    func resume(_ result: Result<DirectEndpoint, any Error>) {
        lock.lock()
        guard let continuation else {
            if pending == nil { pending = result }
            lock.unlock()
            return
        }
        self.continuation = nil
        pending = result
        lock.unlock()
        continuation.resume(with: result)
    }
}
