import AlondraDomain
import Foundation
import Testing

struct PRReviewUseCaseTests {
    @Test
    func staleLineAnchorCannotSendPartialReview() {
        let files = UnifiedDiff.parse("""
        diff --git a/a.swift b/a.swift
        --- a/a.swift
        +++ b/a.swift
        @@ -1 +1 @@
        -old
        +new
        """)
        let result = PreparePRReviewUseCase().execute(
            comments: ["a.swift:99": "Old comment", "a.swift:1": "Current comment"],
            note: "", attachments: [], files: files, isOpen: true, commentsAreCurrent: true
        )
        #expect(result == .outdatedDiff)
    }

    @Test
    func attachmentsWithoutTextAreRejectedBeforeTransport() {
        let attachment = PRReviewAttachment(id: UUID(), name: "log.txt", data: Data("log".utf8))
        let decision = PreparePRReviewUseCase().execute(
            comments: [:], note: "  ", attachments: [attachment], files: nil,
            isOpen: true, commentsAreCurrent: true
        )
        #expect(decision == .textRequired)
    }

}
