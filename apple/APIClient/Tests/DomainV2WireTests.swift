import AlondraAPIClient
import AlondraContracts
import Foundation
import XCTest

private actor DomainOperations: ServiceOperationTransport {
    var requests: [ServiceRequest] = []
    let responses: [ServiceResponse]
    init(_ response: ServiceResponse) { self.responses = [response] }
    init(_ responses: [ServiceResponse]) { self.responses = responses }
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        requests.append(request)
        return responses[min(requests.count - 1, responses.count - 1)]
    }
    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        XCTFail("Domain mutation must not poll")
    }
    func recorded() -> [ServiceRequest] { requests }
}

final class DomainV2WireTests: XCTestCase {
    private func client(_ operations: DomainOperations) -> AlondraAPIClient {
        .init(serviceURL: URL(string: "https://service.example.invalid")!, deviceID: "synthetic-mac", operations: operations)
    }

    private var envelope: DomainV2.CommandEnvelope {
        .init(protocolVersion: 2, dataEpoch: "epoch-2", commandId: "command-1", expectedRevision: 7,
              command: .answerQuestion(attachments: [], interactionId: "question-1", optionId: "other",
                                       inputs: ["explanation": .text(value: "Synthetic reason")]))
    }

    func testCommandKeepsExactTaggedShapeAndDurableIdentityWithoutActor() async throws {
        let operations = DomainOperations(.completed(status: 200, body: Data(#"{"revision":8,"changed":true,"effects":[],"created_ids":[]}"#.utf8)))
        let outcome = try await client(operations).executeDomainCommand(envelope)
        XCTAssertEqual(outcome.revision, 8)
        let requests = await operations.recorded()
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests[0].path, "/v2/domain/commands")
        XCTAssertEqual(requests[0].operationID, "command-1")
        let body = try XCTUnwrap(requests[0].body)
        XCTAssertEqual(try DomainV2.decode(DomainV2.CommandEnvelope.self, from: body), envelope)
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertNil(json["actor"])
        XCTAssertEqual(json["data_epoch"] as? String, "epoch-2")
        let command = try XCTUnwrap(json["command"] as? [String: Any])
        XCTAssertEqual(command["kind"] as? String, "answer_question")
        XCTAssertEqual(command["interaction_id"] as? String, "question-1")
        XCTAssertNil(command["answer"], "P1 answer fields are not nested")
        let inputs = try XCTUnwrap(command["inputs"] as? [String: [String: Any]])
        XCTAssertEqual(inputs["explanation"]?["kind"] as? String, "text")
        XCTAssertEqual(inputs["explanation"]?["value"] as? String, "Synthetic reason")
    }

    func testTypedConflictIsReturnedWithoutLegacyFallbackOrReplay() async throws {
        let operations = DomainOperations(.completed(status: 409, body: Data(#"{"code":"stale_revision","message":"Draft is stale","current_revision":9,"subject":"question-1"}"#.utf8)))
        do {
            _ = try await client(operations).executeDomainCommand(envelope)
            XCTFail("Expected conflict")
        } catch let error as DomainV2.DomainError {
            XCTAssertEqual(error.code, "stale_revision")
            XCTAssertEqual(error.currentRevision, 9)
            XCTAssertEqual(error.subject, "question-1")
        }
        let requests = await operations.recorded()
        XCTAssertEqual(requests.count, 1)
        XCTAssertEqual(requests[0].path, "/v2/domain/commands")
    }

    func testUnknownOutcomeDoesNotRetryOrTurnIntoSuccess() async throws {
        let operations = DomainOperations(.unknown)
        do {
            _ = try await client(operations).executeDomainCommand(envelope)
            XCTFail("Expected uncertain outcome")
        } catch { XCTAssertEqual(error as? ServiceAPIError, .outcomeUnknown) }
        let requests = await operations.recorded()
        XCTAssertEqual(requests.count, 1)
    }

    func testUnsupportedCapabilitiesRejectBeforeClientCanMutate() async throws {
        let operations = DomainOperations(.completed(status: 200, body: Data(#"{"protocol_version":3,"data_epoch":"epoch-3","revision":12}"#.utf8)))
        do {
            _ = try await client(operations).domainCapabilities()
            XCTFail("Expected protocol rejection")
        } catch let error as DomainV2.DomainError { XCTAssertEqual(error.code, "incompatible_protocol") }
        let requests = await operations.recorded()
        XCTAssertEqual(requests.map(\.path), ["/v2/domain/capabilities"])
    }

    func testDeliveryEvidenceAndCASResolutionUseExactEncryptedWire() async throws {
        let pageJSON = Data(#"{"protocol_version":2,"data_epoch":"epoch-2","revision":4,"attempts":[{"attempt_id":1,"message_id":1,"state":"uncertain","current_state":"uncertain","version":2,"reason":"daemon_restarted_after_claim","recipient_kind":"agent","task_id":"task-a","interaction_id":"question-a"}],"next_after":1,"has_more":false}"#.utf8)
        let reads = DomainOperations(.completed(status: 200, body: pageJSON))
        let page = try await client(reads).domainDeliveryAttempts(after: 0, epoch: "epoch-2")
        XCTAssertEqual(page.attempts.first?.interactionId, "question-a")
        XCTAssertEqual(page.attempts.first?.recipientKind, "agent")
        let readRequests = await reads.recorded()
        XCTAssertEqual(readRequests.map(\.path), ["/v2/domain/delivery-attempts?after=0"])

        let resultJSON = Data(#"{"attempt_id":1,"message_id":1,"version":3,"state":"retryable"}"#.utf8)
        let writes = DomainOperations(.completed(status: 200, body: resultJSON))
        let result = try await client(writes).resolveDomainDeliveryAttempt(1, epoch: "epoch-2",
            expectedVersion: 2, decision: .retry, operationID: "resolve-1")
        XCTAssertEqual(result.state, "retryable")
        let writeRequests = await writes.recorded()
        let request = try XCTUnwrap(writeRequests.first)
        XCTAssertEqual(request.path, "/v2/domain/delivery-attempts/1/resolve")
        XCTAssertEqual(request.operationID, "resolve-1")
        let body = try XCTUnwrap(request.body)
        let expected = try DomainV2.decode(DomainV2.ResolveDeliveryAttempt.self, from: body)
        XCTAssertEqual(expected.dataEpoch, "epoch-2")
        XCTAssertEqual(expected.expectedVersion, 2)
        XCTAssertEqual(expected.decision, .retry)
    }

    func testReadsRejectAnotherEpochAndBackwardsRevision() async throws {
        let page = DomainOperations(.completed(status: 200, body: Data(#"{"protocol_version":2,"data_epoch":"other","revision":4,"attempts":[],"next_after":0,"has_more":false}"#.utf8)))
        do {
            _ = try await client(page).domainDeliveryAttempts(after: 0, epoch: "epoch-2")
            XCTFail("Expected stale delivery evidence")
        } catch let error as DomainV2.DomainError {
            XCTAssertEqual(error.code, "incompatible_protocol")
        }

        let changes = DomainOperations(.completed(status: 200, body: Data(#"{"protocol_version":2,"data_epoch":"epoch-2","revision":6,"changed":true}"#.utf8)))
        do {
            _ = try await client(changes).domainChanges(epoch: "epoch-2", after: 7)
            XCTFail("Expected backwards revision rejection")
        } catch let error as DomainV2.DomainError {
            XCTAssertEqual(error.code, "incompatible_protocol")
        }
    }

    func testRustCanonicalProjectionsDecodeWithAuthoritativeHierarchyAndActions() throws {
        let env = ProcessInfo.processInfo.environment
        let directory: URL
        if let runfiles = env["TEST_SRCDIR"] {
            directory = URL(fileURLWithPath: runfiles)
                .appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
                .appendingPathComponent("apple/APIClient/Tests")
        } else {
            directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        }
        let bytes = try Data(contentsOf: directory.appendingPathComponent("domain-v2-wire-fixtures.json"))
        let fixtures = try JSONDecoder().decode([String: DomainV2.DomainProjection].self, from: bytes)
        let pr = try XCTUnwrap(fixtures["pr_question"])
        XCTAssertEqual(pr.protocolVersion, 2)
        XCTAssertEqual(pr.dataEpoch, "epoch")
        let prRoot = try XCTUnwrap(pr.inbox.first { item in
            if case .pullRequest = item.subject { return true }
            return false
        })
        XCTAssertTrue(prRoot.children.contains { child in
            if case .interaction(interactionId: "q") = child.subject { return true }
            return false
        }, "An explicitly linked question remains a child with its own ID")
        XCTAssertTrue(prRoot.children.contains { child in
            child.availableActions.contains { $0.kind == "authorize_merge" && $0.enabled }
        })
        let information = try XCTUnwrap(fixtures["information"])
        let info = try XCTUnwrap(information.inbox.first { $0.kind == "information" })
        XCTAssertEqual(info.availableActions.map(\.kind), ["mark_read"])
        XCTAssertEqual(fixtures["completed"]?.tasks.first?.state, .done)
    }

    func testDocumentAndPRTargetsPreserveVersionIdentity() throws {
        let json = Data(#"{"kind":"present_result","task_id":"task-1","interaction_id":"final-2","body":"Synthetic delivery","targets":[{"kind":"text","text":"Answer"},{"kind":"document_revision","document_id":"doc-1","revision_id":"revision-2"},{"kind":"pull_request","pr_id":"pr-1","head":"head-b"}],"is_final":true}"#.utf8)
        let command = try DomainV2.decode(DomainV2.Command.self, from: json)
        guard case let .presentResult(attachments, taskId, interactionId, body, targets, isFinal) = command else {
            return XCTFail("Wrong command kind")
        }
        XCTAssertEqual(attachments, [])
        XCTAssertEqual(taskId, "task-1")
        XCTAssertEqual(interactionId, "final-2")
        XCTAssertEqual(body, "Synthetic delivery")
        XCTAssertTrue(isFinal)
        XCTAssertEqual(targets, [.text(text: "Answer"), .documentRevision(documentId: "doc-1", revisionId: "revision-2"), .pullRequest(prId: "pr-1", head: "head-b")])
        XCTAssertEqual(try DomainV2.decode(DomainV2.Command.self, from: DomainV2.encode(command)), command)
    }

    func testDocumentContentChecksChunkSequenceAndDigestBeforeRendering() async throws {
        let first = Data(#"{"revision_id":"rev","content_digest":"b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9","version":"v1","data":"aGVsbG8g","offset":0,"total":11,"next_offset":6,"eof":false}"#.utf8)
        let second = Data(#"{"revision_id":"rev","content_digest":"b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9","version":"v1","data":"d29ybGQ","offset":6,"total":11,"next_offset":11,"eof":true}"#.utf8)
        let operations = DomainOperations([.completed(status: 200, body: first), .completed(status: 200, body: second)])
        let text = try await client(operations).domainDocumentContent("rev", expectedDigest: "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9")
        XCTAssertEqual(text, "hello world")
        let paths = await operations.recorded().map(\.path)
        XCTAssertEqual(paths, ["/v2/domain/document-revisions/rev/content?offset=0",
                               "/v2/domain/document-revisions/rev/content?offset=6&version=v1"])
    }

    func testDocumentContentRejectsChangedVersionWithoutReturningPartialText() async throws {
        let digest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        let first = Data("{\"revision_id\":\"rev\",\"content_digest\":\"\(digest)\",\"version\":\"v1\",\"data\":\"aGVsbG8g\",\"offset\":0,\"total\":11,\"next_offset\":6,\"eof\":false}".utf8)
        let second = Data("{\"revision_id\":\"rev\",\"content_digest\":\"\(digest)\",\"version\":\"v2\",\"data\":\"d29ybGQ\",\"offset\":6,\"total\":11,\"next_offset\":11,\"eof\":true}".utf8)
        let operations = DomainOperations([.completed(status: 200, body: first), .completed(status: 200, body: second)])
        do {
            _ = try await client(operations).domainDocumentContent("rev", expectedDigest: digest)
            XCTFail("Expected content_unavailable")
        } catch let error as DomainV2.DomainError { XCTAssertEqual(error.code, "content_unavailable") }
    }
}
