import AlondraAPIClient
import AlondraContracts
import Foundation
import XCTest

private actor DomainOperations: ServiceOperationTransport {
    var requests: [ServiceRequest] = []
    let response: ServiceResponse
    init(_ response: ServiceResponse) { self.response = response }
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        requests.append(request)
        return response
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
              command: .answerQuestion(interactionId: "question-1", optionId: "other",
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

    func testDocumentAndPRTargetsPreserveVersionIdentity() throws {
        let json = Data(#"{"kind":"present_result","task_id":"task-1","interaction_id":"final-2","body":"Synthetic delivery","targets":[{"kind":"text","text":"Answer"},{"kind":"document_revision","document_id":"doc-1","revision_id":"revision-2"},{"kind":"pull_request","pr_id":"pr-1","head":"head-b"}],"is_final":true}"#.utf8)
        let command = try DomainV2.decode(DomainV2.Command.self, from: json)
        guard case let .presentResult(taskId, interactionId, body, targets, isFinal) = command else {
            return XCTFail("Wrong command kind")
        }
        XCTAssertEqual(taskId, "task-1")
        XCTAssertEqual(interactionId, "final-2")
        XCTAssertEqual(body, "Synthetic delivery")
        XCTAssertTrue(isFinal)
        XCTAssertEqual(targets, [.text(text: "Answer"), .documentRevision(documentId: "doc-1", revisionId: "revision-2"), .pullRequest(prId: "pr-1", head: "head-b")])
        XCTAssertEqual(try DomainV2.decode(DomainV2.Command.self, from: DomainV2.encode(command)), command)
    }
}
