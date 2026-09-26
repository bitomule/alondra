import AlondraContracts
import AlondraDomain
import Foundation
import Testing

@Suite struct NewWorkSelectionTests {
    @Test func staleCatalogCannotChangeRequestAndRemovedChoiceIsNotSent() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let first = try decoder.decode(OptionsCatalog.self, from: Data(#"{"project":"A","options":[{"id":"effort","label":"Effort","kind":"choice","values":[{"id":"normal","label":"Normal"},{"id":"hard","label":"Hard"}],"default":"normal"}]}"#.utf8))
        let stale = try decoder.decode(OptionsCatalog.self, from: Data(#"{"project":"B","options":[{"id":"effort","label":"Effort","kind":"choice","values":[{"id":"light","label":"Light"}],"default":"light"}]}"#.utf8))
        let updated = try decoder.decode(OptionsCatalog.self, from: Data(#"{"project":"A","options":[{"id":"effort","label":"Effort","kind":"choice","values":[{"id":"normal","label":"Normal"}],"default":"normal"}]}"#.utf8))

        var selection = NewWorkSelection(project: "A")
        selection.apply(first)
        selection.set(try #require(selection.specs.first), to: .value("hard"))
        selection.apply(stale)
        #expect(selection.chosen["effort"] == .value("hard"))
        selection.apply(updated)

        let body = selection.body(text: "Do the work", attachments: ["upload-1"])
        let sent = try JSONSerialization.jsonObject(with: JSONEncoder().encode(body)) as? [String: Any]
        #expect(sent?["project"] as? String == "A")
        #expect(sent?["attachments"] as? [String] == ["upload-1"])
        #expect((sent?["options"] as? [String: Any])?.isEmpty == true)
    }

    @Test func changingProjectCannotSendPreviousProjectsOverrides() throws {
        let decoder = JSONDecoder()
        let catalog = try decoder.decode(OptionsCatalog.self, from: Data(#"{"project":"A","options":[{"id":"effort","label":"Effort","kind":"choice","values":[{"id":"normal","label":"Normal"},{"id":"hard","label":"Hard"}],"default":"normal"}]}"#.utf8))
        var selection = NewWorkSelection(project: "A")
        selection.apply(catalog)
        let oldSpec = try #require(selection.specs.first)
        selection.set(oldSpec, to: .value("hard"))
        selection.issue = "owner/repo#1"

        selection.project = "B"
        selection.set(oldSpec, to: .value("hard"))
        let sent = try JSONSerialization.jsonObject(with: JSONEncoder().encode(selection.body(text: "Next"))) as? [String: Any]
        #expect(sent?["project"] as? String == "B")
        #expect((sent?["options"] as? [String: Any])?.isEmpty == true)
        #expect(sent?["issue"] == nil)

        selection.select(issue: "owner/other#2", inferredProject: "C")
        let issueSent = try JSONSerialization.jsonObject(with: JSONEncoder().encode(selection.body(text: "Issue"))) as? [String: Any]
        #expect(issueSent?["project"] as? String == "C")
        #expect(issueSent?["issue"] as? String == "owner/other#2")
        #expect((issueSent?["options"] as? [String: Any])?.isEmpty == true)
    }
}
