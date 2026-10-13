import AlondraTransport
import AlondraAccount
import AlondraContract
import CryptoKit
import Foundation

/// Runs production client services with explicitly synthetic HTTP peers/storage.
/// This module is testonly and is never a dependency of either shipping app.
public enum NativeScenarioRunner {
    public static let scenarios = ["large-resource-read", "session-revoked", "lost-response"]

    public static func run(environment: [String: String]) async -> Int32 {
        guard let runID = environment["ALONDRA_TEST_RUN_ID"], UUID(uuidString: runID) != nil,
              let scenario = environment["ALONDRA_TEST_SCENARIO"], scenarios.contains(scenario),
              let destination = outputURL(environment) else { return 2 }
        let start = ContinuousClock.now
        var report = Report(runID: runID, scenario: scenario)
        let directory = destination.deletingLastPathComponent().appendingPathComponent("work-" + runID)
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false,
                attributes: [.posixPermissions: 0o700])
            defer { try? FileManager.default.removeItem(at: directory) }
            let fixture = try ScenarioFixture(directory: directory, scenario: scenario)
            if scenario == "lost-response" {
                let first = try await fixture.session.execute(requestID: "synthetic-mutation", method: "POST",
                    path: "/requests", body: Data(#"{"data_epoch":"synthetic","command_id":"synthetic-command","expected_revision":0,"request_id":"synthetic-request","body":"synthetic work"}"#.utf8))
                report.checks["receipt_recovered"] = completed(first)
                await fixture.session.deactivate()
                let restarted = try fixture.makeSession()
                let second = try await restarted.execute(requestID: "synthetic-mutation", method: "POST",
                    path: "/requests", body: Data(#"{"data_epoch":"synthetic","command_id":"synthetic-command","expected_revision":0,"request_id":"synthetic-request","body":"synthetic work"}"#.utf8))
                report.checks["durable_result_after_restart"] = completed(second)
                let observation = await fixture.peer.observation()
                report.checks["mutation_submitted_once"] = observation.mutations == 1
                report.checks["status_read_after_loss"] = observation.statusReads == 1 && observation.losses == 1
                report.checks["verified_encrypted_request"] = observation.opened == 1
                await restarted.deactivate()
            } else {
                let api = AlondraAPIClient(serviceURL: fixture.scope.relay, deviceID: fixture.scope.deviceID, operations: fixture.session)
                if scenario == "session-revoked" {
                    let auth = fixture.auth
                    await fixture.peer.onFirstChunk { try await auth.logout() }
                }
                var result: Data?
                var rejected = false
                do {
                    if scenario == "large-resource-read" {
                        _ = try await api.send(Endpoint<Data>.data(.get, path: ["summary"]))
                        _ = try await api.send(Endpoint<Data>.data(.get, path: ["documents", "document-1"]))
                    }
                    let text = try await readDocumentContent(api, expectedDigest: fixture.documentDigest)
                    result = Data(text.utf8)
                }
                catch { rejected = true }
                let observation = await fixture.peer.observation()
                let authCalls = await fixture.authHTTP.observation()
                if scenario == "large-resource-read" {
                    let chunks = stride(from: 0, to: fixture.document.count, by: SyntheticRelayHTTP.chunkBytes).map { $0 }
                    report.checks["exact_large_unicode_document"] = result == fixture.document && fixture.document.count > 180 * 1024
                    report.checks["all_resource_chunks_requested"] = observation.offsets == chunks
                    report.checks["resource_reads_only"] = observation.resourcePaths.contains("/summary")
                        && observation.resourcePaths.contains("/documents/document-1")
                        && observation.resourcePaths.allSatisfy { !$0.contains("snapshot") && !$0.contains("/v1/") && !$0.contains("/v2/") }
                    report.checks["verified_encrypted_requests"] = observation.opened == chunks.count + 2
                    report.checks["single_token_exchange"] = authCalls.tokens == 1
                } else {
                    report.checks["revoked_read_rejected"] = rejected && result == nil
                    report.checks["first_resource_chunk_reached"] = observation.offsets == [0]
                    report.checks["no_token_renewal_after_revocation"] = authCalls.tokens == 1
                    report.checks["issuer_revocation_executed"] = authCalls.revocations == 1
                    report.checks["credential_removed"] = try fixture.storage.read() == nil
                    var leaseRejected = false
                    do { _ = try await fixture.auth.cachedBearerLease() } catch { leaseRejected = true }
                    report.checks["cached_lease_rejected"] = leaseRejected
                }
                await fixture.session.deactivate()
            }
            report.status = report.checks.values.allSatisfy { $0 } && !report.checks.isEmpty ? "passed" : "failed"
        } catch {
            report.status = "failed"
            // No arbitrary localized error strings (which may include request data).
            report.error = "scenario_execution_failed"
        }
        let elapsed = start.duration(to: .now).components
        report.elapsedMilliseconds = Double(elapsed.seconds) * 1000 + Double(elapsed.attoseconds) / 1e15
        do {
            let encoder = JSONEncoder(); encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            try encoder.encode(report).write(to: destination, options: .atomic)
        } catch { return 3 }
        return report.status == "passed" ? 0 : 1
    }

    private static func completed(_ result: RelayOperationResult) -> Bool {
        guard case let .completed(status, body) = result, status == 201,
              let value = try? JSONDecoder().decode(MutationReply.self, from: body) else { return false }
        return value.id == "synthetic-task" && value.accepted
    }

    /// Keep the fixture's immutable, digest-checked fragmented read independent of
    /// the previous client so this harness can use the shipping Views client.
    private static func readDocumentContent(_ api: AlondraAPIClient, expectedDigest: String) async throws -> String {
        struct Chunk: Decodable, Sendable {
            let revisionId: String
            let contentDigest: String
            let version: String
            let data: String
            let offset: UInt64
            let total: UInt64
            let nextOffset: UInt64
            let eof: Bool
        }
        var collected = Data()
        var version: String?
        var total: UInt64?
        repeat {
            let offset = collected.count
            let endpoint = Endpoint<Chunk>(.get,
                path: ["document-revisions", SyntheticRelayHTTP.revisionID, "content"],
                query: [("offset", String(offset))] + (version.map { [("version", $0)] } ?? [])) {
                    let decoder = JSONDecoder()
                    decoder.keyDecodingStrategy = .convertFromSnakeCase
                    return try decoder.decode(Chunk.self, from: $0)
                }
            let chunk = try await api.send(endpoint)
            guard chunk.revisionId == SyntheticRelayHTTP.revisionID,
                  chunk.contentDigest == expectedDigest, chunk.contentDigest.count == 64,
                  !chunk.version.isEmpty, version == nil || version == chunk.version,
                  total == nil || total == chunk.total, chunk.total <= 32 * 1024 * 1024,
                  chunk.offset == UInt64(offset), chunk.nextOffset <= chunk.total,
                  chunk.nextOffset > chunk.offset || chunk.eof else { throw RelayWireError.invalidField }
            let base64 = chunk.data.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            guard chunk.data.utf8.allSatisfy({
                (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
            }), let bytes = Data(base64Encoded: base64 + String(repeating: "=", count: (4 - base64.count % 4) % 4)),
                  bytes.base64EncodedString().replacingOccurrences(of: "+", with: "-")
                    .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "") == chunk.data,
                  UInt64(bytes.count) == chunk.nextOffset - chunk.offset else { throw RelayWireError.invalidField }
            collected.append(bytes)
            version = chunk.version
            total = chunk.total
            if chunk.eof {
                guard chunk.nextOffset == chunk.total,
                      SHA256.hash(data: collected).map({ String(format: "%02x", $0) }).joined() == expectedDigest,
                      let text = String(data: collected, encoding: .utf8) else { throw RelayWireError.invalidField }
                return text
            }
        } while true
    }
    private struct MutationReply: Decodable { let id: String; let accepted: Bool }

    private static func outputURL(_ environment: [String: String]) -> URL? {
        #if os(iOS)
        return FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first?
            .appendingPathComponent("native-result.json")
        #else
        guard let path = environment["ALONDRA_TEST_OUTPUT"], path.hasPrefix("/") else { return nil }
        return URL(fileURLWithPath: path)
        #endif
    }
    private struct Report: Encodable {
        let schema = 1
        let scope = "native-client-with-synthetic-http-peers"
        let runID: String
        let scenario: String
        var status = "running"
        var checks: [String: Bool] = [:]
        var elapsedMilliseconds: Double = 0
        var error: String?
    }
}

private struct ScenarioFixture {
    let directory: URL
    let document: Data
    let scope: RelayOutboxScope
    let storage = SyntheticCredentialStorage()
    let authHTTP = SyntheticAuthHTTP()
    let auth: AuthSession
    let peer: SyntheticRelayHTTP
    let session: RelaySession
    private let phoneSign = P256.Signing.PrivateKey()
    private let phoneAgreement = P256.KeyAgreement.PrivateKey()
    private let macSign = P256.Signing.PrivateKey()
    private let macAgreement = P256.KeyAgreement.PrivateKey()
    var documentDigest: String { SHA256.hash(data: document).map { String(format: "%02x", $0) }.joined() }

    init(directory: URL, scenario: String) throws {
        self.directory = directory
        document = try JSONSerialization.data(withJSONObject: ["tick": 42,
            "reports": [String(repeating: "á🦜", count: 40_000)]], options: [.sortedKeys])
        auth = AuthSession(configuration: SyntheticAuthHTTP.configuration, transport: authHTTP,
            browser: ForbiddenBrowser(), storage: storage, now: { Date(timeIntervalSince1970: 1) })
        peer = SyntheticRelayHTTP(document: document, loseResponse: scenario == "lost-response",
            phoneSign: phoneSign.publicKey, phoneAgreement: phoneAgreement.publicKey,
            macSign: macSign, macAgreement: macAgreement)
        scope = RelayOutboxScope(relay: URL(string: "https://relay.fixture.invalid")!,
            macID: "mac-test", deviceID: "phone-test", credentialRevision: "synthetic-generation")
        session = try Self.session(directory: directory, scope: scope, auth: auth, peer: peer,
            phoneSign: phoneSign, phoneAgreement: phoneAgreement, macSign: macSign, macAgreement: macAgreement)
    }
    func makeSession() throws -> RelaySession {
        try Self.session(directory: directory, scope: scope, auth: auth, peer: peer,
            phoneSign: phoneSign, phoneAgreement: phoneAgreement, macSign: macSign, macAgreement: macAgreement)
    }
    private static func session(directory: URL, scope: RelayOutboxScope, auth: AuthSession, peer: SyntheticRelayHTTP,
        phoneSign: P256.Signing.PrivateKey, phoneAgreement: P256.KeyAgreement.PrivateKey,
        macSign: P256.Signing.PrivateKey, macAgreement: P256.KeyAgreement.PrivateKey) throws -> RelaySession {
        try RelaySession(scope: scope,
            bearer: { try await auth.bearerToken() },
            transport: peer, signing: phoneSign, agreement: phoneAgreement,
            macSigning: macSign.publicKey, macAgreement: macAgreement.publicKey, now: { 1000 }, sleep: { _ in })
    }
}
