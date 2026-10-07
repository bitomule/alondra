import AlondraFeatures
import AlondraTransport
import Foundation

extension Connection {
    private struct Pending: Decodable {
        struct Request: Decodable { let id: String; let name: String; let code: String }
        let pending: [Request]
    }

    private struct Devices: Decodable {
        struct Device: Decodable { let id: String; let name: String; let origin: String? }
        let devices: [Device]
    }

    /// Devices waiting for someone on this Mac to allow them. Loopback only.
    public func pendingPairingRequests() async throws -> [PairingRequest] {
        try JSONDecoder().decode(Pending.self, from: try await local(.pendingDevices)).pending
            .map { PairingRequest(id: $0.id, name: $0.name, code: $0.code) }
    }

    public func decidePairing(_ id: String, allow: Bool) async throws {
        _ = try await local(allow ? .approvePending(id) : .denyPending(id))
    }

    public func trustedDevices() async throws -> [TrustedDevice] {
        try JSONDecoder().decode(Devices.self, from: try await local(.devices)).devices
            .map { TrustedDevice(id: $0.id, name: $0.name, origin: $0.origin.flatMap(TrustedDevice.Origin.init)) }
    }

    /// Cuts the device off on every path at once: direct, relay and push.
    public func revokeDevice(_ id: String) async throws {
        _ = try await local(.revokeDevice(id))
    }
}
