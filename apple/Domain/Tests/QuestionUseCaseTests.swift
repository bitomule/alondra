import AlondraContracts
import AlondraDomain
import Foundation
import Testing

struct QuestionUseCaseTests {
    private func question(_ id: Int = 1, origin: String = "permission", options: String = "[]", toolUseID: String? = nil) throws -> Question {
        let marker = toolUseID.map { #", "toolUseId":"\#($0)""# } ?? ""
        let json = #"{"id":\#(id),"sessionId":"s","title":"Can I proceed?","options":\#(options),"links":[],"attachments":[],"origin":"\#(origin)","status":"open","askedAt":1\#(marker)}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Question.self, from: Data(json.utf8))
    }

    private func report(_ id: Int = 7, links: String = "[]", attachments: String = "[]", body: String = "Text") throws -> Report {
        let json = #"{"id":\#(id),"sessionId":"s","title":"Document","body":"\#(body)","links":\#(links),"attachments":\#(attachments),"kind":"document","createdAt":1}"#
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Report.self, from: Data(json.utf8))
    }

    @Test
    func reportFeedbackRequiresContentAndFormatsAnchoredComments() throws {
        let report = try report()
        let useCase = ReviewDocumentUseCase()
        #expect(throws: QuestionWorkError.noFeedback) {
            try useCase.feedback(for: report, comments: [], general: "  ", author: "David", attachmentIDs: [])
        }
        let comment = ReviewComment(anchor: "p1", quote: "The design is unsafe", text: "Please revise")
        let request = try useCase.feedback(for: report, comments: [comment], general: "", author: "David", attachmentIDs: ["stored-1"])
        #expect(request.reportID == 7 && request.sessionID == "s")
        #expect(request.text.contains("Please revise"))
        #expect(request.attachmentIDs == ["stored-1"])
    }

    @Test
    func planChangesNeedFeedbackButApprovalCanUseProtocolOption() throws {
        let plan = try question(origin: "plan")
        var draft = QuestionDraft()
        draft.selectedOption = AnswerOption.requestChanges
        #expect(throws: QuestionWorkError.explanationRequired) {
            try AnswerQuestionUseCase().answer(plan, draft: draft)
        }
        draft.text = "Adjust the rollout order"
        #expect(try AnswerQuestionUseCase().answer(plan, draft: draft) ==
                .answer(id: 1, text: "Adjust the rollout order", option: AnswerOption.requestChanges, attachments: []))
        draft.selectedOption = AnswerOption.approve
        draft.text = ""
        #expect(try AnswerQuestionUseCase().answer(plan, draft: draft) ==
                .answer(id: 1, text: AnswerOption.approve, option: AnswerOption.approve, attachments: []))
    }

    @Test
    func terminalMenuRequiresExactOptionAndRejectsAttachmentsAndContext() throws {
        let menu = try question(options: #"[{"label":"Keep","description":null},{"label":"Close","description":null}]"#,
                                toolUseID: "menu:node-a")
        #expect(menu.isTerminalMenu)
        var draft = QuestionDraft()
        draft.text = "Close"
        #expect(throws: QuestionWorkError.menuRequiresOption) {
            try AnswerQuestionUseCase().answer(menu, draft: draft)
        }
        draft.selectedOption = "Close"
        draft.attachmentIDs = ["file-id"]
        #expect(throws: QuestionWorkError.menuDoesNotAcceptAttachments) {
            try AnswerQuestionUseCase().answer(menu, draft: draft)
        }
        draft.attachmentIDs = []
        #expect(try AnswerQuestionUseCase().answer(menu, draft: draft) ==
                .answer(id: 1, text: "Close", option: "Close", attachments: []))
        #expect(throws: QuestionWorkError.menuRequiresOption) {
            try AnswerQuestionUseCase().checkedMoreContext(menu, draft: draft)
        }
    }

}
