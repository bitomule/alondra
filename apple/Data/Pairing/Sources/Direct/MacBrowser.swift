import AlondraTransport
import Foundation
import Network
import os

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

    public enum Update: Sendable, Equatable {
        case found([DiscoveredMac])
        /// The system refuses the local network. While its permission alert is on screen it says
        /// the same, so the caller decides whether this is the person's answer.
        case denied
        /// The refusal ended: the person allowed Local Network access, Macs or not.
        case allowed
    }

    public static func browse(type: String = serviceType, access: LocalNetworkAccess = .standard) -> AsyncStream<Update> {
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
                if !macs.isEmpty { access.record(.allowed) }
                continuation.yield(.found(macs.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }))
            }
            let refused = OSAllocatedUnfairLock(initialState: false)
            let denials = OSAllocatedUnfairLock(initialState: 0)
            browser.stateUpdateHandler = { state in
                switch state {
                case .waiting(let error), .failed(let error):
                    if error == .dns(DNSServiceErrorType(kDNSServiceErr_PolicyDenied)) {
                        refused.withLock { $0 = true }
                        denials.withLock { $0 += 1 }
                        continuation.yield(.denied)
                    }
                    if case .failed = state { continuation.finish() }
                case .ready:
                    guard leavesDenied(refusedDuringBrowse: refused.withLock({ $0 }), saved: access.state)
                    else { return }
                    Task {
                        guard await confirmsAllowed(denialCount: { denials.withLock { $0 } }) else { return }
                        refused.withLock { $0 = false }
                        access.record(.allowed)
                        continuation.yield(.allowed)
                    }
                default: break
                }
            }
            continuation.onTermination = { _ in browser.cancel() }
            browser.start(queue: .global(qos: .userInitiated))
        }
    }

    /// A browser that gets ready proves the system lets this app use the local network. That ends a
    /// refusal seen in this browse, and one saved by an earlier run: access may have been granted
    /// in Settings while the app was closed.
    static func leavesDenied(refusedDuringBrowse: Bool, saved: LocalNetworkAccess.State) -> Bool {
        refusedDuringBrowse || saved == .denied
    }

    /// The browser reports ready before the system's refusal arrives, so ready alone is not an answer:
    /// access counts as allowed only once the browser has stayed ready, with no refusal, for `delay`.
    static func confirmsAllowed(delay: Duration = .seconds(1.5), denialCount: @Sendable () -> Int,
                                sleep: @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }) async -> Bool {
        let before = denialCount()
        do { try await sleep(delay) } catch { return false }
        return denialCount() == before
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
