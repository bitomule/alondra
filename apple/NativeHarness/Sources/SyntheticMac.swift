@testable import AlondraTransport
import CryptoKit
import Foundation

/// A declared fake Mac on its direct port. Production envelope crypto, the direct router, resource
/// reads and the durable envelope journal all still execute.
actor SyntheticMacHTTP: HTTPTransport {
    struct Observation: Sendable {
        var offsets: [Int] = []
        var resourcePaths: [String] = []
        var opened = 0
        var actionPosts: [Data] = []
        var actionRuns = 0
        var resultReads = 0
        var losses = 0
    }
    static let chunkBytes = 49_152
    static let revisionID = "revision-1"
    static let endpoint = DirectEndpoint(host: "192.168.1.5", port: 47810, kind: .localNetwork)
    private let document: Data
    private let loseResponse: Bool
    private let phoneSign: P256.Signing.PublicKey
    private let phoneAgreement: P256.KeyAgreement.PublicKey
    private let macSign: P256.Signing.PrivateKey
    private let macAgreement: P256.KeyAgreement.PrivateKey
    private var observed = Observation()
    private var answered: [String: (request: Data, digest: String, response: Envelope)] = [:]
    private var firstChunk: (@Sendable () async throws -> Void)?

    init(document: Data, loseResponse: Bool, phoneSign: P256.Signing.PublicKey,
         phoneAgreement: P256.KeyAgreement.PublicKey, macSign: P256.Signing.PrivateKey,
         macAgreement: P256.KeyAgreement.PrivateKey) {
        self.document = document; self.loseResponse = loseResponse
        self.phoneSign = phoneSign; self.phoneAgreement = phoneAgreement
        self.macSign = macSign; self.macAgreement = macAgreement
    }

    nonisolated var identity: DirectMacIdentity {
        DirectMacIdentity(macID: "mac-test", name: "Synthetic Mac",
                          signingKey: Base64URL.encode(macSign.publicKey.x963Representation),
                          agreementKey: Base64URL.encode(macAgreement.publicKey.x963Representation))
    }

    func onFirstChunk(_ action: @escaping @Sendable () async throws -> Void) { firstChunk = action }
    func observation() -> Observation { observed }

    func send(_ request: URLRequest) async throws -> HTTPReply {
        guard let url = request.url, url.host == Self.endpoint.host else { throw URLError(.cannotFindHost) }
        if url.path == "/direct/hello", request.httpMethod == "GET" {
            return HTTPReply(status: 200, body: try JSONEncoder().encode(identity))
        }
        guard url.path == "/direct/request", request.httpMethod == "POST", let body = request.httpBody else {
            throw RelayWireError.invalidField
        }
        let envelope = try JSONDecoder().decode(Envelope.self, from: body)
        let plaintext = try envelope.open(senderKey: phoneSign, recipientKey: macAgreement,
            sender: "phone-test", recipient: "mac-test", route: "mac-test", nowMilliseconds: 1000)
        observed.opened += 1
        struct Operation: Decodable { let path: String; let method: String }
        let operation = try JSONDecoder().decode(Operation.self, from: plaintext)
        if operation.method == "POST", operation.path == "/actions" {
            return try act(envelope, bytes: body)
        }
        return HTTPReply(status: 200, body: try JSONEncoder().encode(seal(await read(operation.method, operation.path), to: envelope.requestID)))
    }

    /// Runs an action once per request ID; the same bytes again get the stored answer.
    private func act(_ envelope: Envelope, bytes: Data) throws -> HTTPReply {
        observed.actionPosts.append(bytes)
        if let stored = answered[envelope.requestID] {
            guard stored.request == bytes else { return HTTPReply(status: 409, body: Data(#"{"error":"idempotency_conflict"}"#.utf8)) }
            return HTTPReply(status: 200, body: try JSONEncoder().encode(stored.response))
        }
        observed.actionRuns += 1
        let response = try seal(["status": 201, "body": ["id": "synthetic-task", "accepted": true]], to: envelope.requestID)
        answered[envelope.requestID] = (bytes, try envelope.recoveryDigest(), response)
        if loseResponse, observed.losses == 0 {
            observed.losses += 1
            throw URLError(.networkConnectionLost)
        }
        return HTTPReply(status: 200, body: try JSONEncoder().encode(response))
    }

    private func read(_ method: String, _ path: String) async throws -> [String: Any] {
        switch (method, path) {
        case ("GET", "/summary"):
            observed.resourcePaths.append(path)
            return ["status": 200, "body": ["protocol_version": 2, "data_epoch": "synthetic", "revision": 1]]
        case ("GET", "/documents/document-1"):
            observed.resourcePaths.append(path)
            return ["status": 200, "body": ["id": "document-1"]]
        case ("GET", let path) where path.hasPrefix("/operation-results/"):
            observed.resultReads += 1
            let components = URLComponents(string: "https://fixture.invalid" + path)
            let id = String(components?.path.dropFirst("/operation-results/".count) ?? "")
            let digest = components?.queryItems?.first(where: { $0.name == "digest" })?.value
            guard let stored = answered[id], stored.digest == digest else { return ["status": 404, "body": ["error": "not_found"]] }
            return ["status": 200, "body": ["request_id": id, "digest": stored.digest, "data_epoch": "synthetic",
                "response": try JSONSerialization.jsonObject(with: JSONEncoder().encode(stored.response))]]
        case ("GET", let path) where path.hasPrefix("/document-revisions/\(Self.revisionID)/content?offset="):
            let components = URLComponents(string: "https://fixture.invalid" + path)
            guard let offset = components?.queryItems?.first(where: { $0.name == "offset" })?.value.flatMap(Int.init),
                  offset >= 0, offset < document.count, offset % Self.chunkBytes == 0 else { throw RelayWireError.invalidField }
            observed.resourcePaths.append(path)
            observed.offsets.append(offset)
            let next = min(offset + Self.chunkBytes, document.count)
            let bytes = document.subdata(in: offset..<next)
            if offset == 0, let callback = firstChunk {
                firstChunk = nil
                try await callback()
            }
            return ["status": 200, "body": ["revision_id": Self.revisionID,
                "content_digest": SHA256.hash(data: document).map { String(format: "%02x", $0) }.joined(),
                "version": "version-1", "offset": offset, "total": document.count, "next_offset": next,
                "eof": next == document.count, "data": bytes.base64EncodedString().replacingOccurrences(of: "+", with: "-")
                    .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")]]
        default:
            throw RelayWireError.invalidField
        }
    }

    private func seal(_ payload: [String: Any], to requestID: String) throws -> Envelope {
        try Envelope.seal(route: "mac-test", sender: "mac-test", recipient: "phone-test",
            requestID: requestID, kind: "response", expiresAtMilliseconds: UInt64(Date().timeIntervalSince1970 * 1000) + 60_000,
            plaintext: JSONSerialization.data(withJSONObject: payload), senderKey: macSign, recipientKey: phoneAgreement)
    }
}
