import AlondraDesign
import Foundation
import Testing

@Suite struct ArtifactTests {
    @Test func findsDistinctClaudeArtifactsInTextOrderAndRejectsOtherHosts() {
        let text = "See https://claude.ai/artifact/first and [code](https://claude.ai/code/artifact/second). "
            + "Again https://claude.ai/artifact/first; ignore https://notclaude.ai/artifact/wrong."
        #expect(Artifacts.urls(in: text).map(\.absoluteString) == [
            "https://claude.ai/artifact/first", "https://claude.ai/code/artifact/second",
        ])
    }
}
