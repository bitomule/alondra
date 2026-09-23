import AlondraShared
import Foundation
import Testing

@Suite struct UsageTests {
    private func usage(_ json: String) throws -> Usage {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Usage.self, from: Data(json.utf8))
    }

    @Test func todayIsShownAgainstItsSliceOfTheWeek() throws {
        let u = try usage(#"{"week_percent":53,"week_resets_at":1790553600000,"today_spent":41,"today_share":17.6,"per_day_left":9.4,"over_pace":true}"#)
        #expect(u.summary == "week 53% · today 41/18%")
        #expect(u.isOverPace)
    }

    @Test func aDayWithoutARecordShowsOnlyTheWeek() throws {
        let u = try usage(#"{"week_percent":53,"week_resets_at":1790553600000,"per_day_left":9.4}"#)
        #expect(u.summary == "week 53%")
        #expect(!u.isOverPace)
    }
}
