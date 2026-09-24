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

    private static func report(_ id: Int, earlier: String = "") -> String {
        #"{"id":\#(id),"session_id":"s","title":"R\#(id)","links":[],"attachments":[],"kind":"final","created_at":\#(id)\#(earlier)}"#
    }

    struct Case: Sendable, CustomTestStringConvertible {
        let name: String
        let extra: String
        let about: [UInt64]
        var testDescription: String { name }
    }

    @Test(arguments: [
        Case(name: "every report the question takes in, oldest first", extra: #","report":\#(report(2)),"reports":[\#(report(1)),\#(report(2))]"#, about: [1, 2]),
        Case(name: "an older alondrad sends only one report", extra: #","report":\#(report(2))"#, about: [2]),
        Case(name: "no report", extra: "", about: []),
    ])
    func aQuestionShowsEveryReportItTakesIn(_ c: Case) throws {
        #expect(try question(c.extra).aboutReports.map(\.id) == c.about)
    }

    @Test func aReportCardCarriesTheEarlierReportsOfItsDelivery() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let json = Self.report(2, earlier: #","earlier":[\#(Self.report(1))]"#)
        let card = try decoder.decode(Report.self, from: Data(json.utf8))
        #expect(card.earlier?.map(\.id) == [1])
        #expect(try decoder.decode(Report.self, from: Data(Self.report(1).utf8)).earlier == nil)
    }
}
