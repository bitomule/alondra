import AlondraAPIClient
import AlondraRelayOutbox
import CryptoKit
import Foundation
import XCTest
@testable import AlondraRelaySession

final class OverviewSnapshotTests: XCTestCase {
    func testActualAPIAdapterAssemblesLargeUnicodeDocumentAndReleasesSnapshot() async throws {
        let fixture = try SnapshotFixture()
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let result = try await fixture.api.fetchOverview()
        XCTAssertEqual(result, fixture.document)
        let paths = await fixture.http.paths
        XCTAssertEqual(paths, ["/v1/overview", OverviewSnapshotAssembly.root,
            SnapshotHTTP.path + "?offset=65536", SnapshotHTTP.path + "?offset=131072",
            SnapshotHTTP.path + "?offset=196608", SnapshotHTTP.path])
        let methods = await fixture.http.methods
        XCTAssertEqual(methods.last, "DELETE")
        let calls = await fixture.credentials.counts
        XCTAssertEqual(calls.normal, 1)
        XCTAssertGreaterThan(calls.cached, 4)
    }

    func testPendingSnapshotAdvancesPastUnrelatedChangesWithoutReposting() async throws {
        let fixture = try SnapshotFixture(mode: .pending)
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let value = try await fixture.api.fetchOverview()
        XCTAssertEqual(value, fixture.document)
        let cursors = await fixture.http.cursors
        XCTAssertEqual(cursors, [1, 1001])
        let paths = await fixture.http.paths
        XCTAssertEqual(paths.filter { $0 == OverviewSnapshotAssembly.root }.count, 1)
    }

    func testFragmentDeadlineCancelsSuspendedHTTPWithoutPublishing() async throws {
        let deadline = SnapshotGate(), httpGate = SnapshotGate()
        let fixture = try SnapshotFixture(readDeadline: { _ in try await deadline.wait() })
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        await fixture.http.holdFirst(httpGate)
        let task = Task { try await fixture.api.fetchOverview() }
        await httpGate.started()
        await deadline.started()
        await deadline.release()
        do { _ = try await task.value; XCTFail("deadline published snapshot") } catch { }
        let cancelled = await httpGate.wasCancelled
        XCTAssertTrue(cancelled)
        let paths = await fixture.http.paths
        XCTAssertFalse(paths.contains(SnapshotHTTP.path + "?offset=65536"))
    }

    func testSmallOverviewPreservesExistingTransportWithoutFragmentAuthentication() async throws {
        let fixture = try SnapshotFixture(mode: .small)
        defer { try? FileManager.default.removeItem(at: fixture.directory) }
        let result = try await fixture.api.fetchOverview()
        XCTAssertEqual(try JSONDecoder().decode(Tick.self, from: result).tick, 42)
        let paths = await fixture.http.paths
        XCTAssertEqual(paths, ["/v1/overview"])
        let counts = await fixture.credentials.counts
        XCTAssertEqual(counts.cached, 0)
    }

    func testMalformedOrUnauthenticatedFragmentsNeverReturnPartialOverview() async throws {
        for mode in [SnapshotHTTP.Mode.wrongSize, .wrongHash, .wrongOffset, .changedMetadata,
                     .paddedBase64, .malformedJSON, .expired, .forged, .denied, .oversizedTotal] {
            let fixture = try SnapshotFixture(mode: mode)
            defer { try? FileManager.default.removeItem(at: fixture.directory) }
            do { _ = try await fixture.api.fetchOverview(); XCTFail("accepted \(mode)") }
            catch { }
            let paths = await fixture.http.paths
            XCTAssertTrue(paths.contains(OverviewSnapshotAssembly.root))
            XCTAssertFalse(paths.contains(SnapshotHTTP.path + "?offset=262144"))
        }
    }

    func testGeneric502DoesNotActivateFallback() async throws {
        for mode in [SnapshotHTTP.Mode.generic502, .forgedInitial] {
            let fixture = try SnapshotFixture(mode: mode)
            defer { try? FileManager.default.removeItem(at: fixture.directory) }
            do { _ = try await fixture.api.fetchOverview(); XCTFail("accepted unverified failure") } catch { }
            let paths = await fixture.http.paths
            XCTAssertEqual(paths, ["/v1/overview"])
        }
    }

    func testLogoutOrCancellationAfterFirstFragmentCannotPublishOrRefresh() async throws {
        for cancellation in [false, true] {
            let fixture = try SnapshotFixture()
            defer { try? FileManager.default.removeItem(at: fixture.directory) }
            let gate = SnapshotGate()
            await fixture.http.holdFirst(gate)
            let task = Task { try await fixture.api.fetchOverview() }
            await gate.started()
            if cancellation { task.cancel() } else {
                await fixture.credentials.invalidate()
                await fixture.session.deactivate()
            }
            await gate.release()
            do { _ = try await task.value; XCTFail("published obsolete snapshot") } catch { }
            let counts = await fixture.credentials.counts
            XCTAssertEqual(counts.normal, 1)
            let paths = await fixture.http.paths
            XCTAssertFalse(paths.contains(SnapshotHTTP.path + "?offset=65536"))
        }
    }

