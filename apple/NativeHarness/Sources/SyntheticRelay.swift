import AlondraTransport
import CryptoKit
import Foundation

/// A declared fake relay + Mac peer. Production request signing, envelope crypto,
/// resource reads and durable outbox all still execute.
actor SyntheticRelayHTTP: HTTPTransport {
    struct Observation: Sendable {
        var offsets: [Int] = []
        var resourcePaths: [String] = []
        var opened = 0
        var mutations = 0
        var statusReads = 0
        var losses = 0
    }
    static let chunkBytes = 49_152
    static let revisionID = "revision-1"
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

    func send(_ request: URLRequest) async throws -> HTTPReply {
        guard let url = request.url, url.host == "relay.fixture.invalid", url.scheme == "https" else {
            throw RelayWireError.invalidEndpoint
        }
        if url.path == "/v1/bootstrap", request.httpMethod == "GET" {
            return HTTPReply(status: 200, body: Data(#"{"protocol":1,"epoch":"AAAAAAAAAAAAAAAAAAAAAA"}"#.utf8))
        }
        guard request.value(forHTTPHeaderField: "Authorization") == "Bearer synthetic-access" else {
            throw RelayWireError.unauthorized
        }
        if url.path.hasPrefix("/v1/requests/"), request.httpMethod == "GET" {
            observed.statusReads += 1
            guard let receipt = receipts[url.lastPathComponent] else { throw RelayWireError.outcomeUnknown }
            return HTTPReply(status: 200, body: receipt)
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
        case ("GET", "/summary"):
            observed.resourcePaths.append(operation.path)
            payload = ["status": 200, "body": ["protocol_version": 2, "data_epoch": "synthetic", "revision": 1]]
        case ("GET", "/documents/document-1"):
            observed.resourcePaths.append(operation.path)
            payload = ["status": 200, "body": ["id": "document-1"]]
        case ("GET", let path) where path.hasPrefix("/document-revisions/\(Self.revisionID)/content?offset="):
            let components = URLComponents(string: "https://fixture.invalid" + path)
            guard let offset = components?.queryItems?.first(where: { $0.name == "offset" })?.value.flatMap(Int.init),
                  offset >= 0, offset < document.count, offset % Self.chunkBytes == 0 else { throw RelayWireError.invalidField }
            observed.resourcePaths.append(operation.path)
            observed.offsets.append(offset)
            let next = min(offset + Self.chunkBytes, document.count)
            let bytes = document.subdata(in: offset..<next)
            payload = ["status": 200, "body": ["revision_id": Self.revisionID,
                "content_digest": SHA256.hash(data: document).map { String(format: "%02x", $0) }.joined(),
                "version": "version-1", "offset": offset, "total": document.count, "next_offset": next,
                "eof": next == document.count, "data": bytes.base64EncodedString().replacingOccurrences(of: "+", with: "-")
                    .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")]]
            if offset == 0, let callback = firstChunk {
                firstChunk = nil
                try await callback()
            }
        case ("POST", "/requests"):
            observed.mutations += 1
            payload = ["status": 201, "body": ["id": "synthetic-task", "accepted": true]]
        default:
            throw RelayWireError.invalidField
        }
        let response = try EnvelopeV1.seal(route: "mac-test", sender: "mac-test", recipient: "phone-test",
            requestID: envelope.requestID, kind: "response", expiresAtMilliseconds: 5000,
            plaintext: JSONSerialization.data(withJSONObject: payload), senderKey: macSign, recipientKey: phoneAgreement)
        let receipt = try JSONSerialization.data(withJSONObject: ["request_id": envelope.requestID,
            "status": "completed", "revision": observed.opened,
            "response": JSONSerialization.jsonObject(with: JSONEncoder().encode(response))])
        receipts[envelope.requestID] = receipt
        if loseResponse, operation.method == "POST", operation.path == "/requests" {
            observed.losses += 1
            throw URLError(.networkConnectionLost)
        }
        return HTTPReply(status: 200, body: receipt)
    }
}
