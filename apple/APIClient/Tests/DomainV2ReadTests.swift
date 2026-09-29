import AlondraAPIClient
import AlondraContracts
import Foundation
import XCTest

private actor ReadServer: ServiceOperationTransport {
    private var requests: [ServiceRequest] = []
    private let respond: @Sendable (ServiceRequest) -> (Int, [String: Any])
    init(_ respond: @escaping @Sendable (ServiceRequest) -> (Int, [String: Any])) { self.respond = respond }
    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        requests.append(request)
        let (status, body) = respond(request)
        return .completed(status: status, body: try JSONSerialization.data(withJSONObject: body))
    }
    func paths() -> [String] { requests.map(\.path) }
}

private let envelope: [String: Any] = ["protocol_version": 2, "data_epoch": "epoch", "revision": 9]

final class DomainV2ReadTests: XCTestCase {
    private func client(_ server: ReadServer) -> AlondraAPIClient {
        AlondraAPIClient(serviceURL: URL(string: "https://service.example")!, deviceID: "phone", operations: server)
    }

    func testReadGrammarMatchesTheDaemon() {
        let valid = [
            "/v2/domain/summary", "/v2/domain/summary?include=inbox,tasks",
            "/v2/domain/inbox?filter=pending&cursor=eyJvIjo1fQ&limit=50", "/v2/domain/inbox/interaction:q-1",
            "/v2/domain/tasks?request=r1&agent=a_1", "/v2/domain/tasks/t1", "/v2/domain/tasks/t1/interactions?cursor=abc",
            "/v2/domain/interactions?scope=orchestrator", "/v2/domain/interactions/q1", "/v2/domain/interactions/q1/body?offset=0",
            "/v2/domain/requests/r1/body?offset=49152", "/v2/domain/pull-requests/p1", "/v2/domain/documents/d1",
            "/v2/domain/merge-authorizations", "/v2/domain/stack-readiness?limit=200", "/v2/domain/superseded-acceptances",
            "/v2/domain/inbox/interaction%3Aq", "/v2/domain/inbox/interaction%3aq",
        ]
        let invalid = [
            "/v2/domain", "/v2/domain/snapshot", "/v2/domain/summary?include=schedules", "/v2/domain/summary?include=inbox,inbox",
            "/v2/domain/inbox?filter=all", "/v2/domain/inbox?limit=0", "/v2/domain/inbox?limit=201", "/v2/domain/inbox?cursor=",
            "/v2/domain/inbox?cursor=a&cursor=b", "/v2/domain/tasks/t1?x=1", "/v2/domain/tasks/t%2F1", "/v2/domain/interactions",
            "/v2/domain/interactions/q1/body", "/v2/domain/agents/a1", "/v2/domain/tasks/t1/interactions/extra",
            "/v2/domain/inbox/a%2Fb", "/v2/domain/inbox/a%25", "/v2/domain/inbox/a%3", "/v2/domain/inbox/%2e%2e",
            "/v2/domain/interactions/q%3A1",
        ]
        for path in valid { XCTAssertTrue(DomainReadPath.isValid(path), path) }
        for path in invalid { XCTAssertFalse(DomainReadPath.isValid(path), path) }
    }

    func testAnInboxItemIsReadByItsPercentEncodedID() async throws {
        let server = ReadServer { _ in
            (200, envelope.merging(["root_id": "interaction:q", "item": [
                "id": "interaction:q", "kind": "question", "subject": ["kind": "interaction", "interaction_id": "q"],
                "title": "Q", "revision": 9, "order": 1, "pending": true, "children": [], "available_actions": []]]) { $1 })
        }
        let lookup = try await client(server).domainInboxItem("interaction:q")
        XCTAssertEqual(lookup.item.id, "interaction:q")
        let paths = await server.paths()
        XCTAssertEqual(paths, ["/v2/domain/inbox/interaction%3Aq"])
    }

