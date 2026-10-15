import AlondraContract
import Foundation

/// Where a Mac's iroh endpoint can be reached: its public key, the iroh relay it sits on, and
/// any addresses known to answer directly.
public struct IrohMacAddress: Sendable, Equatable {
    public let endpointID: String
    public let relay: URL?
    public let directAddresses: [String]

    public init?(endpointID: String, relay: URL?, directAddresses: [String] = []) {
        guard IrohIdentity.validEndpointID(endpointID), relay != nil || !directAddresses.isEmpty else { return nil }
        self.endpointID = endpointID
        self.relay = relay
        self.directAddresses = directAddresses
    }
}

/// The path iroh picked for a connection.
public enum IrohPath: Sendable, Equatable {
    case direct
    case relay
    case none

    var route: ConnectionRoute? {
        switch self {
        case .direct: .direct
        case .relay: .irohRelay
        case .none: nil
        }
    }
}

public enum IrohLinkError: Error, Sendable, Equatable {
    /// The request never finished leaving this device, so the Mac cannot have acted on it.
    case notSent
    /// The request left whole but no answer came back.
    case noAnswer
}

/// One live QUIC connection to a Mac's iroh endpoint.
public protocol IrohLink: AnyObject, Sendable {
    /// Opens a stream, writes `request`, finishes it, and reads the answer to its end.
    func exchange(_ request: Data, answerLimit: Int) async throws -> Data
    func path() -> IrohPath
    /// Every change of the selected path, until the connection closes.
    func pathChanges() -> AsyncStream<IrohPath>
    var isClosed: Bool { get }
    func close() async
}

/// This device's iroh endpoint for one paired Mac.
public protocol IrohDialer: Sendable {
    /// The endpoint's public key, 32 raw bytes.
    var endpointID: Data { get }
    func dial(_ mac: IrohMacAddress) async throws -> any IrohLink
    /// The device moved to another network: start over with fresh sockets and a fresh relay
    /// connection, so iroh looks for paths on the network the device is on now.
    func networkMoved() async
    func shutdown() async
}
