import AlondraDomain
import Foundation
import Testing

@Suite struct DiffFoldingTests {
    private func file(_ path: String, added: Int = 1, removed: Int = 0) -> DiffFile {
        let header = "@@ -1,\(removed) +1,\(added) @@"
        let body = Array(repeating: "-old", count: removed) + Array(repeating: "+new", count: added)
        let text = (["diff --git a/\(path) b/\(path)", "--- a/\(path)", "+++ b/\(path)", header] + body).joined(separator: "\n")
        return UnifiedDiff.parse(text)[0]
    }

    @Test(arguments: [
        "Cargo.lock", "apple/Package.resolved", "MODULE.bazel.lock",
        "apple/Shared/Resources/AlondraShared.xcstrings",
        "apple/Tests/__Snapshots__/CardTests/testCard.1.png",
    ])
    func generatedFilesStartCollapsed(path: String) {
        #expect(file(path).startsCollapsed)
    }

    @Test func sourceFilesStartExpandedUnlessHuge() {
        #expect(!file("apple/iOS/Sources/PRReviewView.swift", added: 300, removed: 100).startsCollapsed)
        #expect(file("apple/iOS/Sources/PRReviewView.swift", added: 300, removed: 101).startsCollapsed)
        #expect(!file("docs/Cargo.lock.md").startsCollapsed, "only the exact lockfile name is generated")
    }

    @Test func theHeaderCountsCommentsOnItsOwnLinesOnly() {
        let diff = UnifiedDiff.parse("""
        diff --git a/a.swift b/a.swift
        --- a/a.swift
        +++ b/a.swift
        @@ -1,2 +1,2 @@
        -let a = 1
        +let a = 2
         let b = 3
        diff --git a/b.swift b/b.swift
        --- a/b.swift
        +++ b/b.swift
        @@ -1 +1 @@
        -x
        +y
        """)
        let comments = ["a.swift:1": "why?", "a.swift:2": "ok", "b.swift:9": "stale"]
        #expect(diff[0].commentCount(in: comments) == 2)
        #expect(diff[1].commentCount(in: comments) == 0)
    }
}