    func testTheWholeProjectionIsNoLongerRead() async {
        let server = ReadServer { _ in (200, [:]) }
        do {
            _ = try await client(server).send(Endpoint<Data>.data(.get, path: ["v2", "domain"]))
            XCTFail("Reads go through sections")
        } catch { XCTAssertEqual(error as? ServiceAPIError, .invalidEndpoint) }
        let paths = await server.paths()
        XCTAssertTrue(paths.isEmpty)
    }

    func testAPageDecodesAndKeepsItsSectionToken() async throws {
        let server = ReadServer { _ in
            (200, envelope.merging(["section": "tasks", "section_revision": "00ff00ff00ff00ff", "total": 3,
                "items": [["id": "t1", "request_id": "r", "title": "T", "state": "running", "revision": 2, "started": true,
                           "review_prs": false, "dependencies": [], "documents": [:], "archived": false]],
                "next_cursor": "eyJpIjoidDEifQ", "truncated": []]) { $1 })
        }
        let page = try await client(server).domainPage(.tasks, of: DomainV2.Task.self, cursor: "eyJpIjoidDAifQ")
        XCTAssertEqual(page.sectionRevision, "00ff00ff00ff00ff")
        XCTAssertEqual(page.items.map(\.id.rawValue), ["t1"])
        XCTAssertEqual(page.nextCursor, "eyJpIjoidDEifQ")
        let paths = await server.paths()
        XCTAssertEqual(paths, ["/v2/domain/tasks?cursor=eyJpIjoidDAifQ"])
    }

    func testAPageFromAnotherSectionIsRejected() async {
        let server = ReadServer { _ in
            (200, envelope.merging(["section": "agents", "section_revision": "a", "total": 0, "items": [], "truncated": []]) { $1 })
        }
        do {
            _ = try await client(server).domainPage(.tasks, of: DomainV2.Task.self)
            XCTFail("A page of another section must not fill this one")
        } catch { XCTAssertEqual((error as? DomainV2.DomainError)?.code, "incompatible_protocol") }
    }

    func testACutInteractionIsCompletedFromItsBodyChunks() async throws {
        let whole = String(repeating: "é", count: 30_000) + "fin"
        let bytes = Array(whole.utf8)
        let server = ReadServer { request in
            if request.path == "/v2/domain/interactions/q1" {
                return (200, envelope.merging(["truncated": true, "item": [
                    "attachments": [], "id": "q1", "scope": ["kind": "task", "task_id": "t"], "author": ["kind": "agent", "agent_id": "a"],
                    "recipient": ["kind": "user"], "body": "éé", "blocking": true, "payload": ["kind": "question", "options": []],
                    "revision": 4, "order": 4]]) { $1 })
            }
            let offset = Int(request.path.components(separatedBy: "offset=").last ?? "") ?? -1
            let end = min(bytes.count, offset + 49_152)
            return (200, envelope.merging(["id": "q1", "offset": offset, "total_bytes": bytes.count,
                "data": String(decoding: bytes[offset..<end], as: UTF8.self), "has_more": end < bytes.count]) { $1 })
        }
        let item = try await client(server).domainInteraction(.init(rawValue: "q1"))
        XCTAssertEqual(item.item.body, whole)
        let paths = await server.paths()
        XCTAssertEqual(paths, ["/v2/domain/interactions/q1", "/v2/domain/interactions/q1/body?offset=0",
                               "/v2/domain/interactions/q1/body?offset=49152"])
    }

    func testABodyThatChangesSizeWhileReadIsRejected() async {
        let server = ReadServer { request in
            if request.path == "/v2/domain/requests/r1" {
                return (200, envelope.merging(["truncated": true, "item": ["id": "r1", "body": "ab", "closed": false]]) { $1 })
            }
            let first = request.path.hasSuffix("offset=0")
            return (200, envelope.merging(["id": "r1", "offset": first ? 0 : 3, "total_bytes": first ? 6 : 7,
                                           "data": "abc", "has_more": true]) { $1 })
        }
        do {
            _ = try await client(server).domainRequest(.init(rawValue: "r1"))
            XCTFail("Chunks of two different bodies must not be joined")
        } catch { XCTAssertEqual((error as? DomainV2.DomainError)?.code, "content_unavailable") }
    }
}
