import AlondraAPIClient
import AlondraContracts
import Foundation
import XCTest

private actor ScheduleOperations: ServiceOperationTransport {
    private var requests: [ServiceRequest] = []
    private let responses: [ServiceResponse]

    init(_ responses: [ServiceResponse]) { self.responses = responses }

    func perform(_ request: ServiceRequest) async throws -> ServiceResponse {
        requests.append(request)
        return responses[min(requests.count - 1, responses.count - 1)]
    }

    func waitForChange(afterSnapshotTick: UInt64, seconds: Int) async throws {
        XCTFail("Schedule operations must not poll")
    }

    func recorded() -> [ServiceRequest] { requests }
}

final class DomainV2ScheduleWireTests: XCTestCase {
    private func client(_ operations: ScheduleOperations) -> AlondraAPIClient {
        .init(serviceURL: URL(string: "https://service.example.invalid")!,
              deviceID: "synthetic-device", operations: operations)
    }

    private var scheduleJSON: String {
        #"{"id":"schedule-1","title":"Review","text":"Review project","project":null,"options":{"pr":"review","notify":true},"disallowed_tools":["Bash"],"frequency":"daily","hour":9,"minute":0,"weekday":null,"interval":null,"start_at":null,"enabled":true,"next_run":1234,"revision":1,"deleted_at":null,"status":"never_run","last_run":null,"last_request":null,"last_error":null,"last_run_id":null}"#
    }

    func testListAndDetailDecodeTheAuthoritativeScheduleProjection() async throws {
        let list = Data((#"{"protocol_version":2,"data_epoch":"epoch-1","revision":7,"schedules":["#
            + scheduleJSON + "]}").utf8)
        let detail = Data((#"{"protocol_version":2,"data_epoch":"epoch-1","revision":7,"schedule":"#
            + scheduleJSON + "}").utf8)
        let operations = ScheduleOperations([.completed(status: 200, body: list), .completed(status: 200, body: detail)])
        let api = client(operations)
        let page = try await api.domainSchedules()
        XCTAssertEqual(page.protocolVersion, 2)
        XCTAssertEqual(page.dataEpoch, "epoch-1")
        XCTAssertEqual(page.schedules.first?.id, "schedule-1")
        XCTAssertEqual(page.schedules.first?.status, .neverRun)
        XCTAssertEqual(page.schedules.first?.nextRun, 1234)
        XCTAssertEqual(page.schedules.first?.options["notify"], .toggle(true))
        let item = try await api.domainSchedule("schedule-1")
        XCTAssertEqual(item.schedule.id, "schedule-1")
        XCTAssertEqual(item.revision, 7)
        let requests = await operations.recorded()
        XCTAssertEqual(requests.map(\.path), ["/v2/domain/schedules", "/v2/domain/schedules/schedule-1"])
        XCTAssertTrue(requests.allSatisfy { $0.method == "GET" && $0.body == nil })
    }

    func testCreateUpdateDeleteKeepWireShapeAndCallerOperationIdentity() async throws {
        let responses: [ServiceResponse] = [
            .completed(status: 201, body: Data(#"{"id":"schedule-1","revision":1}"#.utf8)),
            .completed(status: 200, body: Data(#"{"id":"schedule-1","revision":2}"#.utf8)),
            .completed(status: 200, body: Data(#"{"ok":true}"#.utf8)),
        ]
        let operations = ScheduleOperations(responses)
        let api = client(operations)
        var draft = DomainV2.ScheduleDraft(title: "Review", text: "Review project", project: nil,
            options: ["pr": .value("review"), "notify": .toggle(true)], disallowedTools: ["Bash"],
            frequency: .daily, hour: 9, minute: 0)
        let created = try await api.createDomainSchedule(draft, operationID: "create-1")
        XCTAssertEqual(created.id, "schedule-1")
        draft.revision = 1
        let changed = try await api.updateDomainSchedule("schedule-1", draft: draft, operationID: "update-1")
        XCTAssertEqual(changed.revision, 2)
        try await api.deleteDomainSchedule("schedule-1", operationID: "delete-1")
        let requests = await operations.recorded()
        XCTAssertEqual(requests.map(\.method), ["POST", "PUT", "DELETE"])
        XCTAssertEqual(requests.map(\.path), ["/v2/domain/schedules", "/v2/domain/schedules/schedule-1",
                                             "/v2/domain/schedules/schedule-1"])
        XCTAssertEqual(requests.map(\.operationID), ["create-1", "update-1", "delete-1"])
        XCTAssertNil(requests[2].body)
        for request in requests.prefix(2) {
            let body = try XCTUnwrap(request.body)
            let value = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
            XCTAssertNil(value["next_run"], "The Mac owns due-time calculation")
            XCTAssertEqual(value["disallowed_tools"] as? [String], ["Bash"])
            XCTAssertNil(value["start_at"])
            let options = try XCTUnwrap(value["options"] as? [String: Any])
            XCTAssertEqual(options["pr"] as? String, "review")
            XCTAssertEqual(options["notify"] as? Bool, true)
        }
        XCTAssertEqual(try DomainV2.decode(DomainV2.ScheduleWriteReceipt.self,
            from: Data(#"{"id":"schedule-1","revision":2}"#.utf8)), changed)
    }

    func testManualRunKeepsEmptyBodyAndLaunchUnavailableAsTypedError() async throws {
        let operations = ScheduleOperations([.completed(status: 503,
            body: Data(#"{"code":"launch_unavailable","message":"runner pending","current_revision":null,"subject":null}"#.utf8))])
        do {
            _ = try await client(operations).requestDomainScheduleRun("schedule-1", operationID: "run-1")
            XCTFail("P3 cannot claim or launch a Schedule")
        } catch let error as DomainV2.DomainError {
            XCTAssertEqual(error.code, "launch_unavailable")
        }
        let recorded = await operations.recorded()
        let request = try XCTUnwrap(recorded.first)
        XCTAssertEqual(request.method, "POST")
        XCTAssertEqual(request.path, "/v2/domain/schedules/schedule-1/run")
        XCTAssertEqual(request.operationID, "run-1")
        XCTAssertNil(request.body)
    }

    func testScheduleRouteAdmissionRejectsExtraSegmentsAndBodies() async throws {
        let operations = ScheduleOperations([.completed(status: 200, body: Data("{}".utf8))])
        let api = client(operations)
        for endpoint in [
            Endpoint<Data>.data(.get, path: ["v2", "domain", "schedules", "id", "run"]),
            Endpoint<Data>.data(.post, path: ["v2", "domain", "schedules", "id", "run"], body: Data("{}".utf8)),
            Endpoint<Data>.data(.delete, path: ["v2", "domain", "schedules", "id"], body: Data("{}".utf8)),
            Endpoint<Data>.data(.get, path: ["v2", "domain", "schedules", "id", "extra"]),
            Endpoint<Data>.data(.get, path: ["v2", "domain", "schedules", "bad%2Fid"]),
            Endpoint<Data>.data(.get, path: ["v2", "domain", "schedules"], query: [("extra", "1")]),
        ] {
            do {
                _ = try await api.send(endpoint, operationID: "rejected-route")
                XCTFail("Rejected route was dispatched")
            } catch ServiceAPIError.invalidEndpoint { }
        }
        let recorded = await operations.recorded()
        XCTAssertTrue(recorded.isEmpty)
    }
}
