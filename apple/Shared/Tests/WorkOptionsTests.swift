import AlondraShared
import Foundation
import Testing

@Suite struct WorkOptionsTests {
    /// The shape `GET /v1/options` serves: the two built-ins, then the user's own.
    static let served = #"""
    {"project": null, "error": null, "options": [
      {"id": "pr", "label": "PR handling", "kind": "choice", "builtin": true, "default": "review",
       "values": [{"id": "review", "label": "review each PR"}, {"id": "auto_merge", "label": "auto-merge"}]},
      {"id": "effort", "label": "Effort", "kind": "choice", "builtin": true, "default": "normal",
       "values": [{"id": "auto", "label": "auto"}, {"id": "light", "label": "light"}, {"id": "normal", "label": "normal"}, {"id": "hard", "label": "hard"}]},
      {"id": "how_far", "label": "Hasta dónde", "kind": "choice", "builtin": false, "default": "merge",
       "values": [{"id": "merge", "label": "merge"}, {"id": "beta", "label": "beta", "description": "Sube una beta"}]},
      {"id": "device", "label": "Probar en mi iPhone", "kind": "toggle", "builtin": false, "default": false, "values": []}
    ]}
    """#

    private func catalog(_ json: String = served) throws -> OptionsCatalog {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(OptionsCatalog.self, from: Data(json.utf8))
    }

    private func loaded() throws -> WorkOptions {
        var options = WorkOptions()
        options.apply(try catalog())
        return options
    }

    private func spec(_ id: String, in options: WorkOptions) -> OptionSpec {
        options.specs.first { $0.id == id }!
    }

    @Test func theCatalogDecodesChoicesAndToggles() throws {
        let options = try loaded()
        #expect(options.specs.map(\.id) == ["pr", "effort", "how_far", "device"])
        #expect(spec("device", in: options).default == .toggle(false))
        #expect(spec("how_far", in: options).default == .value("merge"))
        #expect(spec("device", in: options).kind == .toggle)
    }

    @Test(arguments: [
        ("nothing touched", [], [String: OptionChoice]()),
        ("a choice changed", [("pr", OptionChoice.value("auto_merge"))], ["pr": .value("auto_merge")]),
        ("a toggle on", [("device", .toggle(true))], ["device": .toggle(true)]),
        ("changed and back", [("how_far", .value("beta")), ("how_far", .value("merge"))], [:]),
        ("several", [("effort", .value("hard")), ("device", .toggle(true))], ["effort": .value("hard"), "device": .toggle(true)]),
    ])
    func onlyWhatDiffersFromTheDefaultIsSent(_ what: String, _ steps: [(String, OptionChoice)], _ sent: [String: OptionChoice]) throws {
        var options = try loaded()
        for (id, choice) in steps {
            options.set(spec(id, in: options), to: choice)
        }
        #expect(options.body(text: "x").options == sent, "\(what)")
    }

    @Test func theBodyIsTheJSONAlondradReads() throws {
        var options = try loaded()
        options.project = "Lumo"
        options.set(spec("device", in: options), to: .toggle(true))
        options.set(spec("pr", in: options), to: .value("auto_merge"))
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        var body = options.body(text: "Arregla el crash")
        body.attachments = ["a1b2"]
        let json = String(decoding: try encoder.encode(body), as: UTF8.self)
        #expect(json == #"{"attachments":["a1b2"],"options":{"device":true,"pr":"auto_merge"},"project":"Lumo","text":"Arregla el crash"}"#)
    }

    @Test func aNewCatalogDropsChoicesItNoLongerOffersAndIgnoresAnotherProject() throws {
        var options = try loaded()
        options.set(spec("how_far", in: options), to: .value("beta"))
        options.set(spec("device", in: options), to: .toggle(true))
        let fewer = Self.served.replacingOccurrences(of: #"{"id": "beta", "label": "beta", "description": "Sube una beta"}"#, with: #"{"id": "store", "label": "App Store"}"#)
        options.apply(try catalog(fewer))
        #expect(options.body(text: "x").options == ["device": .toggle(true)])
        let other = Self.served.replacingOccurrences(of: #""project": null"#, with: #""project": "Other""#)
        options.apply(try catalog(other.replacingOccurrences(of: "Probar en mi iPhone", with: "Changed")))
        #expect(spec("device", in: options).label == "Probar en mi iPhone", "an answer for a project no longer chosen is dropped")
    }

    @Test func builtInsAreWordedByTheAppAndTheUsersOptionsKeepTheirWords() throws {
        let options = try loaded()
        #expect(options.summary == "PRs: Review each one · Effort: Normal · Hasta dónde: merge · Probar en mi iPhone: No")
    }

    @Test func eachValueCarriesTheLineThatExplainsIt() throws {
        let options = try loaded()
        let effort = spec("effort", in: options)
        #expect(effort.choices.map { effort.title(of: $0) } == ["Auto", "Light", "Normal", "Hard"])
        #expect(effort.description(of: .value("auto")) == "The orchestrator picks the model and effort from the task")
        let far = spec("how_far", in: options)
        #expect(far.description(of: .value("beta")) == "Sube una beta", "the user's own line from options.toml")
        #expect(far.description(of: .value("merge")) == nil)
        #expect(spec("device", in: options).choices == [.toggle(true), .toggle(false)])
    }

    @Test(arguments: [
        ("", ["Recent", "Older", "Here"]),
        ("re", ["Recent", "Here"]),
        ("OLD", ["Older"]),
        ("zz", []),
    ])
    func theProjectListFiltersInRankedOrder(_ query: String, _ shown: [String]) {
        #expect(ProjectList.filter(["Recent", "Older", "Here"], query: query) == shown)
    }
}
