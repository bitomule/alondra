import AlondraAPIClient
import AlondraContracts
@testable import AlondraLoopbackSession
import CryptoKit
import Foundation
import XCTest

final class LoopbackSessionTests: XCTestCase {
    private var directory: URL!

    override func setUp() {
        directory = FileManager.default.temporaryDirectory.appending(path: "loopback-\(UUID().uuidString)")
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
    }

    private static let summary: [String] = ["summary"]

    private func read(_ api: AlondraAPIClient) async throws {
        _ = try await api.send(Endpoint<Data>.data(.get, path: Self.summary))
    }

    private func envelope() -> DomainV2.CommandEnvelope {
        DomainV2.CommandEnvelope(protocolVersion: DomainV2.protocolVersion, dataEpoch: "epoch", commandId: "command-1",
                                 expectedRevision: 3, command: .markRead(interactionId: "info-1"))
    }

    func testPairsItselfOnceThenSignsEveryRequestWithThePairedKey() async throws {
        let key = P256.Signing.PrivateKey()
        let daemon = StubDaemon { request in
            switch request.target {
            case "/v2/pairing": .status(200, Data(#"{"code":"482913","expires_in":600}"#.utf8))
            case "/v2/pair": .status(200, Data(#"{"device_id":"mac-device"}"#.utf8))
            default: .status(200, Data(#"{"revision":7}"#.utf8))
            }
        }
        let api = client(daemon, key: key)

        try await read(api)
        try await read(api)

        let requests = daemon.requests
        XCTAssertEqual(requests.map(\.target), ["/v2/pairing", "/v2/pair", "/summary", "/summary"])
        let pair = try JSONSerialization.jsonObject(with: requests[1].body) as? [String: String]
        XCTAssertEqual(pair?["code"], "482913")
        XCTAssertEqual(pair?["public_key"], key.publicKey.x963Representation.base64EncodedString())
        for request in requests.suffix(2) {
            XCTAssertEqual(request.headers["X-Alondra-Device"], "mac-device")
            let timestamp = try XCTUnwrap(request.headers["X-Alondra-Timestamp"])
            let signature = try P256.Signing.ECDSASignature(
                derRepresentation: XCTUnwrap(Data(base64Encoded: XCTUnwrap(request.headers["X-Alondra-Signature"]))))
            let digest = SHA256.hash(data: Data()).map { String(format: "%02x", $0) }.joined()
            XCTAssertTrue(key.publicKey.isValidSignature(signature, for: Data("GET\n/summary\n\(timestamp)\n\(digest)".utf8)))
        }
    }

    func testRevokedDeviceIsPairedAgainOnceAndTheRequestRepeated() async throws {
        let paired = Counter()
        let daemon = StubDaemon { request in
            switch request.target {
            case "/v2/pairing": return .status(200, Data(#"{"code":"1"}"#.utf8))
            case "/v2/pair": return .status(200, Data(#"{"device_id":"device-\#(paired.next())"}"#.utf8))
            default:
                return request.headers["X-Alondra-Device"] == "device-2"
                    ? .status(200, Data(#"{"revision":1}"#.utf8))
                    : .status(401, Data(#"{"error":"device not paired"}"#.utf8))
            }
        }
        let identity = LoopbackIdentityStore(directory: directory, makeKey: { .software(P256.Signing.PrivateKey()) })
        let api = client(daemon, identity: identity)

        try await read(api)

        XCTAssertEqual(daemon.requests.map(\.target), ["/v2/pairing", "/v2/pair", "/summary",
                                                      "/v2/pairing", "/v2/pair", "/summary"])
        let saved = await identity.deviceID()
        XCTAssertEqual(saved, "device-2")
    }

    func testPersistentRejectionIsReportedWithoutPairingInALoop() async throws {
        let daemon = StubDaemon { request in
            switch request.target {
            case "/v2/pairing": .status(200, Data(#"{"code":"1"}"#.utf8))
            case "/v2/pair": .status(200, Data(#"{"device_id":"device"}"#.utf8))
            default: .status(401, Data(#"{"error":"wrong signature"}"#.utf8))
            }
        }

        do {
            try await read(client(daemon))
            XCTFail("A second 401 must reach the caller")
        } catch let error as APIError {
            XCTAssertEqual(error, .unauthorized)
        }
        XCTAssertEqual(daemon.requests.filter { $0.target == "/v2/pair" }.count, 2)
    }

    func testCommandWhoseResponseIsLostIsUnknownAndNeverSentAgain() async throws {
        let daemon = pairedDaemon { _ in .failure(.networkConnectionLost) }
        let session = loopback(daemon)
        let api = AlondraAPIClient(serviceURL: LoopbackSession.defaultBase, deviceID: "mac", operations: session)

        do {
            _ = try await api.executeDomainCommand(envelope(), transportID: "operation-1")
            XCTFail("A lost response cannot be reported as success or failure")
        } catch let error as ServiceAPIError {
            XCTAssertEqual(error, .outcomeUnknown)
        }

        XCTAssertEqual(daemon.requests.filter { $0.target == "/interactions/info-1/read-state" }.count, 1)
        let outcome = await session.outcome(requestID: "operation-1")
        XCTAssertEqual(outcome, .unknown)
    }

    func testCommandThatNeverReachedTheDaemonFailsDefinitely() async throws {
        let daemon = pairedDaemon { _ in .failure(.cannotConnectToHost) }

        do {
            _ = try await client(daemon).executeDomainCommand(envelope(), transportID: "operation-1")
            XCTFail("A refused connection is a definite failure")
        } catch let error as URLError {
            XCTAssertEqual(error.code, .cannotConnectToHost)
        }
    }

    func testCompletedCommandOutcomeIsKept() async throws {
        let reply = Data(#"{"revision":4,"changed":true,"effects":[],"created_ids":[]}"#.utf8)
        let daemon = pairedDaemon { _ in .status(200, reply) }
        let session = loopback(daemon)
        let api = AlondraAPIClient(serviceURL: LoopbackSession.defaultBase, deviceID: "mac", operations: session)

        let recorded = try await api.executeDomainCommand(envelope(), transportID: "operation-2")

        XCTAssertEqual(recorded.revision, 4)
        let outcome = await session.outcome(requestID: "operation-2")
        XCTAssertEqual(outcome, .completed(status: 200, body: reply))
    }

    func testChangesLongPollTheLocalChangeFeed() async throws {
        let daemon = pairedDaemon { _ in
            .status(200, Data(#"{"protocol_version":2,"data_epoch":"epoch","revision":9,"changed":true}"#.utf8))
        }

        let changes = try await client(daemon).domainChanges(epoch: "epoch", after: 8, wait: 10)

        XCTAssertEqual(changes.revision, 9)
        XCTAssertEqual(daemon.requests.last?.target, "/changes?data_epoch=epoch&after=8&wait=10")
    }

    func testChunkedUploadIsStoredWithOneRawLocalUpload() async throws {
        let payload = Data((0..<150_000).map { UInt8($0 % 251) })
        let digest = SHA256.hash(data: payload).map { String(format: "%02x", $0) }.joined()
        let daemon = pairedDaemon { _ in
            .status(200, Data(#"{"id":"upload-1","name":"informe año.pdf","content_digest":"\#(digest)","content_reference":"files/uploads/upload-1","bytes":150000}"#.utf8))
        }
        var progress: [Double] = []
        let lock = NSLock()

        let stored = try await client(daemon).uploadDomainAttachment(payload, name: "informe año.pdf", transferID: UUID()) { value in
            lock.withLock { progress.append(value) }
        }

        XCTAssertEqual(stored.id, "upload-1")
        XCTAssertEqual(stored.contentDigest, digest)
        let uploads = daemon.requests.filter { $0.target.hasPrefix("/v2/uploads") }
        XCTAssertEqual(uploads.map(\.target), ["/v2/uploads?name=informe%20a%C3%B1o.pdf"])
        XCTAssertEqual(uploads.first?.body, payload)
        XCTAssertEqual(uploads.first?.headers["Content-Type"], "application/octet-stream")
        XCTAssertNotNil(uploads.first?.headers["X-Alondra-Signature"])
        XCTAssertFalse(daemon.requests.contains { $0.target.hasPrefix("/uploads") }, "the chunks stay in this process")
        XCTAssertEqual(lock.withLock { progress.last }, 1)
    }

    func testChunkedDownloadReadsTheLocalFileOnce() async throws {
        let file = Data((0..<100_000).map { UInt8($0 % 13) })
        let digest = SHA256.hash(data: file).map { String(format: "%02x", $0) }.joined()
        let daemon = pairedDaemon { _ in .status(200, file) }
        let attachment = DomainV2.AttachmentRef(id: "a-1", name: "a b.txt", contentDigest: digest,
                                                contentReference: "files/uploads/a b.txt", bytes: 100_000)

        let bytes = try await client(daemon).domainAttachmentContent(attachment)

        XCTAssertEqual(bytes, file)
        XCTAssertEqual(daemon.requests.filter { $0.target.hasPrefix("/v2/files") }.map(\.target),
                       ["/v2/files?path=files%2Fuploads%2Fa%20b.txt"])
    }

    private func pairedDaemon(_ work: @escaping (StubDaemon.Received) -> StubDaemon.Reply) -> StubDaemon {
        StubDaemon { request in
            switch request.target {
            case "/v2/pairing": .status(200, Data(#"{"code":"1"}"#.utf8))
            case "/v2/pair": .status(200, Data(#"{"device_id":"device"}"#.utf8))
            default: work(request)
            }
        }
    }

    private func loopback(_ daemon: StubDaemon, key: P256.Signing.PrivateKey = P256.Signing.PrivateKey(),
                          identity: LoopbackIdentityStore? = nil) -> LoopbackSession {
        LoopbackSession(identity: identity ?? LoopbackIdentityStore(directory: directory, makeKey: { .software(key) }),
                        deviceName: "Mac", urlSession: daemon.urlSession())
    }

    private func client(_ daemon: StubDaemon, key: P256.Signing.PrivateKey = P256.Signing.PrivateKey(),
                        identity: LoopbackIdentityStore? = nil) -> AlondraAPIClient {
        AlondraAPIClient(serviceURL: LoopbackSession.defaultBase, deviceID: "mac",
                         operations: loopback(daemon, key: key, identity: identity))
    }
}

private final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var value = 0
    func next() -> Int { lock.withLock { value += 1; return value } }
}
