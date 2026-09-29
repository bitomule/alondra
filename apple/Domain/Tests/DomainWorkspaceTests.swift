import AlondraContracts
import AlondraDomain
import Foundation
import XCTest

enum DomainFixtures {
    static func all() throws -> [String: DomainV2.DomainProjection] {
        let env = ProcessInfo.processInfo.environment
        let url: URL
        if let runfiles = env["TEST_SRCDIR"] {
            url = URL(fileURLWithPath: runfiles).appendingPathComponent(env["TEST_WORKSPACE"] ?? "_main")
                .appendingPathComponent("apple/APIClient/Tests/domain-v2-wire-fixtures.json")
        } else {
            url = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
                .deletingLastPathComponent().appendingPathComponent("APIClient/Tests/domain-v2-wire-fixtures.json")
        }
        return try JSONDecoder().decode([String: DomainV2.DomainProjection].self, from: Data(contentsOf: url))
    }

    static func workspace(_ name: String) throws -> DomainWorkspace {
        DomainWorkspace(try XCTUnwrap(all()[name]))
    }
}

final class DomainWorkspaceTests: XCTestCase {
    func testChildrenKeepTheirOwnIDsAndResolveToTheServerRoot() throws {
        let workspace = try DomainFixtures.workspace("pr_question")
        XCTAssertEqual(workspace.roots.map(\.id), ["pr:pr"])
        XCTAssertEqual(workspace.root(containing: "interaction:q")?.id, "pr:pr")
        XCTAssertEqual(workspace.root(containing: "interaction:d")?.id, "pr:pr")
        XCTAssertEqual(workspace.root(containing: "pr:pr")?.id, "pr:pr")
        XCTAssertEqual(workspace.item(for: "q")?.id, "interaction:q")
        XCTAssertEqual(workspace.item(for: "q")?.kind, "question")
        XCTAssertNil(workspace.root(containing: "interaction:missing"))
    }

    func testTaskDetailFollowsExplicitRelationsOnly() throws {
        let workspace = try DomainFixtures.workspace("pr_question")
        let detail = try XCTUnwrap(DomainTaskDetail("t", in: workspace))
        XCTAssertEqual(detail.pullRequest?.id, "pr")
        XCTAssertEqual(detail.agent?.id, "a")
        XCTAssertEqual(detail.request?.id, "r")
        XCTAssertEqual(Set(detail.pending.map(\.id)), ["pr:pr", "interaction:q", "interaction:d"])
        XCTAssertEqual(detail.finalPresentation?.id, "d")
        XCTAssertEqual(detail.requestTasks.map(\.id), ["t"])
        XCTAssertNil(DomainTaskDetail("unknown", in: workspace))
    }

    func testAnsweredItemsStayInTheProjectionButAreNotPending() throws {
        let workspace = try DomainFixtures.workspace("accepted")
        let question = try XCTUnwrap(workspace.item(for: "q"))
        XCTAssertFalse(question.pending)
        let detail = try XCTUnwrap(DomainTaskDetail("t", in: workspace))
        XCTAssertFalse(detail.pending.contains { $0.id == question.id })
    }
}

final class DomainCommandFactoryTests: XCTestCase {
    private let factory = DomainCommandFactory()

    private func envelope(_ intent: DomainIntent, _ fixture: String, taskActions: [DomainV2.AvailableAction] = []) throws -> DomainV2.CommandEnvelope {
        try factory.envelope(for: intent, in: DomainFixtures.workspace(fixture), taskActions: taskActions,
                             commandID: "command-1", newID: { "new-id" })
    }

    func testMarkReadUsesTheServerRevisionAndEpochAndNothingElse() throws {
        let result = try envelope(.markRead("info"), "information")
        XCTAssertEqual(result.dataEpoch, "epoch")
        XCTAssertEqual(result.expectedRevision, 4)
        XCTAssertEqual(result.commandId, "command-1")
        XCTAssertEqual(result.command, .markRead(interactionId: "info"))
    }

    func testWithdrawingARequestSaysTheUserWithdrewIt() throws {
        XCTAssertEqual(try envelope(.closeRequest("r"), "information").command, .closeRequest(requestId: "r", withdrawn: true))
    }

