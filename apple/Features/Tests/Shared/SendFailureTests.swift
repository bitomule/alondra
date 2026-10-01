import AlondraFeatures
import Foundation
import Testing

@Suite struct SendFailureTests {
    private static let cloudflare = """
    <!DOCTYPE html>
    <!--[if lt IE 7]> <html class="no-js ie6 oldie" lang="en-US"> <![endif]-->
    <head><title>davidcollado.dev | 502: Bad gateway</title></head>
    """

    /// alondra/retry-queue, 24 Sep: gh refusing to merge the conflicting PR #54 came back as
    /// alondrad's own 502, which counted as "Alondra is down" and was retried forever.
    @Test(arguments: [
        (502, cloudflare, AlondraError.unreachable),
        (503, "", AlondraError.unreachable),
        (504, "<html>gateway timeout</html>", AlondraError.unreachable),
        (502, #"{"error":"X Pull request a/b#54 is not mergeable"}"#, AlondraError.server("X Pull request a/b#54 is not mergeable")),
        (502, #"{"error":"X Pull request a/b#54 is not mergeable","kind":"merge_refused"}"#, AlondraError.server("GitHub didn’t merge it: X Pull request a/b#54 is not mergeable")),
        (504, #"{"error":"gh pr merge did not finish within 90 s"}"#, AlondraError.server("gh pr merge did not finish within 90 s")),
        (422, #"{"error":"X Pull request a/b#54 is not mergeable","kind":"merge_refused"}"#, AlondraError.server("GitHub didn’t merge it: X Pull request a/b#54 is not mergeable")),
        (409, #"{"error":"https://github.com/a/b/pull/54 is no longer open","kind":"pr_not_open"}"#, AlondraError.server("This PR is no longer open")),
        (409, #"{"error":"question #7 is no longer open","kind":"question_closed"}"#, AlondraError.server("This question was already answered")),
        (409, #"{"error":"question #7 is no longer open"}"#, AlondraError.server("question #7 is no longer open")),
        (409, #"{"error":"the task is already done","kind":"task_refused"}"#, AlondraError.server("The task’s state does not allow that now")),
        (404, #"{"error":"task t-9 does not exist"}"#, AlondraError.server("Alondra no longer has it")),
        (500, cloudflare, AlondraError.unexpected(500)),
        (404, "", AlondraError.unexpected(404)),
        (422, "not json at all", AlondraError.unexpected(422)),
        (401, cloudflare, AlondraError.unauthorized),
    ])
    func onlyAlondrasOwnErrorsReachTheUserAsText(status: Int, body: String, expected: AlondraError) {
        let error = AlondraError.from(status: status, body: Data(body.utf8))
        #expect(error == expected)
        #expect(!(error.errorDescription ?? "").contains("<"), "no markup is ever shown")
    }

    @Test func aLongServerMessageIsCut() {
        let long = String(repeating: "x", count: 5000)
        let error = AlondraError.from(status: 409, body: Data(#"{"error":"\#(long)"}"#.utf8))
        #expect((error.errorDescription ?? "").count <= AlondraError.longestMessage + 1)
    }

    @Test(arguments: [
        (AlondraError.unreachable as any Error, true),
        (URLError(.timedOut), true),
        (URLError(.notConnectedToInternet), true),
        (URLError(.cannotConnectToHost), true),
        (AlondraError.from(status: 502, body: Data(#"{"error":"not mergeable","kind":"merge_refused"}"#.utf8)), false),
        (AlondraError.server("question #7 is no longer open"), false),
        (AlondraError.unexpected(500), false),
        (AlondraError.unauthorized, false),
        (URLError(.cancelled), false),
    ])
    func onlyFailuresWithoutAnAuthoritativeOutcomeAreTransient(error: any Error, transient: Bool) {
        #expect(error.isTransient == transient)
    }

}