    private struct Tick: Decodable { let tick: UInt64 }
}

private actor SnapshotCredentials {
    private var valid = true
    private var normal = 0
    private var cached = 0
    var counts: (normal: Int, cached: Int) { (normal, cached) }
    func ordinary() -> String { normal += 1; return "fixture-token" }
    func lease() throws -> @Sendable () async throws -> String {
        guard valid else { throw CancellationError() }
        return { try await self.token() }
    }
    func token() throws -> String {
        cached += 1
        guard valid else { throw CancellationError() }
        return "fixture-token"
    }
    func invalidate() { valid = false }
}

private struct SnapshotFixture {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    let credentials = SnapshotCredentials()
    let document: Data
    let http: SnapshotHTTP
    let session: RelaySession
    let api: AlondraAPIClient
    init(mode: SnapshotHTTP.Mode = .normal,
         readDeadline: @escaping @Sendable (ContinuousClock.Instant) async throws -> Void = { try await ContinuousClock().sleep(until: $0) }) throws {
        let phoneSign = P256.Signing.PrivateKey(), macSign = P256.Signing.PrivateKey()
        let phoneAgreement = P256.KeyAgreement.PrivateKey(), macAgreement = P256.KeyAgreement.PrivateKey()
        document = try JSONSerialization.data(withJSONObject: ["tick": 42, "reports": [String(repeating: "á🦜", count: 40_000)]], options: [.sortedKeys])
        http = SnapshotHTTP(document: document, mode: mode, macSign: macSign,
            macAgreement: macAgreement, phoneSign: phoneSign.publicKey, phoneAgreement: phoneAgreement.publicKey)
        let credentials = credentials
        session = try RelaySession(journalFile: directory.appendingPathComponent("outbox.json"),
            scope: RelayOutboxScope(relay: URL(string: "https://relay.example")!, macID: "mac-test",
                deviceID: "phone-test", credentialRevision: "fixture-generation"),
            bearer: { await credentials.ordinary() }, cachedBearer: { try await credentials.lease() }, transport: http,
            signing: phoneSign, agreement: phoneAgreement, macSigning: macSign.publicKey,
            macAgreement: macAgreement.publicKey, now: { 1000 }, sleep: {}, waitForReadDeadline: readDeadline)
        api = AlondraAPIClient(serviceURL: URL(string: "https://relay.example")!, deviceID: "phone-test", operations: session)
    }
}

