import AlondraContract
@testable import AlondraTransport
@testable import AlondraFeed
import CryptoKit
import Foundation
import XCTest

final class StoredAttachmentTests: XCTestCase {
    /// A card's attachment and an image a document cites name their copy relative to the daemon's
    /// HOME (`files/…`); the app read none of them because it only accepted `/files/…`.
    func testAnAttachmentIsReadInChunksByItsReferenceUnderTheDaemonHome() async throws {
        let bytes = Data("png".utf8)
        let digest = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        let reference = "files/images/\(digest).png"
        let chunk = try JSONSerialization.data(withJSONObject: [
            "content_digest": digest, "version": "1", "offset": 0, "total": bytes.count, "next_offset": bytes.count, "eof": true,
            "data": bytes.base64EncodedString().replacingOccurrences(of: "=", with: "")
                .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_"),
        ])
        let operations = RecordingOperations(answer: .completed(status: 200, body: chunk))
        let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "phone-1", operations: operations)
        let attachment = Attachment(id: digest, name: "\(digest).png", kind: .image, size: 3, fetch: reference, digest: digest)

        let data = try await api.fetchAttachment(attachment)

        XCTAssertEqual(data, bytes)
        let paths = await operations.paths
        XCTAssertEqual(paths.count, 1)
        let query = URLComponents(string: paths[0])?.queryItems ?? []
        XCTAssertEqual(URLComponents(string: paths[0])?.path, "/files/chunk")
        XCTAssertEqual(query.first { $0.name == "path" }?.value, reference)
    }

    func testAReferenceThatLeavesTheFilesFolderIsNotRequested() async {
        let operations = RecordingOperations(answer: .completed(status: 200, body: Data()))
        let api = AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "phone-1", operations: operations)
        for fetch in ["files/../secrets", "/etc/hosts", "https://x.dev/a.png"] {
            let attachment = Attachment(id: "a", name: "a", kind: .image, size: 1, fetch: fetch, digest: String(repeating: "a", count: 64))
            do {
                _ = try await api.fetchAttachment(attachment)
                XCTFail("\(fetch) was requested")
            } catch {}
        }
        let paths = await operations.paths
        XCTAssertEqual(paths, [])
    }
}

private actor RecordingOperations: OperationAPI {
    let answer: APIResponse
    private(set) var paths: [String] = []
    init(answer: APIResponse) { self.answer = answer }

    func perform(_ request: APIRequest) async throws -> APIResponse {
        paths.append(request.path)
        return answer
    }
    func recover(_ request: APIRequest) async throws -> APIResponse { try await perform(request) }
}
