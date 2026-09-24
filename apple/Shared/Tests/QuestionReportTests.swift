import AlondraShared
import Foundation
import Testing

@Suite struct QuestionReportTests {
    private func question(_ extra: String) throws -> Question {
        let json = """
        {"id":152,"session_id":"orq","title":"¿Lanzo un nodo?","options":[],"links":[],"attachments":[],"origin":"ask","status":"open","asked_at":2\(extra)}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Question.self, from: Data(json.utf8))
    }

    @Test func aQuestionCarriesTheReportItIsAbout() throws {
        let report = #","report":{"id":136,"session_id":"audit","title":"Auditoría: 13 cosas","body":"Detalle","links":[],"attachments":[],"kind":"document","created_at":1}"#
        #expect(try question(report).report?.title == "Auditoría: 13 cosas")
        #expect(try question("").report == nil)
    }
}
