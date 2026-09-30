import AlondraNotificationActions
import AlondraNotificationPayload
import AlondraViews
import AlondraViewsData
import Foundation
import XCTest

final class NotificationActionSubmissionTests: XCTestCase {
    func testTextButtonPostsOpaqueActionOptionAndTextThroughViewsSender() async throws {
        let userInfo = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(#"{"alondra":{"protocol":1,"kind":"question","card":"q-1","data_epoch":"epoch-1","actions":[{"id":"ans:q-1@117","option":"approve","label":"Approve"},{"id":"ans:q-1@117","option":"other","label":"Other","text":true,"placeholder":"Tell it what to do"}]}}"#.utf8)) as? [AnyHashable: Any])
        let item = try XCTUnwrap(NotificationPayload.Item(userInfo: userInfo))
        let action = try XCTUnwrap(NotificationAction(item: item, action: "A1", text: "  Use Safari  ", lang: "en"))
        let recorder = ActionRecorder()
        let sender = ViewActionSender(transport: recorder)
        let commandID = "b87e9f09-fc76-4a6b-80ab-1a621610568f"

        let response = try await sender.send(action.request(for: item, commandID: commandID))

        XCTAssertEqual(response.outcome, .noLongerAvailable)
        let recorded = await recorder.call()
        let (request, body) = try XCTUnwrap(recorded)
        XCTAssertEqual(request.actionId, "ans:q-1@117")
        XCTAssertEqual(request.optionId, "other")
        XCTAssertEqual(request.input.text, "Use Safari")
        XCTAssertEqual(request.commandId, commandID)
        let encoded = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertEqual(encoded["action_id"] as? String, "ans:q-1@117")
        XCTAssertEqual(encoded["option_id"] as? String, "other")
        XCTAssertEqual((encoded["input"] as? [String: Any])?["text"] as? String, "Use Safari")
    }
}

private actor ActionRecorder: ViewsTransport {
    private var submitted: (ActionRequest, Data)?

    func fetchView(key: String, client: ViewClient, lang: String, known: String?) async throws -> ViewFetch {
        throw Failure.unexpectedRead
    }

    func fetchChanges(client: ViewClient, lang: String, wait: UInt, cursor: String?, views: [OpenViewVersion]) async throws -> ViewFeed {
        throw Failure.unexpectedRead
    }

    func submitAction(_ request: ActionRequest, body: Data) async throws -> ActionResponse {
        submitted = (request, body)
        return try ViewsCodec.decode(ActionResponse.self, from: Data(#"{"protocol":1,"outcome":"no_longer_available"}"#.utf8))
    }

    func call() -> (ActionRequest, Data)? { submitted }

    private enum Failure: Error { case unexpectedRead }
}
