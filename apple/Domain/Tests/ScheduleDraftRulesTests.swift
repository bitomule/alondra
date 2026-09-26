import AlondraContracts
import AlondraDomain
import Testing

struct ScheduleDraftRulesTests {
    @Test
    func invalidCatalogOverridesAreRemovedBeforeSave() {
        var draft = ScheduledTaskDraft()
        draft.title = "Run review"
        draft.text = "Review"
        draft.frequency = "weekly"
        draft.weekday = 7
        #expect(!ScheduleDraftRules.canSave(draft))
        draft.weekday = 1
        #expect(ScheduleDraftRules.canSave(draft))
        #expect(ScheduleDraftRules.validOverrides(["removed": .value("x")], in: []).isEmpty)
    }

}
