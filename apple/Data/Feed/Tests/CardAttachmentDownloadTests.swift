import AlondraContract
import AlondraTestSupport
@testable import AlondraTransport
@testable import AlondraFeed
import CryptoKit
import Foundation
import Synchronization
import Testing

@Suite(.temporaryFiles)
struct CardAttachmentDownloadTests {
    /// A card attachment is served by the interaction that registered it, in blocks the app joins,
    /// writes and checks against the card's digest, over a route every connection carries.
    @Test func aCardAttachmentDownloadsInBlocksByItsInteractionRouteAndIsVerified() async throws {
        let bytes = Data((0..<150_000).map { UInt8($0 % 251) })
        let digest = Self.digest(bytes)
        let answers = [0..<65_536, 65_536..<131_072, 131_072..<150_000].map { range in
            Self.chunk(bytes, range: range, digest: digest, interaction: "i-1", attachment: "clip: ñ 1")
        }
        let operations = ScriptedOperations(answers: answers)
        let attachment = Attachment(id: "clip: ñ 1", name: "clip.mp4", kind: .video, size: UInt64(bytes.count),
                                    fetch: "/interactions/i-1/attachments/clip%3A%20%C3%B1%201/content", digest: digest)
        let file = try TestFiles.directory().appendingPathComponent("clip.mp4")
        let seen = Mutex<[UInt64]>([])

        try await Self.api(operations).downloadAttachment(attachment, to: file) { done, _ in seen.withLock { $0.append(done) } }

        #expect(try Data(contentsOf: file) == bytes)
        #expect(seen.withLock { $0 } == [65_536, 131_072, 150_000])
        let paths = await operations.paths
        #expect(paths == [
            "/interactions/i-1/attachments/clip%3A%20%C3%B1%201/content?offset=0",
            "/interactions/i-1/attachments/clip%3A%20%C3%B1%201/content?offset=65536&version=v1",
            "/interactions/i-1/attachments/clip%3A%20%C3%B1%201/content?offset=131072&version=v1",
        ])
        for path in paths {
            #expect(EnvelopeRequestPolicy.validClientContractPath(method: "GET", path: path), "the relay refuses \(path)")
        }
        #expect(try await Self.api(ScriptedOperations(answers: answers)).fetchAttachment(attachment) == bytes)
    }

    @Test(arguments: ["content that does not hash to the digest", "a block of another interaction", "a version that changes mid-download"])
    func aBlockThatDoesNotMatchTheCardIsRefused(_ name: String) async throws {
        let bytes = Data(repeating: 7, count: 100)
        let digest = Self.digest(bytes)
        let attachment = Attachment(id: "a", name: "a.png", kind: .image, size: 100,
                                    fetch: "/interactions/i-1/attachments/a/content", digest: digest)
        let answers: [Data] = switch name {
        case "content that does not hash to the digest":
            [Self.chunk(Data(repeating: 8, count: 100), range: 0..<100, digest: digest, interaction: "i-1", attachment: "a")]
        case "a block of another interaction":
            [Self.chunk(bytes, range: 0..<100, digest: digest, interaction: "i-2", attachment: "a")]
        default:
            [Self.chunk(bytes, range: 0..<50, digest: digest, interaction: "i-1", attachment: "a"),
             Self.chunk(bytes, range: 50..<100, digest: digest, interaction: "i-1", attachment: "a", version: "v2")]
        }
        let error = await #expect(throws: FeedError.invalidContent) {
            try await Self.api(ScriptedOperations(answers: answers)).fetchAttachment(attachment)
        }
        #expect(error.map(FeedError.attachmentIsGone) == true)
    }

    @Test func anAttachmentRouteThatIsNotTheDaemonsIsNeverRequested() async {
        let operations = ScriptedOperations(answers: [])
        let digest = String(repeating: "a", count: 64)
        let routes: [(id: String, fetch: String)] = [
            ("a", "/interactions/i-1/attachments/a/content?offset=9"),
            ("a", "/interactions/i-1/attachments/b/content"),
            ("..", "/interactions/i-1/attachments/../content"),
            ("a/b", "/interactions/i-1/attachments/a/b/content"),
            ("a b", "/interactions/i-1/attachments/a b/content"),
            ("a", "/interactions/i-1/attachments/a/raw"),
            ("a", "https://x.dev/interactions/i-1/attachments/a/content"),
        ]
        for route in routes {
            let attachment = Attachment(id: route.id, name: "a", kind: .binary, size: 1, fetch: route.fetch, digest: digest)
            await #expect(throws: (any Error).self, "\(route.fetch) was requested") {
                try await Self.api(operations).fetchAttachment(attachment)
            }
        }
        #expect(await operations.paths == [])
        #expect(!APIRequest.validClientContractRoute(method: .post, path: ["interactions", "i-1", "attachments", "a", "content"]))
        #expect(!EnvelopeRequestPolicy.validClientContractPath(method: "GET", path: "/interactions/i-1/attachments/a/content?offset=999999999"))
        #expect(!EnvelopeRequestPolicy.validClientContractPath(method: "GET", path: "/interactions/i-1/attachments/a/content?offset=0&path=x"))
    }

    @Test func onlyAFileTheMacNoLongerHoldsIsGoneForGood() {
        #expect(FeedError.attachmentIsGone(APIError.conflict(kind: "content_unavailable", detail: nil, reasons: [])))
        #expect(FeedError.attachmentIsGone(APIError.server(status: 404, kind: "not_found", detail: nil, reasons: [])))
        #expect(!FeedError.attachmentIsGone(APIError.unavailable))
        #expect(!FeedError.attachmentIsGone(OperationError.outcomeUnknown))
        #expect(!FeedError.attachmentIsGone(URLError(.notConnectedToInternet)))
    }

    private static func api(_ operations: ScriptedOperations) -> AlondraAPIClient {
        AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "phone-1", operations: operations)
    }

    private static func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }

    private static func chunk(_ bytes: Data, range: Range<Int>, digest: String, interaction: String, attachment: String,
                              version: String = "v1") -> Data {
        try! JSONSerialization.data(withJSONObject: [
            "interaction_id": interaction, "attachment_id": attachment, "content_digest": digest, "version": version,
            "offset": range.lowerBound, "total": bytes.count, "next_offset": range.upperBound, "eof": range.upperBound == bytes.count,
            "data": bytes.subdata(in: range).base64EncodedString().replacingOccurrences(of: "=", with: "")
                .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_"),
        ])
    }
}

private actor ScriptedOperations: OperationAPI {
    private var answers: [Data]
    private(set) var paths: [String] = []
    init(answers: [Data]) { self.answers = answers }

    func perform(_ request: APIRequest) async throws -> APIResponse {
        paths.append(request.path)
        guard !answers.isEmpty else { return .completed(status: 404, body: Data()) }
        return .completed(status: 200, body: answers.removeFirst())
    }
    func recover(_ request: APIRequest) async throws -> APIResponse { try await perform(request) }
}
