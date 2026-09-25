import Foundation
import Testing
import AlondraShared

@Test func scheduledTaskWireFormatCarriesRunAndRevision() throws {
    let data = Data(#"{"id":"abc","title":"Weekly review","text":"Review it","project":null,"options":{"pr":"auto_merge"},"frequency":"weekly","hour":9,"minute":30,"weekday":1,"enabled":true,"next_run":1790321400000,"last_run":1790235000000,"last_request":"r-1","last_state":"in_progress","last_nodes":["node-1"],"last_error":null,"status":"sent","revision":7}"#.utf8)
    let decoder = JSONDecoder()
    decoder.keyDecodingStrategy = .convertFromSnakeCase
    let schedule = try decoder.decode(ScheduledTask.self, from: data)
    #expect(schedule.lastState == "in_progress")
    #expect(schedule.lastNodes == ["node-1"])
    #expect(schedule.options["pr"] == .value("auto_merge"))
    let draft = ScheduledTaskDraft(schedule)
    let encoded = try JSONEncoder().encode(draft)
    let body = try #require(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
    #expect(body["revision"] as? Int == 7)
    #expect(body["weekday"] as? Int == 1)
    #expect(body["enabled"] as? Bool == true)
}
