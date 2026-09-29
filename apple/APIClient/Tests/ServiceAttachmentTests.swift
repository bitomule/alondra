import AlondraAPIClient
import AlondraContracts
import CryptoKit
import Foundation
import XCTest

final class ServiceAttachmentTests: XCTestCase {
    func testLostFinishReceiptResumesSameTransferWithoutFinalizingTwice() async throws {
        let data = Data((0..<150_000).map { UInt8($0 % 251) })
        let transport = AttachmentServiceFixture()
        let api = AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "client-test", operations: transport)
        let id = UUID()
        do {
            _ = try await api.uploadDomainAttachment(data, name: "a b&c.txt", transferID: id)
            XCTFail("Lost finish receipt must not report success")
        } catch { XCTAssertEqual(error as? ServiceAPIError, .outcomeUnknown) }
        let stored = try await api.uploadDomainAttachment(data, name: "a b&c.txt", transferID: id)
        XCTAssertEqual(stored.id, "stored-evidence")
        XCTAssertEqual(stored.contentDigest, SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined())
        let result = await transport.result()
        XCTAssertEqual(result.bytes, data)
        XCTAssertEqual(result.name, "a b&c.txt")
        XCTAssertEqual(result.finishCount, 1)
        XCTAssertEqual(result.maxChunk, 65_536)
        XCTAssertEqual(result.transferIDs, [id.uuidString.lowercased()])
    }

    func testAttachmentContentPinsVersionAndChecksTheDigest() async throws {
        let transport = DownloadFixture()
        let api = AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "client-test", operations: transport)
        let bytes = Data(repeating: 1, count: 131_072)
        let ref = DomainV2.AttachmentRef(id: "file", name: "test.bin",
            contentDigest: SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined(),
            contentReference: "files/file", bytes: 131_072)
        do {
            _ = try await api.domainAttachmentContent(ref)
            XCTFail("A changed version must not produce a mixed file")
        } catch { XCTAssertEqual((error as? DomainV2.DomainError)?.code, "content_unavailable") }
        let paths = await transport.recorded().map(\.path)
        XCTAssertEqual(paths, ["/v2/files/chunk?path=files%2Ffile&offset=0",
                               "/v2/files/chunk?path=files%2Ffile&offset=65536&version=version-one"])
    }
}

private actor AttachmentServiceFixture: ServiceOperationTransport {
    var bytes = Data()
    var name: String?
    var maxChunk = 0
    var finishCount = 0
    var transfers: Set<String> = []
    var cache: [String: ServiceResponse] = [:]
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        if let cached = cache[request.operationID] { return cached }
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: request.body ?? Data()) as? [String: Any])
        transfers.insert(try XCTUnwrap(object["transfer_id"] as? String))
        let response: [String: Any]
        switch request.path {
        case "/v2/domain/uploads/start":
            name = try XCTUnwrap(object["name"] as? String)
            response = ["offset": bytes.count, "chunk_bytes": 65_536, "completed": false]
        case "/v2/domain/uploads/chunk":
            let encoded = try XCTUnwrap(object["data"] as? String)
            XCTAssertFalse(encoded.contains("="))
            let base = encoded.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            let chunk = try XCTUnwrap(Data(base64Encoded: base + String(repeating: "=", count: (4 - base.count % 4) % 4)))
            XCTAssertEqual(object["offset"] as? Int, bytes.count)
            maxChunk = max(maxChunk, chunk.count)
            bytes.append(chunk)
            response = ["offset": bytes.count]
        case "/v2/domain/uploads/finish":
            finishCount += 1
            let digest = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
            let stored: [String: Any] = ["id": "stored-evidence", "name": name ?? "", "content_digest": digest,
                                         "content_reference": "files/stored-evidence", "bytes": bytes.count]
            cache[request.operationID] = .completed(status: 200, body: try JSONSerialization.data(withJSONObject: ["attachment": stored]))
            return .unknown
        default: XCTFail("Unexpected path"); return .unknown
        }
        let result = ServiceResponse.completed(status: 200, body: try JSONSerialization.data(withJSONObject: response))
        cache[request.operationID] = result
        return result
    }
    func result() -> (bytes: Data, name: String?, finishCount: Int, maxChunk: Int, transferIDs: Set<String>) { (bytes, name, finishCount, maxChunk, transfers) }
}

private actor DownloadFixture: ServiceOperationTransport {
    var requests: [ServiceRequest] = []
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        requests.append(request)
        let offset = requests.count == 1 ? 0 : 65_536
        let bytes = Data(repeating: 1, count: 65_536)
        let data = bytes.base64EncodedString().replacingOccurrences(of: "=", with: "")
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
        let body: [String: Any] = ["content_digest": SHA256.hash(data: Data(repeating: 1, count: 131_072)).map { String(format: "%02x", $0) }.joined(),
                                   "version": requests.count == 1 ? "version-one" : "version-two", "data": data,
                                   "offset": offset, "total": 131_072, "next_offset": offset + bytes.count, "eof": requests.count == 2]
        return .completed(status: 200, body: try JSONSerialization.data(withJSONObject: body))
    }
    func recorded() -> [ServiceRequest] { requests }
}
