import AlondraContract
import AlondraDesign
import Foundation
import Testing

@Suite struct UsageExhaustedWordingTests {
    private let now = Date(timeIntervalSince1970: 1_790_000_000)

    private func usage(scope: String = "this_mac", in seconds: TimeInterval, exhausted: UInt32 = 1, total: UInt32 = 1) -> UsageExhausted {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return UsageExhausted(
            scope: scope, until: formatter.string(from: now.addingTimeInterval(seconds)),
            macsExhausted: exhausted, macsTotal: total
        )
    }

    @Test func overAnHourShowsTheClockTime() {
        let wording = UsageExhaustedWording(usage(in: 3 * 3600), now: now)
        #expect(wording.when == .until(now.addingTimeInterval(3 * 3600)))
        #expect(wording.subject == .thisMac)
    }

    @Test func underAnHourCountsDownInMinutes() {
        #expect(UsageExhaustedWording(usage(in: 25 * 60), now: now).when == .minutesLeft(25))
        #expect(UsageExhaustedWording(usage(in: 10), now: now).when == .minutesLeft(1))
    }

    @Test func aResetThatAlreadyPassedFallsBackToTheClock() {
        #expect(UsageExhaustedWording(usage(in: -30), now: now).when == .until(now.addingTimeInterval(-30)))
    }

    @Test func manyMacsNameHowManyAreOut() {
        #expect(UsageExhaustedWording(usage(scope: "all_macs", in: 7200, exhausted: 2, total: 3), now: now).subject
            == .someMacs(exhausted: 2, total: 3))
        #expect(UsageExhaustedWording(usage(scope: "all_macs", in: 7200, exhausted: 3, total: 3), now: now).subject == .allMacs)
    }

    @Test func anUnreadableTimeIsShownAsReceived() {
        let odd = UsageExhausted(scope: "this_mac", until: "soon", macsExhausted: 1, macsTotal: 1)
        #expect(UsageExhaustedWording(odd, now: now).when == .untilText("soon"))
    }

    @Test func aPauseByTheUserSaysSoWhateverTheTimeLeft() throws {
        let paused = UsageExhausted(scope: "this_mac", until: "2026-10-06T00:00:00.000+02:00", macsExhausted: 1, macsTotal: 1, pausedByUser: true)
        let until = Date(timeIntervalSince1970: 1_791_237_600)
        let soon = UsageExhaustedWording(paused, now: until.addingTimeInterval(-1800))
        #expect(soon.subject == .pausedByUser)
        #expect(soon.when == .until(until))
        let decoded = try JSONDecoder().decode(UsageExhausted.self, from: Data(#"{"scope":"this_mac","until":"x","macs_exhausted":1,"macs_total":1,"paused_by_user":true}"#.utf8))
        #expect(decoded.pausedByUser)
    }

    @Test func theViewStatusDecodesTheFieldAndStaysAbsentWithout() throws {
        let base = #"{"version":"s1","summary":"x","badge":0,"counts":[],"gauges":[],"orchestrator":{"text":"idle","tone":"neutral"}"#
        let without = try JSONDecoder().decode(ViewStatus.self, from: Data((base + "}").utf8))
        #expect(without.usageExhausted == nil)
        let field = #","usage_exhausted":{"scope":"this_mac","until":"2026-10-05T14:30:00.000+02:00","macs_exhausted":1,"macs_total":2}}"#
        let with = try JSONDecoder().decode(ViewStatus.self, from: Data((base + field).utf8))
        #expect(with.usageExhausted == UsageExhausted(scope: "this_mac", until: "2026-10-05T14:30:00.000+02:00", macsExhausted: 1, macsTotal: 2))
        #expect(without.sessionWait == nil)
        let wait = #","session_wait":{"text":"Esperando una sesión de Claude","tone":"warning"}}"#
        let waiting = try JSONDecoder().decode(ViewStatus.self, from: Data((base + wait).utf8))
        #expect(waiting.sessionWait?.text == "Esperando una sesión de Claude")
        #expect(waiting.usageExhausted == nil)
    }
}