private actor SnapshotHTTP: ServiceHTTPTransport {
    enum Mode { case normal, small, generic502, pending, forgedInitial, wrongSize, wrongHash, wrongOffset, changedMetadata, paddedBase64, malformedJSON, expired, forged, denied, oversizedTotal }
    static let id = String(repeating: "a", count: 32)
    static let path = OverviewSnapshotAssembly.root + "/" + id
    let document: Data
    let mode: Mode
    let macSign: P256.Signing.PrivateKey
    let macAgreement: P256.KeyAgreement.PrivateKey
    let phoneSign: P256.Signing.PublicKey
    let phoneAgreement: P256.KeyAgreement.PublicKey
    var paths: [String] = []
    var methods: [String] = []
    var cursors: [UInt64] = []
    private var pendingBody: Data?
    private var pendingID = ""
    private var gate: SnapshotGate?
    init(document: Data, mode: Mode, macSign: P256.Signing.PrivateKey, macAgreement: P256.KeyAgreement.PrivateKey,
         phoneSign: P256.Signing.PublicKey, phoneAgreement: P256.KeyAgreement.PublicKey) {
        self.document = document; self.mode = mode; self.macSign = macSign; self.macAgreement = macAgreement
        self.phoneSign = phoneSign; self.phoneAgreement = phoneAgreement
    }
    func holdFirst(_ gate: SnapshotGate) { self.gate = gate }
    func send(_ request: URLRequest) async throws -> ServiceHTTPResponse {
        if request.url?.path == "/v1/bootstrap" {
            return ServiceHTTPResponse(status: 200, body: Data(#"{"protocol":1,"epoch":"AAAAAAAAAAAAAAAAAAAAAA"}"#.utf8))
        }
        if request.url?.path == "/v1/changes" {
            let items = URLComponents(url: try XCTUnwrap(request.url), resolvingAgainstBaseURL: false)!.queryItems!
            cursors.append(try XCTUnwrap(items.first { $0.name == "since" }?.value.flatMap(UInt64.init)))
            return ServiceHTTPResponse(status: 200, body: try JSONSerialization.data(withJSONObject: [
                "epoch": "AAAAAAAAAAAAAAAAAAAAAA", "revision": cursors.count == 1 ? 1001 : 1002, "changes": []]))
        }
        if request.url?.path == "/v1/requests/" + pendingID, let pendingBody {
            if cursors.count < 2 { return ServiceHTTPResponse(status: 200, body: try pendingReceipt().body) }
            return ServiceHTTPResponse(status: 200, body: pendingBody)
        }
        guard request.httpMethod == "POST", request.url?.path == "/v1/requests" else {
            return ServiceHTTPResponse(status: 410, body: Data())
        }
        let envelope = try JSONDecoder().decode(EnvelopeV1.self, from: XCTUnwrap(request.httpBody))
        let plaintext = try envelope.open(senderKey: phoneSign, recipientKey: macAgreement,
            sender: "phone-test", recipient: "mac-test", route: "mac-test", nowMilliseconds: 1000)
        let operation = try XCTUnwrap(JSONSerialization.jsonObject(with: plaintext) as? [String: Any])
        let path = try XCTUnwrap(operation["path"] as? String), method = try XCTUnwrap(operation["method"] as? String)
        paths.append(path); methods.append(method)
        let payload: [String: Any]
        if path == "/v1/overview" {
            payload = mode == .small ? ["status": 200, "body": ["tick": 42]] :
                ["status": 502, "error": mode == .generic502 ? "unavailable" : "response_too_large"]
        } else if method == "DELETE" { payload = ["status": 200, "body": ["released": true]] }
        else {
            if path == OverviewSnapshotAssembly.root, let gate { try await gate.wait() }
            if mode == .denied { return ServiceHTTPResponse(status: 401, body: Data()) }
            let offset = path == OverviewSnapshotAssembly.root ? 0 : try XCTUnwrap(Int(path.components(separatedBy: "offset=").last!))
            let bytes = mode == .malformedJSON ? Data(repeating: 65, count: document.count) : document
            var chunk = bytes.subdata(in: offset..<min(offset + 65_536, bytes.count))
            if mode == .wrongSize && offset > 0 { chunk.removeLast() }
            var body: [String: Any] = ["schema": 1, "snapshot_id": Self.id, "total_bytes": bytes.count,
                "sha256": SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined(),
                "chunk_bytes": 65_536, "offset": offset,
                "data": chunk.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")]
            if mode == .wrongHash { body["sha256"] = String(repeating: "0", count: 64) }
            if mode == .wrongOffset && offset > 0 { body["offset"] = 0 }
            if mode == .changedMetadata && offset > 0 { body["snapshot_id"] = String(repeating: "b", count: 32) }
            if mode == .paddedBase64 { body["data"] = (body["data"] as! String) + "=" }
            if mode == .oversizedTotal { body["total_bytes"] = OverviewSnapshotAssembly.maximum + 1 }
            payload = ["status": 200, "body": body]
        }
        let response = try EnvelopeV1.seal(route: "mac-test", sender: "mac-test", recipient: "phone-test",
            requestID: envelope.requestID, kind: "response", expiresAtMilliseconds: mode == .expired && path != "/v1/overview" ? 999 : 5000,
            plaintext: JSONSerialization.data(withJSONObject: payload),
            senderKey: ((mode == .forged && path != "/v1/overview") || mode == .forgedInitial) ? P256.Signing.PrivateKey() : macSign, recipientKey: phoneAgreement)
        let body = try JSONSerialization.data(withJSONObject: [
            "request_id": envelope.requestID, "status": "completed", "revision": mode == .pending ? 1002 : paths.count,
            "response": JSONSerialization.jsonObject(with: JSONEncoder().encode(response))])
        if mode == .pending, path == OverviewSnapshotAssembly.root {
            pendingBody = body; pendingID = envelope.requestID
            return try pendingReceipt()
        }
        return ServiceHTTPResponse(status: 200, body: body)
    }
    private func pendingReceipt() throws -> ServiceHTTPResponse {
        ServiceHTTPResponse(status: 202, body: try JSONSerialization.data(withJSONObject:
            ["request_id": pendingID, "status": "pending", "revision": 1]))
    }
}

private actor SnapshotGate {
    private var entered = false
    private var startWaiters: [CheckedContinuation<Void, Never>] = []
    private var releaseWaiter: CheckedContinuation<Void, Error>?
    private(set) var wasCancelled = false
    func wait() async throws {
        try await withTaskCancellationHandler {
            try Task.checkCancellation()
            entered = true
            startWaiters.forEach { $0.resume() }; startWaiters.removeAll()
            try await withCheckedThrowingContinuation { releaseWaiter = $0 }
        } onCancel: { Task { await self.cancel() } }
    }
    private func cancel() {
        wasCancelled = true
        releaseWaiter?.resume(throwing: CancellationError()); releaseWaiter = nil
    }
    func started() async {
        if entered { return }
        await withCheckedContinuation { startWaiters.append($0) }
    }
    func release() { releaseWaiter?.resume(); releaseWaiter = nil }
}
