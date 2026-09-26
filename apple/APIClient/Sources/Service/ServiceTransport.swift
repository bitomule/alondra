import Foundation
import CryptoKit

/// Infrastructure capability for durable coordinators and pairing services. It does not
/// retain requests, retry mutations, or own app state. Its concrete implementation is internal.
public protocol ServiceTransport: RelayRequestTransport {
    func bootstrap() async throws -> RelayBootstrap
    func health() async throws -> Bool
    func submit(_ envelope: EnvelopeV1) async throws -> RelayJob
    func status(requestID: String) async throws -> RelayJob
    func changes(since: UInt64, epoch: String?, waitSeconds: UInt64) async throws -> RelayChanges
    func registerPush(_ registration: RelayPushRegistration) async throws
    func pairingRequest(method: String, target: String, body: Data) async throws -> ServiceHTTPResponse
}

public extension AlondraAPIClient {
    static func makeServiceHTTPTransport() -> any ServiceHTTPTransport { URLSessionServiceTransport() }

    static func makeServiceTransport(base: URL, deviceID: String, signing: P256.Signing.PrivateKey,
        bearer: @escaping @Sendable () async throws -> String,
        transport: (any ServiceHTTPTransport)? = nil,
        now: @escaping @Sendable () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }) throws -> any ServiceTransport {
        try makeServiceTransport(base: base, deviceID: deviceID,
            signer: { try signing.signature(for: $0).rawRepresentation }, bearer: bearer, transport: transport, now: now)
    }

    static func makeServiceTransport(base: URL, deviceID: String,
        signer: @escaping @Sendable (Data) throws -> Data,
        bearer: @escaping @Sendable () async throws -> String,
        transport: (any ServiceHTTPTransport)? = nil,
        now: @escaping @Sendable () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }) throws -> any ServiceTransport {
        try RelayTransport(base: base, deviceID: deviceID, signer: signer, bearer: bearer,
                           transport: transport ?? URLSessionServiceTransport(), now: now)
    }
}
