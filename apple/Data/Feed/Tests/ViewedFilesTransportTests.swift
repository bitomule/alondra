import AlondraContract
@testable import AlondraTransport
@testable import AlondraFeed
import Foundation
import XCTest

final class ViewedFilesTransportTests: XCTestCase {
    private let head = String(repeating: "a", count: 40)
    private let fetch = "/pull-requests/github.com%2Fme%2Fapp%237/diff"

    private func reference() throws -> ContentRef {
        try JSONDecoder().decode(ContentRef.self, from: Data(#"{"kind":"diff","title":"Changes","size":0,"digest":"\#(head)","fetch":"\#(fetch)","commentable":false}"#.utf8))
    }

    private func api(_ answer: APIResponse) -> (AlondraAPIClient, Operations) {
        let operations = Operations(answer: answer)
        return (AlondraAPIClient(serviceURL: URL(string: "https://example.test")!, deviceID: "phone-1", operations: operations), operations)
    }

    func testTheDiffBringsGitHubsViewedBoxesOfItsHead() async throws {
        let body = try JSONSerialization.data(withJSONObject: [
            "pr_id": "github.com/me/app#7", "head": head, "diff": "diff --git a/a b/a\n", "truncated": false,
            "files": [["path": "a.swift", "viewed": "viewed"], ["path": "b.swift", "viewed": "unviewed"],
                      ["path": "c.swift", "viewed": "changed"], ["path": "d.swift", "viewed": "someday"]],
        ])
        let (api, _) = api(.completed(status: 200, body: body))

        let fetched = try await api.fetchDiff(try reference())

        XCTAssertEqual(fetched.marks, FileMarks(prID: "github.com/me/app#7", head: head,
                                                files: ["a.swift": .viewed, "b.swift": .unviewed, "c.swift": .changed]))
    }

    func testADiffWithoutBoxesHasNoMarks() async throws {
        let body = try JSONSerialization.data(withJSONObject: ["pr_id": "github.com/me/app#7", "head": head, "diff": "", "truncated": false])
        let (api, _) = api(.completed(status: 200, body: body))

        let fetched = try await api.fetchDiff(try reference())

        XCTAssertNil(fetched.marks)
    }

    func testABoxIsSavedForTheHeadTheReaderSaw() async throws {
        let body = try JSONSerialization.data(withJSONObject: ["pr_id": "github.com/me/app#7", "head": head, "path": "a.swift", "viewed": "viewed"])
        let (api, operations) = api(.completed(status: 200, body: body))
        let marks = FileMarks(prID: "github.com/me/app#7", head: head, files: ["a.swift": .unviewed])

        let viewed = try await api.setFileViewed(true, path: "a.swift", marks: marks)

        XCTAssertEqual(viewed, .viewed)
        let sent = await operations.requests
        XCTAssertEqual(sent.map(\.method), ["POST"])
        XCTAssertEqual(sent.map(\.path), ["/pull-requests/github.com%2Fme%2Fapp%237/viewed-files"])
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: try XCTUnwrap(sent.first?.body)) as? [String: AnyHashable])
        XCTAssertEqual(json, ["path": "a.swift", "viewed": true, "head": head])
        XCTAssertTrue(EnvelopeRequestPolicy.validViewsPath(method: "POST", path: sent[0].path, hasBody: true), "the relay carries it")
    }

    func testAnOldHeadIsStaleAndAFileOutsideThePullRequestIsRefused() async throws {
        let marks = FileMarks(prID: "github.com/me/app#7", head: head, files: ["a.swift": .unviewed])
        let stale = Data(#"{"code":"stale_head","message":"registered PR head changed"}"#.utf8)
        let refused = Data(#"{"code":"invalid_input","message":"the file is not in the pull request"}"#.utf8)
        for (status, body, expected) in [(409, stale, FileMarkError.stale), (422, refused, .refused)] {
            let (api, _) = api(.completed(status: status, body: body))
            do {
                _ = try await api.setFileViewed(true, path: "a.swift", marks: marks)
                XCTFail("\(status) saved the box")
            } catch let error as FileMarkError {
                XCTAssertEqual(error, expected)
            }
        }
    }
}

private actor Operations: OperationAPI {
    let answer: APIResponse
    private(set) var requests: [APIRequest] = []
    init(answer: APIResponse) { self.answer = answer }

    func perform(_ request: APIRequest) async throws -> APIResponse {
        requests.append(request)
        return answer
    }
}