    func testAnswerRequiresTheOptionsDeclaredInputNotItsLabel() throws {
        XCTAssertThrowsError(try envelope(.answer("q", optionID: "proceed", inputs: [:]), "question")) {
            XCTAssertEqual($0 as? DomainIntentError, .inputRequired("explanation"))
        }
        XCTAssertThrowsError(try envelope(.answer("q", optionID: "proceed", inputs: ["explanation": .text(value: "  ")]), "question")) {
            XCTAssertEqual($0 as? DomainIntentError, .inputRequired("explanation"))
        }
        XCTAssertThrowsError(try envelope(.answer("q", optionID: "Continuar", inputs: ["explanation": .text(value: "x")]), "question")) {
            XCTAssertEqual($0 as? DomainIntentError, .optionRequired, "A label is not an option ID")
        }
        XCTAssertThrowsError(try envelope(.answer("q", optionID: "proceed", inputs: ["explanation": .boolean(value: true)]), "question")) {
            XCTAssertEqual($0 as? DomainIntentError, .inputInvalid("explanation"))
        }
        XCTAssertThrowsError(try envelope(.answer("q", optionID: "proceed", inputs: ["explanation": .text(value: "x"), "extra": .text(value: "y")]), "question")) {
            XCTAssertEqual($0 as? DomainIntentError, .inputInvalid("extra"))
        }
        let result = try envelope(.answer("q", optionID: "proceed", inputs: ["explanation": .text(value: " Reviewed ")]), "question")
        XCTAssertEqual(result.expectedRevision, 5)
        XCTAssertEqual(result.command, .answerQuestion(attachments: [], interactionId: "q", optionId: "proceed",
                                                       inputs: ["explanation": .text(value: "Reviewed")]))
    }

    func testDisabledActionsAreRefusedWithTheServerReason() throws {
        XCTAssertThrowsError(try envelope(.answer("q", optionID: "proceed", inputs: ["explanation": .text(value: "x")]), "completed")) {
            XCTAssertEqual($0 as? DomainIntentError, .unavailable(reason: "task_not_active"))
        }
        XCTAssertThrowsError(try envelope(.acceptDelivery("d"), "pr_question")) {
            XCTAssertEqual($0 as? DomainIntentError, .unavailable(reason: "acceptance_missing"))
        }
        XCTAssertThrowsError(try envelope(.markRead("q"), "question")) {
            XCTAssertEqual($0 as? DomainIntentError, .unavailable(reason: "invalid_action_for_kind"),
                           "Reading a question is not an answer")
        }
    }

    func testAcceptanceAndFeedbackReferToThePresentedTargets() throws {
        let accepted = try envelope(.acceptDelivery("d"), "accepted")
        XCTAssertEqual(accepted.command, .acceptDelivery(presentationId: "d", targets: [.text(text: "The result")]))
        XCTAssertThrowsError(try envelope(.submitFeedback("d", body: " \n "), "accepted")) {
            XCTAssertEqual($0 as? DomainIntentError, .feedbackRequired)
        }
        let feedback = try envelope(.submitFeedback("d", body: " Change the title "), "pr_question")
        XCTAssertEqual(feedback.command, .submitFeedback(attachments: [], presentationId: "d",
            targets: [.pullRequest(prId: "pr", head: "head1")], body: "Change the title"))
    }

    func testMergeAuthorizationUsesTheServerHeadAndAnAllowedMethod() throws {
        let result = try envelope(.authorizeMerge("d", method: .squash), "pr_question")
        XCTAssertEqual(result.command, .authorizeMerge(authorizationId: "new-id", presentationId: "d", prId: "pr",
                                                       expectedHead: "head1", method: .squash))
        XCTAssertThrowsError(try envelope(.authorizeMerge("d", method: .merge), "pr_question")) {
            XCTAssertEqual($0 as? DomainIntentError, .methodNotAllowed)
        }
    }

    func testTaskActionsComeFromTheServerTaskActions() throws {
        XCTAssertThrowsError(try envelope(.cancelTask("t"), "question")) {
            XCTAssertEqual($0 as? DomainIntentError, .unavailable(reason: nil))
        }
        let actions = [DomainV2.AvailableAction(kind: "cancel_task", enabled: true, disabledReason: nil,
                                                expectedRevision: 3, presentationId: nil, expectedHead: nil),
                       DomainV2.AvailableAction(kind: "reopen_task", enabled: false, disabledReason: "task_not_active",
                                                expectedRevision: 3, presentationId: nil, expectedHead: nil)]
        XCTAssertEqual(try envelope(.cancelTask("t"), "question", taskActions: actions).expectedRevision, 3)
        XCTAssertThrowsError(try envelope(.reopenTask("t"), "question", taskActions: actions)) {
            XCTAssertEqual($0 as? DomainIntentError, .unavailable(reason: "task_not_active"))
        }
    }

    func testRequestsAreCreatedWithAFreshIDAndTrimmedBody() throws {
        let result = try envelope(.createRequest(body: "  Build it "), "information")
        XCTAssertEqual(result.command, .createRequest(requestId: "new-id", body: "Build it"))
        XCTAssertThrowsError(try envelope(.createRequest(body: " "), "information")) {
            XCTAssertEqual($0 as? DomainIntentError, .requestRequired)
        }
    }
}
