import Foundation
import CryptoKit

/// Infrastructure capability for durable coordinators and pairing services. It does not
/// retain requests, retry mutations, or own app state. Its concrete implementation is internal.
public protocol Transport: RelayRequestTransport {
    func bootstrap() async throws -> RelayBootstrap
    func bootstrapIfNeeded() async throws -> RelayBootstrap
    func health() async throws -> Bool
    func submit(_ envelope: Envelope) async throws -> RelayJob
    func acknowledgeResult(requestID: String, revision: UInt64) async throws
    func status(requestID: String) async throws -> RelayJob
    func waitForRequest(requestID: String, waitSeconds: UInt64) async throws -> RelayReceipt
    func changes(since: UInt64, epoch: String?, waitSeconds: UInt64) async throws -> RelayChanges
    func registerPush(_ registration: RelayPushRegistration) async throws
    func pairingRequest(method: String, target: String, body: Data) async throws -> HTTPReply
    func bindIrohNode(_ binding: Data) async throws
}

public extension Transport {
    func bootstrapIfNeeded() async throws -> RelayBootstrap { try await bootstrap() }
    func acknowledgeResult(requestID: String, revision: UInt64) async throws {}
    func bindIrohNode(_ binding: Data) async throws { throw RelayWireError.invalidEndpoint }
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
