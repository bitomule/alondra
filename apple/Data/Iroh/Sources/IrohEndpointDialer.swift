import AlondraTransport
import Foundation
import IrohLib
import Synchronization

/// This device's iroh endpoint for one paired Mac. It binds once, when the pairing's connection
/// is made, so coming back to the foreground only has to connect.
public final class IrohEndpointDialer: IrohDialer {
    public let endpointID: Data
    private let secretKey: Data
    private let binding: Mutex<Task<IrohLib.Endpoint, any Error>>
    private let relays = Mutex<Set<String>>([])

    static let alpn = Data("alondra/1".utf8)
    /// iroh-relay's QUIC address discovery port.
    static let relayQuicPort: UInt16 = 7842

    public init(identity: IrohIdentity) {
        endpointID = identity.endpointID
        secretKey = identity.secretKey
        binding = Mutex(Self.bind(identity.secretKey))
    }

    private static func bind(_ secretKey: Data) -> Task<IrohLib.Endpoint, any Error> {
        Task {
            try await IrohLib.Endpoint.bind(options: EndpointOptions(preset: presetMinimal(), secretKey: secretKey,
                relayMode: RelayMode.custom(map: RelayMap.empty())))
        }
    }

    public func dial(_ mac: IrohMacAddress) async throws -> any IrohLink {
        let endpoint = try await binding.withLock { $0 }.value
        let relay = mac.relay?.absoluteString
        if let relay, relays.withLock({ $0.insert(relay).inserted }) {
            try await endpoint.insertRelay(config: RelayConfig(url: relay, quicPort: Self.relayQuicPort))
        }
        let address = EndpointAddr(id: try EndpointId.fromString(s: mac.endpointID), relayUrl: relay,
                                   addresses: mac.directAddresses)
        return IrohConnectionLink(try await endpoint.connect(addr: address, alpn: Self.alpn))
    }

    /// iroh 1.0's bindings have no way to tell an endpoint the network changed, so a new one
    /// binds on the new network and the old one closes.
    public func networkMoved() async {
        let fresh = Self.bind(secretKey)
        let old = binding.withLock { current in
            defer { current = fresh }
            return current
        }
        relays.withLock { $0.removeAll() }
        guard let endpoint = try? await old.value else { return }
        try? await endpoint.close()
    }

    public func shutdown() async {
        let current = binding.withLock { $0 }
        current.cancel()
        guard let endpoint = try? await current.value else { return }
        try? await endpoint.close()
    }
}
