import AlondraContracts
import AlondraNotificationActions
import AlondraNotificationPayload
import Foundation
import XCTest

final class NotificationActionTests: XCTestCase {
    func testV2PayloadRoutesOnlyValidActionsByOptionID() throws {
        let questionPush = try payload(#"{"alondra":{"protocol_version":2,"kind":"question","item_id":"interaction:q-1","interaction_id":"q-1","data_epoch":"e","expected_revision":7,"options":[{"id":"approve","label":"Approve"},{"id":"changes","label":"Request changes","text_input":"why"}],"reply_option_id":"changes"}}"#)
        let infoPush = try payload(#"{"alondra":{"protocol_version":2,"kind":"information","item_id":"interaction:i-1","interaction_id":"i-1","data_epoch":"e","expected_revision":7}}"#)
        let question = try XCTUnwrap(NotificationPayload.Item(userInfo: questionPush))
        let options = NotificationPayload.Question(userInfo: questionPush)
        let info = try XCTUnwrap(NotificationPayload.Item(userInfo: infoPush))

        XCTAssertEqual(NotificationAction(item: question, question: options, action: "OPT:approve", text: nil),
                       .answer(interaction: "q-1", option: "approve", text: nil))
        XCTAssertNil(NotificationAction(item: question, question: options, action: "OPT:changes", text: nil),
                     "An option that takes text is answered by typing a reply")
        XCTAssertEqual(NotificationAction(item: question, question: options, action: "REPLY", text: "  Need logs  "),
                       .answer(interaction: "q-1", option: "changes", text: "Need logs"))
        XCTAssertEqual(NotificationAction(item: question, question: options, action: "MORE_CONTEXT", text: nil), .moreContext(interaction: "q-1"))
        XCTAssertEqual(NotificationAction(item: info, question: nil, action: "READ", text: nil), .markRead(interaction: "i-1"))
        XCTAssertNil(NotificationAction(item: info, question: nil, action: "ALLOW", text: nil))
        XCTAssertNil(NotificationAction(item: question, question: options, action: "READ", text: nil))
        XCTAssertNil(NotificationAction(item: question, question: options, action: "REPLY", text: "  "))
        XCTAssertNil(NotificationAction(item: question, question: options, action: "OPT:unknown", text: nil))
    }

    func testTheCommandComesFromTheItemAsTheMacServesItNow() throws {
        let action = NotificationAction.answer(interaction: "q-1", option: "changes", text: "Need logs")

        let envelope = try action.envelope(fresh: lookup(revision: 12), interaction: interaction(), commandID: "fresh-1")

        XCTAssertEqual(envelope.commandId, "fresh-1")
        XCTAssertEqual(envelope.dataEpoch, "epoch-1")
        XCTAssertEqual(envelope.expectedRevision, 12)
        XCTAssertEqual(envelope.command, .answerQuestion(attachments: [], interactionId: "q-1", optionId: "changes",
                                                         inputs: ["why": .text(value: "Need logs")]))
    }

    func testAChoiceThatNoLongerAppliesIsNoLongerPending() throws {
        let answer = NotificationAction.answer(interaction: "q-1", option: "approve", text: nil)
        XCTAssertThrowsError(try answer.envelope(fresh: lookup(revision: 12, pending: false), interaction: interaction(), commandID: "c")) {
            XCTAssertEqual($0 as? NotificationActionError, .noLongerPending)
        }
        XCTAssertThrowsError(try answer.envelope(fresh: lookup(revision: 12, enabled: false), interaction: interaction(), commandID: "c")) {
            XCTAssertEqual($0 as? NotificationActionError, .noLongerPending)
        }
        let gone = NotificationAction.answer(interaction: "q-1", option: "removed", text: nil)
        XCTAssertThrowsError(try gone.envelope(fresh: lookup(revision: 12), interaction: interaction(), commandID: "c")) {
            XCTAssertEqual($0 as? NotificationActionError, .noLongerPending)
        }
        let other = NotificationAction.markRead(interaction: "i-9")
        XCTAssertThrowsError(try other.envelope(fresh: lookup(revision: 12), interaction: interaction(), commandID: "c")) {
            XCTAssertEqual($0 as? NotificationActionError, .noLongerPending)
        }
    }

    private func lookup(revision: Int, pending: Bool = true, enabled: Bool = true) throws -> DomainV2.InboxLookup {
        try DomainV2.decode(DomainV2.InboxLookup.self, from: Data(#"""
        {"protocol_version":2,"data_epoch":"epoch-1","revision":\#(revision),"root_id":"interaction:q-1","item":{"id":"interaction:q-1","kind":"question","subject":{"kind":"interaction","interaction_id":"q-1"},"title":"Deploy?","revision":\#(revision),"order":3,"pending":\#(pending),"children":[],"available_actions":[{"kind":"answer_question","enabled":\#(enabled),"expected_revision":\#(revision)}]}}
        """#.utf8))
    }

    private func interaction() throws -> DomainV2.Interaction {
        try DomainV2.decode(DomainV2.Interaction.self, from: Data(#"""
        {"attachments":[],"id":"q-1","scope":{"kind":"task","task_id":"t-1"},"author":{"kind":"agent","agent_id":"a-1"},"recipient":{"kind":"user"},"body":"Deploy?","blocking":true,"payload":{"kind":"question","options":[{"id":"approve","label":"Approve","inputs":[]},{"id":"changes","label":"Request changes","inputs":[{"id":"why","kind":"text","required":true}]}]},"revision":12,"order":3}
        """#.utf8))
    }

    private func payload(_ json: String) throws -> [AnyHashable: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [AnyHashable: Any])
    }
}
