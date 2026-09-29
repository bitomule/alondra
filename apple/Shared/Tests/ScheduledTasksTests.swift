import Foundation
import Testing
import AlondraShared

@Test func scheduledTaskWireFormatCarriesRunAndRevision() throws {
    let data = Data(#"{"id":"abc","title":"Weekly review","text":"Review it","project":null,"options":{"pr":"auto_merge"},"disallowed_tools":["mcp__revenuecat__create-*"],"frequency":"every_months","hour":9,"minute":30,"weekday":1,"interval":2,"start_at":1790321400000,"enabled":true,"next_run":1790321400000,"last_run":1790235000000,"last_request":"r-1","last_error":null,"status":"sent","revision":7}"#.utf8)
    let schedule = ScheduledTask(try DomainV2.decode(DomainV2.Schedule.self, from: data))
    #expect(schedule.lastRequest == "r-1")
    #expect(schedule.status == "sent")
    #expect(schedule.options["pr"] == .value("auto_merge"))
    #expect(schedule.disallowedTools == ["mcp__revenuecat__create-*"])
    #expect(schedule.interval == 2)
    #expect(schedule.startAt == 1790321400000)
    let draft = try #require(DomainV2.ScheduleDraft(ScheduledTaskDraft(schedule)))
    let encoded = try JSONEncoder().encode(draft)
    let body = try #require(JSONSerialization.jsonObject(with: encoded) as? [String: Any])
    #expect(body["revision"] as? Int == 7)
    #expect(body["weekday"] as? Int == 1)
    #expect(body["interval"] as? Int == 2)
    #expect(body["start_at"] as? Int64 == 1790321400000)
    #expect(body["enabled"] as? Bool == true)
    #expect(body["disallowed_tools"] as? [String] == ["mcp__revenuecat__create-*"])
}
