import AlondraDomain
import Foundation
import Testing

@Suite struct UnifiedDiffTests {
    private let diff = """
    diff --git a/src/a.rs b/src/a.rs
    index 1..2 100644
    --- a/src/a.rs
    +++ b/src/a.rs
    @@ -10,3 +10,4 @@ fn main() {
     let a = 1;
    -let b = 2;
    +let b = 3;
    +let c = 4;
     }
    diff --git a/new.md b/new.md
    new file mode 100644
    --- /dev/null
    +++ b/new.md
    @@ -0,0 +1 @@
    +hello
    \\ No newline at end of file
    """

    @Test func linesKeepTheirOldAndNewNumbers() {
        let files = UnifiedDiff.parse(diff)
        #expect(files.map(\.path) == ["src/a.rs", "new.md"])
        let a = files[0]
        #expect(a.additions == 2 && a.deletions == 1)
        let kinds = a.lines.map(\.kind)
        #expect(kinds == [.hunk, .context, .removed, .added, .added, .context])
        #expect(a.lines[2].old == 11 && a.lines[2].anchor == "src/a.rs:11")
        #expect(a.lines[4].new == 12 && a.lines[4].anchor == "src/a.rs:12")
        #expect(a.lines[5].old == 12 && a.lines[5].new == 13)
        #expect(files[1].lines.last?.kind == .note)
        #expect(files[1].lines[1].anchor == "new.md:1")
    }

    @Test func aReviewReadsLikeTuicrs() {
        let text = ReviewExport.text(comments: [("src/a.rs:12", "Name this constant")], general: "Otherwise fine.")
        #expect(text == "I reviewed your code and have the following comments. Please address them.\n\n1. `src/a.rs:12` - Name this constant\n\nOtherwise fine.")
        #expect(ReviewExport.text(comments: [], general: "  ") == "")
    }
}
