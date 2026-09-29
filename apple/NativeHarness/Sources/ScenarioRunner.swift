import AlondraAPIClient
import AlondraAuth
import AlondraRelayOutbox
import AlondraRelaySession
import CryptoKit
import Foundation

/// Runs production client services with explicitly synthetic HTTP peers/storage.
/// This module is testonly and is never a dependency of either shipping app.
public enum NativeScenarioRunner {
    public static let scenarios = ["large-overview", "session-revoked", "lost-response"]

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
                    path: "/v1/tasks", body: Data(#"{"prompt":"synthetic work"}"#.utf8))
                report.checks["receipt_recovered"] = completed(first)
                await fixture.session.deactivate()
                let restarted = try fixture.makeSession()
                let second = try await restarted.execute(requestID: "synthetic-mutation", method: "POST",
                    path: "/v1/tasks", body: Data(#"{"prompt":"synthetic work"}"#.utf8))
                report.checks["durable_result_after_restart"] = completed(second)
                let observation = await fixture.peer.observation()
                report.checks["mutation_submitted_once"] = observation.mutations == 1
                report.checks["status_read_after_loss"] = observation.statusReads == 1 && observation.losses == 1
                report.checks["verified_encrypted_request"] = observation.opened == 1
                await restarted.deactivate()
            } else {
                if scenario == "session-revoked" {
                    let auth = fixture.auth
                    await fixture.peer.onFirstChunk { try await auth.logout() }
                }
                var result: Data?
                var rejected = false
                do { result = try await fixture.session.fetchOverview() }
                catch { rejected = true }
                let observation = await fixture.peer.observation()
                let authCalls = await fixture.authHTTP.observation()
                if scenario == "large-overview" {
                    report.checks["exact_large_unicode_document"] = result == fixture.document && fixture.document.count > 180 * 1024
                    report.checks["all_fragments_requested"] = observation.offsets == [0, 65_536, 131_072, 196_608]
                    report.checks["snapshot_released"] = observation.deletes == 1
                    report.checks["verified_encrypted_requests"] = observation.opened == 6
                    report.checks["single_token_exchange"] = authCalls.tokens == 1
                } else {
                    report.checks["revoked_read_rejected"] = rejected && result == nil
                    report.checks["first_fragment_reached"] = observation.offsets == [0]
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
    let storage = SyntheticCredentialStorage()
    let authHTTP = SyntheticAuthHTTP()
    let auth: AuthSession
    let peer: SyntheticRelayHTTP
    let session: RelaySession
    private let phoneSign = P256.Signing.PrivateKey()
    private let phoneAgreement = P256.KeyAgreement.PrivateKey()
    private let macSign = P256.Signing.PrivateKey()
    private let macAgreement = P256.KeyAgreement.PrivateKey()

    init(directory: URL, scenario: String) throws {
        self.directory = directory
        document = try JSONSerialization.data(withJSONObject: ["tick": 42,
            "reports": [String(repeating: "á🦜", count: 40_000)]], options: [.sortedKeys])
        auth = AuthSession(configuration: SyntheticAuthHTTP.configuration, transport: authHTTP,
            browser: ForbiddenBrowser(), storage: storage, now: { Date(timeIntervalSince1970: 1) })
        peer = SyntheticRelayHTTP(document: document, loseResponse: scenario == "lost-response",
            phoneSign: phoneSign.publicKey, phoneAgreement: phoneAgreement.publicKey,
            macSign: macSign, macAgreement: macAgreement)
        session = try Self.session(directory: directory, auth: auth, peer: peer,
            phoneSign: phoneSign, phoneAgreement: phoneAgreement, macSign: macSign, macAgreement: macAgreement)
    }
    func makeSession() throws -> RelaySession {
        try Self.session(directory: directory, auth: auth, peer: peer,
            phoneSign: phoneSign, phoneAgreement: phoneAgreement, macSign: macSign, macAgreement: macAgreement)
    }
    private static func session(directory: URL, auth: AuthSession, peer: SyntheticRelayHTTP,
        phoneSign: P256.Signing.PrivateKey, phoneAgreement: P256.KeyAgreement.PrivateKey,
        macSign: P256.Signing.PrivateKey, macAgreement: P256.KeyAgreement.PrivateKey) throws -> RelaySession {
        try RelaySession(journalFile: directory.appendingPathComponent("outbox.json"),
            scope: RelayOutboxScope(relay: URL(string: "https://relay.fixture.invalid")!,
                macID: "mac-test", deviceID: "phone-test", credentialRevision: "synthetic-generation"),
            bearer: { try await auth.bearerToken() },
            transport: peer, signing: phoneSign, agreement: phoneAgreement,
            macSigning: macSign.publicKey, macAgreement: macAgreement.publicKey, now: { 1000 }, sleep: {})
    }
}
