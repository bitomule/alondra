import AlondraContract
import Testing
@testable import AlondraDesign

@Suite struct SheetSpeechTests {
    private func scenario(mark: SheetChange, result: SheetResult? = nil) -> SheetScenario {
        SheetScenario(id: "s/a", title: "Part of an order", kind: "judgment on captures", mark: SheetMark(change: mark, label: "new"), lanes: [], result: result)
    }

    @Test func aScenarioSaysItsTitleItsKindAndWhatChanged() {
        #expect(SheetSpeech.scenario(scenario(mark: .new)) == "Part of an order, judgment on captures, new")
    }

    @Test func aScenarioThatDidNotChangeDoesNotSayIt() {
        #expect(SheetSpeech.scenario(scenario(mark: .unchanged)) == "Part of an order, judgment on captures")
    }

    @Test func aValidatedScenarioSaysItsState() {
        let result = SheetResult(state: SheetLine(icon: .check, tone: .success, text: "Validated"))
        #expect(SheetSpeech.scenario(scenario(mark: .new, result: result)) == "Part of an order, judgment on captures, new, Validated")
    }

    @Test func aSpecAndADecisionSayTheirTitleAndWhatChanged() {
        let spec = SpecSheet(id: "s", spec: "billing", title: "Partial refunds", mark: SheetMark(change: .changed, label: "changed"), scenarios: [])
        #expect(SheetSpeech.spec(spec) == "Partial refunds, changed")
        let decision = DecisionSheet(id: "d", decision: "limit", title: "A limit", status: "accepted", sections: [])
        #expect(SheetSpeech.decision(decision) == "A limit, accepted")
    }
}
