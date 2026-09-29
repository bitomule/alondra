import AlondraContracts
import AlondraDomain
import Foundation
import Testing

struct ScheduleDraftRulesTests {
    @Test
    func weeklyDraftsNeedAValidWeekday() {
        var draft = ScheduledTaskDraft()
        draft.title = "Run review"
        draft.text = "Review"
        draft.frequency = "weekly"
        draft.weekday = 7
        #expect(!ScheduleDraftRules.canSave(draft))
        draft.weekday = 1
        #expect(ScheduleDraftRules.canSave(draft))
    }

    @Test
    func choicesAreKeptWhenThereIsNoCatalogAndFilteredWhenThereIsOne() throws {
        #expect(ScheduleDraftRules.validOverrides(["kept": .value("x")], in: []) == ["kept": .value("x")])
        let spec = try JSONDecoder().decode(OptionSpec.self, from: Data(#"{"id":"notify","label":"Notify","kind":"toggle","default":false,"values":[]}"#.utf8))
        #expect(ScheduleDraftRules.validOverrides(["notify": .toggle(true), "removed": .value("x")], in: [spec]) == ["notify": .toggle(true)])
    }
}
