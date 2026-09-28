import AlondraAPIClient
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

    func testPairsItselfOnceThenSignsEveryRequestWithThePairedKey() async throws {
        let key = P256.Signing.PrivateKey()
        let daemon = StubDaemon { request in
            switch request.target {
            case "/v1/pairing": .status(200, Data(#"{"code":"482913","expires_in":600}"#.utf8))
            case "/v1/pair": .status(200, Data(#"{"device_id":"mac-device"}"#.utf8))
            default: .status(200, Data(#"{"tick":7}"#.utf8))
            }
        }
        let api = client(daemon, key: key)

        _ = try await api.overview()
        _ = try await api.overview()

        let requests = daemon.requests
        XCTAssertEqual(requests.map(\.target), ["/v1/pairing", "/v1/pair", "/v1/overview", "/v1/overview"])
        let pair = try JSONSerialization.jsonObject(with: requests[1].body) as? [String: String]
        XCTAssertEqual(pair?["code"], "482913")
        XCTAssertEqual(pair?["public_key"], key.publicKey.x963Representation.base64EncodedString())
        for request in requests.suffix(2) {
            XCTAssertEqual(request.headers["X-Alondra-Device"], "mac-device")
            let timestamp = try XCTUnwrap(request.headers["X-Alondra-Timestamp"])
            let signature = try P256.Signing.ECDSASignature(
                derRepresentation: XCTUnwrap(Data(base64Encoded: XCTUnwrap(request.headers["X-Alondra-Signature"]))))
            let digest = SHA256.hash(data: Data()).map { String(format: "%02x", $0) }.joined()
            XCTAssertTrue(key.publicKey.isValidSignature(signature, for: Data("GET\n/v1/overview\n\(timestamp)\n\(digest)".utf8)))
        }
    }

    func testRevokedDeviceIsPairedAgainOnceAndTheRequestRepeated() async throws {
        let paired = Counter()
        let daemon = StubDaemon { request in
            switch request.target {
            case "/v1/pairing": return .status(200, Data(#"{"code":"1"}"#.utf8))
            case "/v1/pair": return .status(200, Data(#"{"device_id":"device-\#(paired.next())"}"#.utf8))
            default:
                return request.headers["X-Alondra-Device"] == "device-2"
                    ? .status(200, Data(#"{"tick":1}"#.utf8))
                    : .status(401, Data(#"{"error":"device not paired"}"#.utf8))
            }
        }
        let identity = LoopbackIdentityStore(directory: directory, makeKey: { .software(P256.Signing.PrivateKey()) })
        let api = client(daemon, identity: identity)

        _ = try await api.overview()

        XCTAssertEqual(daemon.requests.map(\.target), ["/v1/pairing", "/v1/pair", "/v1/overview",
                                                      "/v1/pairing", "/v1/pair", "/v1/overview"])
        let saved = await identity.deviceID()
        XCTAssertEqual(saved, "device-2")
    }

    func testPersistentRejectionIsReportedWithoutPairingInALoop() async throws {
        let daemon = StubDaemon { request in
            switch request.target {
            case "/v1/pairing": .status(200, Data(#"{"code":"1"}"#.utf8))
            case "/v1/pair": .status(200, Data(#"{"device_id":"device"}"#.utf8))
            default: .status(401, Data(#"{"error":"wrong signature"}"#.utf8))
            }
        }

        do {
            _ = try await client(daemon).overview()
            XCTFail("A second 401 must reach the caller")
        } catch let error as APIError {
            XCTAssertEqual(error, .unauthorized)
        }
        XCTAssertEqual(daemon.requests.filter { $0.target == "/v1/pair" }.count, 2)
    }

    func testMutationWhoseResponseIsLostIsUnknownAndNeverSentAgain() async throws {
        let daemon = pairedDaemon { _ in .failure(.networkConnectionLost) }
        let session = loopback(daemon)
        let api = AlondraAPIClient(serviceURL: LoopbackSession.defaultBase, deviceID: "mac", operations: session)

        do {
            try await api.markRead(12, operationID: "operation-1")
            XCTFail("A lost response cannot be reported as success or failure")
        } catch let error as ServiceAPIError {
            XCTAssertEqual(error, .outcomeUnknown)
        }

        XCTAssertEqual(daemon.requests.filter { $0.target == "/v1/reports/12/read" }.count, 1)
        let outcome = await session.outcome(requestID: "operation-1")
        XCTAssertEqual(outcome, .unknown)
    }

    func testMutationThatNeverReachedTheDaemonFailsDefinitely() async throws {
        let daemon = pairedDaemon { _ in .failure(.cannotConnectToHost) }

        do {
            try await client(daemon).markRead(12, operationID: "operation-1")
            XCTFail("A refused connection is a definite failure")
        } catch let error as URLError {
            XCTAssertEqual(error.code, .cannotConnectToHost)
        }
    }

    func testCompletedMutationOutcomeIsKeptForRecovery() async throws {
        let daemon = pairedDaemon { _ in .status(200, Data(#"{"ok":true}"#.utf8)) }
        let session = loopback(daemon)
        let api = AlondraAPIClient(serviceURL: LoopbackSession.defaultBase, deviceID: "mac", operations: session)

        try await api.markRead(3, operationID: "operation-2")

        let outcome = await session.outcome(requestID: "operation-2")
        XCTAssertEqual(outcome, .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)))
    }

    func testWaitForChangeLongPollsTheLocalChangeFeed() async throws {
        let daemon = pairedDaemon { _ in .status(200, Data(#"{"tick":9}"#.utf8)) }

        try await client(daemon).waitForChange(afterSnapshotTick: 8, seconds: 10)

        XCTAssertEqual(daemon.requests.last?.target, "/v1/changes?after=8&wait=10")
    }

    func testChunkedUploadIsStoredWithOneRawLocalUpload() async throws {
        let payload = Data((0..<150_000).map { UInt8($0 % 251) })
        let daemon = pairedDaemon { request in
            .status(200, Data(#"{"id":"upload-1","name":"informe año.pdf","bytes":150000,"kind":"file","path":"/x"}"#.utf8))
        }
        var progress: [Double] = []
        let lock = NSLock()

        let id = try await client(daemon).uploadAttachment(payload, name: "informe año.pdf", transferID: UUID()) { value in
            lock.withLock { progress.append(value) }
        }

        XCTAssertEqual(id, "upload-1")
        let uploads = daemon.requests.filter { $0.target.hasPrefix("/v1/uploads") }
        XCTAssertEqual(uploads.map(\.target), ["/v1/uploads?name=informe%20a%C3%B1o.pdf"])
        XCTAssertEqual(uploads.first?.body, payload)
        XCTAssertEqual(uploads.first?.headers["Content-Type"], "application/octet-stream")
        XCTAssertFalse(daemon.requests.contains { $0.target.hasPrefix("/v1/service/") })
        XCTAssertEqual(lock.withLock { progress.last }, 1)
    }

    func testChunkedDownloadReadsTheLocalFileOnce() async throws {
        let file = Data((0..<100_000).map { UInt8($0 % 13) })
        let daemon = pairedDaemon { _ in .status(200, file) }

        let bytes = try await client(daemon).downloadAttachment(path: "/home/files/a b.txt", downloadID: UUID())

        XCTAssertEqual(bytes, file)
        XCTAssertEqual(daemon.requests.filter { $0.target.hasPrefix("/v1/file") }.map(\.target),
                       ["/v1/file?path=%2Fhome%2Ffiles%2Fa%20b.txt"])
    }

    private func pairedDaemon(_ work: @escaping (StubDaemon.Received) -> StubDaemon.Reply) -> StubDaemon {
        StubDaemon { request in
            switch request.target {
            case "/v1/pairing": .status(200, Data(#"{"code":"1"}"#.utf8))
            case "/v1/pair": .status(200, Data(#"{"device_id":"device"}"#.utf8))
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
