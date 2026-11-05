import Foundation
import CryptoKit

/// The relay's account services: bootstrap, pairing, push registration, iroh admission and the
/// owner's Macs. Requests to a Mac never go through it. Its concrete implementation is internal.
public protocol Transport: Sendable {
    func bootstrap() async throws -> RelayBootstrap
    func bootstrapIfNeeded() async throws -> RelayBootstrap
    func health() async throws -> Bool
    func registerPush(_ registration: RelayPushRegistration) async throws
    func registerActivity(_ registration: RelayActivityRegistration) async throws
    func unregisterActivity(phoneID: String) async throws
    func pairingRequest(method: String, target: String, body: Data) async throws -> HTTPReply
    func bindIrohNode(_ binding: Data) async throws
    func macs() async throws -> [RelayMac]
}

public extension Transport {
    func bootstrapIfNeeded() async throws -> RelayBootstrap { try await bootstrap() }
    func registerActivity(_ registration: RelayActivityRegistration) async throws { throw RelayWireError.invalidEndpoint }
    func unregisterActivity(phoneID: String) async throws { throw RelayWireError.invalidEndpoint }
    func bindIrohNode(_ binding: Data) async throws { throw RelayWireError.invalidEndpoint }
    func macs() async throws -> [RelayMac] { throw RelayWireError.invalidEndpoint }
}

public extension AlondraAPIClient {
    static func makeHTTPTransport() -> any HTTPTransport { URLSessionTransport() }

    static func makeTransport(base: URL, deviceID: String, signing: P256.Signing.PrivateKey,
        bearer: @escaping @Sendable () async throws -> String,
        bearerRefused: @escaping @Sendable () async -> Void = {},
        bearerRefusedForToken: (@Sendable (String) async -> Void)? = nil,
        transport: (any HTTPTransport)? = nil,
        now: @escaping @Sendable () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }) throws -> any Transport {
        try makeTransport(base: base, deviceID: deviceID,
            signer: { try signing.signature(for: $0).rawRepresentation }, bearer: bearer, bearerRefused: bearerRefused, bearerRefusedForToken: bearerRefusedForToken,
            transport: transport, now: now)
    }

    static func makeTransport(base: URL, deviceID: String,
        signer: @escaping @Sendable (Data) throws -> Data,
        bearer: @escaping @Sendable () async throws -> String,
        bearerRefused: @escaping @Sendable () async -> Void = {},
        bearerRefusedForToken: (@Sendable (String) async -> Void)? = nil,
        transport: (any HTTPTransport)? = nil,
        now: @escaping @Sendable () -> UInt64 = { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }) throws -> any Transport {
        try RelayTransport(base: base, deviceID: deviceID, signer: signer, bearer: bearer, bearerRefused: bearerRefused, bearerRefusedForToken: bearerRefusedForToken,
                           transport: transport ?? URLSessionTransport(), now: now)
    }
}
