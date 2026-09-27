import AlondraAPIClient
import CryptoKit
import Foundation

/// A declared fake relay + Mac peer. Production request signing, envelope crypto,
/// response verification, snapshot assembly and durable outbox all still execute.
actor SyntheticRelayHTTP: ServiceHTTPTransport {
    struct Observation: Sendable {
        var offsets: [Int] = []
        var deletes = 0
        var opened = 0
        var mutations = 0
        var statusReads = 0
        var losses = 0
    }
    private static let root = "/v1/service/overview-snapshot"
    private static let snapshotID = String(repeating: "a", count: 32)
    private let document: Data
    private let loseResponse: Bool
    private let phoneSign: P256.Signing.PublicKey
    private let phoneAgreement: P256.KeyAgreement.PublicKey
    private let macSign: P256.Signing.PrivateKey
    private let macAgreement: P256.KeyAgreement.PrivateKey
    private var observed = Observation()
    private var receipts: [String: Data] = [:]
    private var firstChunk: (@Sendable () async throws -> Void)?

    init(document: Data, loseResponse: Bool, phoneSign: P256.Signing.PublicKey,
         phoneAgreement: P256.KeyAgreement.PublicKey, macSign: P256.Signing.PrivateKey,
         macAgreement: P256.KeyAgreement.PrivateKey) {
        self.document = document; self.loseResponse = loseResponse
        self.phoneSign = phoneSign; self.phoneAgreement = phoneAgreement
        self.macSign = macSign; self.macAgreement = macAgreement
    }
    func onFirstChunk(_ action: @escaping @Sendable () async throws -> Void) { firstChunk = action }
    func observation() -> Observation { observed }

    func send(_ request: URLRequest) async throws -> ServiceHTTPResponse {
        guard let url = request.url, url.host == "relay.fixture.invalid", url.scheme == "https" else {
            throw RelayWireError.invalidEndpoint
        }
        if url.path == "/v1/bootstrap", request.httpMethod == "GET" {
            return ServiceHTTPResponse(status: 200, body: Data(#"{"protocol":1,"epoch":"AAAAAAAAAAAAAAAAAAAAAA"}"#.utf8))
        }
        guard request.value(forHTTPHeaderField: "Authorization") == "Bearer synthetic-access" else {
            throw RelayWireError.unauthorized
        }
        if url.path.hasPrefix("/v1/requests/"), request.httpMethod == "GET" {
            observed.statusReads += 1
            guard let receipt = receipts[url.lastPathComponent] else { throw RelayWireError.outcomeUnknown }
            return ServiceHTTPResponse(status: 200, body: receipt)
        }
        guard url.path == "/v1/requests", request.httpMethod == "POST", let data = request.httpBody else {
            throw RelayWireError.invalidField
        }
        let envelope = try JSONDecoder().decode(EnvelopeV1.self, from: data)
        let plaintext = try envelope.open(senderKey: phoneSign, recipientKey: macAgreement,
            sender: "phone-test", recipient: "mac-test", route: "mac-test", nowMilliseconds: 1000)
        struct Operation: Decodable { let path: String; let method: String }
        let operation = try JSONDecoder().decode(Operation.self, from: plaintext)
        observed.opened += 1
        let payload: [String: Any]
        switch (operation.method, operation.path) {
        case ("GET", "/v1/overview"):
            payload = ["status": 502, "error": "response_too_large"]
        case ("POST", "/v1/tasks"):
            observed.mutations += 1
            payload = ["status": 201, "body": ["id": "synthetic-task", "accepted": true]]
        case ("DELETE", Self.root + "/" + Self.snapshotID):
            observed.deletes += 1
            payload = ["status": 200, "body": ["released": true]]
        default:
            guard operation.method == "GET" else { throw RelayWireError.invalidField }
            let offset: Int
            if operation.path == Self.root {
                offset = 0
            } else {
                let prefix = Self.root + "/" + Self.snapshotID + "?offset="
                guard operation.path.hasPrefix(prefix), let value = Int(operation.path.dropFirst(prefix.count)),
                      value > 0, value < document.count, value % 65_536 == 0 else { throw RelayWireError.invalidField }
                offset = value
            }
            observed.offsets.append(offset)
            let chunk = document.subdata(in: offset..<min(offset + 65_536, document.count))
            payload = ["status": 200, "body": ["schema": 1, "snapshot_id": Self.snapshotID,
                "total_bytes": document.count, "sha256": SHA256.hash(data: document).map { String(format: "%02x", $0) }.joined(),
                "chunk_bytes": 65_536, "offset": offset, "data": chunk.base64EncodedString().replacingOccurrences(of: "+", with: "-")
                    .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")]]
            if offset == 0, let callback = firstChunk {
                firstChunk = nil
                try await callback()
            }
        }
        let response = try EnvelopeV1.seal(route: "mac-test", sender: "mac-test", recipient: "phone-test",
            requestID: envelope.requestID, kind: "response", expiresAtMilliseconds: 5000,
            plaintext: JSONSerialization.data(withJSONObject: payload), senderKey: macSign, recipientKey: phoneAgreement)
        let receipt = try JSONSerialization.data(withJSONObject: ["request_id": envelope.requestID,
            "status": "completed", "revision": observed.opened,
            "response": JSONSerialization.jsonObject(with: JSONEncoder().encode(response))])
        receipts[envelope.requestID] = receipt
        if loseResponse, operation.method == "POST", operation.path == "/v1/tasks" {
            observed.losses += 1
            throw URLError(.networkConnectionLost)
        }
        return ServiceHTTPResponse(status: 200, body: receipt)
    }
}
