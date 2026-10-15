import AlondraContract
import AlondraTransport
import CryptoKit
import Foundation

/// One request from this device to a Mac, as measured where it ends: which path answered, how
/// long it took, on which network, and how it ended. Holds nothing that names the person, their
/// network or their work: no ids, addresses, bodies or server text.
public struct ConnectionMeasurement: Codable, Equatable, Sendable {
    public enum Outcome: String, Codable, Sendable {
        /// The Mac answered with a success.
        case ok
        /// The Mac answered with an error status of its own.
        case macError = "mac_error"
        /// The request went out and no answer from the Mac came back.
        case unanswered
        case failed
        case cancelled
    }

    public var at: Date
    /// The first 6 hex characters of the SHA-256 of the Mac's ID, or `this-mac`.
    public var mac: String
    public var operation: String
    public var method: String
    /// The first segment of the request's path, such as `changes` or `views`.
    public var kind: String
    public var route: ConnectionRoute?
    public var ms: Int
    public var network: String
    public var radio: String?
    public var outcome: Outcome
    public var status: Int?
    public var error: String?

    public init(at: Date, mac: String, operation: String, method: String, kind: String, route: ConnectionRoute?, ms: Int,
                network: String, radio: String? = nil, outcome: Outcome, status: Int? = nil, error: String? = nil) {
        self.at = at; self.mac = mac; self.operation = operation; self.method = method; self.kind = kind
        self.route = route; self.ms = ms; self.network = network; self.radio = radio
        self.outcome = outcome; self.status = status; self.error = error
    }

    /// The `kind` of the time from coming to the foreground to a live iroh connection.
    public static let connectKind = "connect"

    /// How long the iroh connection took to open after the app came to the foreground.
    public static func connected(after duration: Duration, route: ConnectionRoute?, at: Date, mac: String,
                                 network: NetworkSnapshot) -> Self {
        let (seconds, attoseconds) = duration.components
        return Self(at: at, mac: mac, operation: connectKind, method: "IROH", kind: connectKind, route: route,
                    ms: Int(seconds * 1000 + attoseconds / 1_000_000_000_000_000), network: network.interface,
                    radio: network.radio, outcome: .ok)
    }

    /// The Mac a measurement belongs to, short enough to tell Macs apart and nothing more.
    public static func macReference(_ macID: String?) -> String {
        guard let macID else { return "this-mac" }
        return SHA256.hash(data: Data(macID.utf8)).map { String(format: "%02x", $0) }.joined().prefix(6).description
    }
}

public extension ConnectionMeasurement {
    init(_ observation: RequestObservation, at: Date, mac: String, network: NetworkSnapshot) {
        let (seconds, attoseconds) = observation.duration.components
        self.init(at: at, mac: mac, operation: observation.operation, method: observation.method, kind: observation.kind,
                  route: observation.route, ms: Int(seconds * 1000 + attoseconds / 1_000_000_000_000_000),
                  network: network.interface, radio: network.radio, outcome: observation.outcome,
                  status: observation.status, error: observation.error)
    }

    /// The network a request ran on: `wifi`, `cellular`, `wired`, `other` or `offline`, and on
    /// iPhone the radio technology such as `NR` or `LTE`.
    struct NetworkSnapshot: Equatable, Sendable {
        public var interface: String
        public var radio: String?
        public init(interface: String, radio: String? = nil) {
            self.interface = interface
            self.radio = radio
        }
    }
}
