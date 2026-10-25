import Foundation

/// A pull request's diff with a file of every kind: modified, added, deleted, renamed with and
/// without changes, binary, and a generated lockfile that comes folded.
enum DiffFixture {
    static func text() throws -> String {
        let environment = ProcessInfo.processInfo.environment
        let root = URL(fileURLWithPath: environment["TEST_SRCDIR"] ?? "")
            .appendingPathComponent(environment["TEST_WORKSPACE"] ?? "_main")
        let shared = try String(contentsOf: root.appendingPathComponent("fixtures/diffs/pull-request.diff"), encoding: .utf8)
        return shared + """
        diff --git a/Package.resolved b/Package.resolved
        index 5a5a5a5..6b6b6b6 100644
        --- a/Package.resolved
        +++ b/Package.resolved
        @@ -12,3 +12,3 @@
               "revision" : "1f2e3d4c",
        -      "version" : "1.4.0"
        +      "version" : "1.5.0"

        """
    }
}
