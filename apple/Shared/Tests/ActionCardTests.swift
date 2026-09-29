import AlondraShared
import Foundation
import Testing

@Suite struct ActionCardTests {
    @Test func theActionOptionsKeepTheLabelsTheServerMatches() {
        #expect(AnswerOption.done == "Done")
        #expect(AnswerOption.cannot == "I can't / explain")
    }

    @Test func aCardCarriesTheOpeningLineOfTheRequestItComesFrom() {
        let long = String(repeating: "word ", count: 80)
        let request = DomainV2.Request(id: "r-1", body: "Ship Lumo 4.5 \(long)\n\nDetails", closed: false)
        let headline = request.headline ?? ""
        #expect(headline.hasPrefix("Ship Lumo 4.5 word"))
        #expect(headline.hasSuffix("…"))
        #expect(headline.count <= 90)
        #expect(DomainV2.Request(id: "r-2", body: "  \n", closed: false).headline == nil)
    }
}
