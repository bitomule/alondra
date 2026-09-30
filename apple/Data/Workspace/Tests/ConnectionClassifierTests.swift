import AlondraAPIClient
import AlondraAuth
import AlondraContracts
import AlondraDomain
import AlondraLoopbackSession
import AlondraRelaySession
import AlondraWorkspace
import CryptoKit
import Foundation
import XCTest

final class ConnectionClassifierTests: XCTestCase {
    func testRemoteFailuresNameWhatThePersonCanDo() {
        let cases: [(any Error, ConnectionProblem)] = [
            (URLError(.notConnectedToInternet), .noInternet),
            (URLError(.cannotConnectToHost), .serverUnreachable),
            (URLError(.timedOut), .serverUnreachable),
            (RelayWireError.server(503), .serverUnreachable),
            (RelaySessionError.outcomeUnknown, .macNotAnswering),
            (ServiceAPIError.outcomeUnknown, .macNotAnswering),
            (RelayWireError.unauthorized, .deviceRevoked),
            (RelaySessionError.localStatus(401), .deviceRevoked),
            (AuthError.noSession, .sessionExpired),
            (AuthError.oauth(status: 400, code: "invalid_grant"), .sessionExpired),
            (CocoaError(.fileReadCorruptFile), .unexpected),
        ]
        for (error, expected) in cases {
            XCTAssertEqual(ConnectionClassifier.problem(for: error, local: false, networkOffline: false), expected, "\(error)")
        }
    }

    func testAReadTheMacNeverAnswersIsExplainedInWordsNotTheCaseName() {
        let described = ReadFailure.describing(ServiceAPIError.outcomeUnknown).localizedDescription
        XCTAssertTrue(described.contains("Your Mac didn’t answer"), described)
        XCTAssertFalse(described.contains("ServiceAPIError"), described)
        XCTAssertTrue(ReadFailure.describing(CancellationError()) is CancellationError)
        XCTAssertTrue(ReadFailure.describing(DomainV2.DomainError(code: "stale_head", message: "x", currentRevision: nil, subject: nil)) is DomainV2.DomainError)
    }

    func testANetworkWithoutAPathIsNoInternetEvenWhenTheRelayReadJustTimesOut() {
        XCTAssertEqual(ConnectionClassifier.problem(for: RelaySessionError.outcomeUnknown, local: false, networkOffline: true), .noInternet)
    }

    func testLoopbackFailuresMeanTheLocalDaemonIsNotRunningWhateverTheNetwork() {
        XCTAssertEqual(ConnectionClassifier.problem(for: URLError(.cannotConnectToHost), local: true, networkOffline: true), .alondraNotRunning)
        XCTAssertEqual(ConnectionClassifier.problem(for: LoopbackSessionError.status(502), local: true, networkOffline: false), .alondraNotRunning)
    }

    func testLocalWorkspaceWithoutADaemonReportsAlondraNotRunning() async throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "classifier-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let identity = LoopbackIdentityStore(directory: directory.appending(path: "identity"), makeKey: { .software(P256.Signing.PrivateKey()) })
        let session = LoopbackSession(base: try Self.closedPort(), identity: identity, deviceName: "Mac")
        let services = try await CloudAppServices.local(session, identity: identity)
        defer { Task { await services.deactivate() } }

        let updates = await services.domainWorkspaceClient().observe()
        var problem: ConnectionProblem?
        for await update in updates {
            if case .offline(let reported) = update.connection { problem = reported; break }
        }

        XCTAssertEqual(problem, .alondraNotRunning)
    }

    private static func closedPort() throws -> URL {
        let socket = Darwin.socket(AF_INET, SOCK_STREAM, 0)
        defer { close(socket) }
        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(socket, $0, length) }
        }
        _ = withUnsafeMutablePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(socket, $0, &length) }
        }
        return URL(string: "http://127.0.0.1:\(UInt16(bigEndian: address.sin_port))")!
    }
}
