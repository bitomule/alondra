import AlondraContracts
import AlondraDomain
import Foundation
import Testing

@Suite struct IssuesTests {
    /// The shape `GET /v1/issues` serves.
    static let served = #"""
    {"fetched_at": 1790200000000, "owners": ["me", "team"], "issues": [
      {"repo": "me/App", "number": 1, "title": "Crash on launch", "url": "https://github.com/me/App/issues/1", "author": "ana",
       "association": "OWNER", "labels": ["bug"], "updated_at": "2026-09-24T10:00:00Z", "comments": 2},
      {"repo": "team/Lib", "number": 7, "title": "Slow sync", "url": "https://github.com/team/Lib/issues/7", "labels": [], "comments": 0}
    ]}
    """#

    private func list(_ json: String = served) throws -> IssueList {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(IssueList.self, from: Data(json.utf8))
    }

    @Test func theListDecodesWhatAlondradServes() throws {
        let l = try list()
        #expect(l.fetchedAt == 1_790_200_000_000)
        #expect(l.issues.map(\.reference) == ["me/App#1", "team/Lib#7"])
        #expect(l.issues[0].labels == ["bug"])
        #expect(l.issues[1].author == nil)
        #expect(try list(#"{"issues": [], "error": "gh: not logged in"}"#).error == "gh: not logged in")
    }

    @Test(arguments: [
        ("crash", ["me/App#1"]),
        ("", ["me/App#1", "team/Lib#7"]),
        ("app bug", ["me/App#1"]),
        ("#7", ["team/Lib#7"]),
        ("SLOW", ["team/Lib#7"]),
        ("crash sync", []),
    ])
    func typingFiltersByEveryWord(_ query: String, _ shown: [String]) throws {
        #expect(try list().issues.filter { $0.matches(query) }.map(\.reference) == shown)
    }

    @Test(arguments: [
        ("https://github.com/me/App/issues/12", "me/App#12"),
        ("  https://github.com/me/App/issues/12#issuecomment-3 ", "me/App#12"),
        ("https://github.com/me/App/issues/12.", "me/App#12"),
        ("me/App#12", "me/App#12"),
        ("https://github.com/me/App/pull/12", nil),
        ("me#12", nil),
        ("a/b/c#1", nil),
        ("crash", nil),
    ] as [(String, String?)])
    func aPastedLinkNamesItsIssue(_ text: String, _ reference: String?) {
        #expect(IssueSummary.reference(in: text) == reference)
    }

    @Test func aPastedIssueIsOfferedOnlyWhenTheListLacksIt() throws {
        let issues = try list().issues
        #expect(IssueSummary.pasted(in: "https://github.com/me/Other/issues/3", besides: issues) == "me/Other#3")
        #expect(IssueSummary.pasted(in: "https://github.com/me/App/issues/1", besides: issues) == nil)
        #expect(IssueSummary.pasted(in: "crash", besides: issues) == nil)
    }

    @Test(arguments: [
        ("me/App#1", ["App", "Lib"], "App"),
        ("team/lib#7", ["App", "Lib"], "Lib"),
        ("me/Other#3", ["App"], nil),
        (nil, ["App"], nil),
    ] as [(String?, [String], String?)])
    func anIssuePicksTheProjectNamedLikeItsRepo(_ issue: String?, _ projects: [String], _ project: String?) {
        #expect(IssueSummary.project(of: issue, among: projects) == project)
    }

    private func intent(_ body: NewRequestBody) -> DomainIntent {
        .createRequest(body: body.text, project: body.project, options: body.options, issue: body.issue, attachments: body.attachments)
    }

    @Test func theRequestCarriesTheIssueWithTheOptionsAndAttachments() throws {
        var options = NewWorkSelection(project: "App")
        options.issue = "me/App#1"
        let file = DomainV2.AttachmentRef(id: "f1", name: "f", contentDigest: "d", contentReference: "r", bytes: 1)
        let workspace = DomainWorkspace(DomainV2.DomainProjection(protocolVersion: 2, dataEpoch: "e", revision: 4, inbox: [],
            counts: DomainV2.InboxCounts(roots: 0, pending: 0, unreadInformation: 0),
            tasks: [], agents: [], requests: [], interactions: [], pullRequests: [], documents: [], documentRevisions: [],
            documentReviews: [], mergeAuthorizations: [], stackReadiness: []))
        let envelope = try DomainCommandFactory().envelope(for: intent(options.body(text: "Arréglalo", attachments: [file])),
                                                           in: workspace, commandID: "c-1", newID: { "r-1" })
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        let command = String(decoding: try encoder.encode(envelope.command), as: UTF8.self)
        #expect(command.contains(#""issue":"me\/App#1""#))
        #expect(command.contains(#""project":"App""#))
        #expect(command.contains(#""id":"f1""#))
        options.issue = nil
        let without = try DomainCommandFactory().envelope(for: intent(options.body(text: "x")), in: workspace,
                                                          commandID: "c-2", newID: { "r-2" })
        #expect(!String(decoding: try encoder.encode(without.command), as: UTF8.self).contains("issue"), "no issue, no key")
    }
}
