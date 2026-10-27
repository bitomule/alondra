import AlondraAttachments
import AlondraContract
import AlondraFeatures
import AlondraFeed
import ComposableArchitecture
import Foundation
import XCTest

@MainActor
final class ModelIntentDeliveryTests: XCTestCase {
    private let fields: [String: JSONValue] = ["text": .string("Keep my exact model"), "model": .string("codex/exact"), "reasoning_effort": .string("high"), "allow_model_substitution": .bool(false), "substitution_was_edited": .bool(true)]

    func testALegacyFormCannotSealAnUnsentExplicitModelIntentAfterReconnect() async throws {
        let document = try Fixtures.document("new-request-ios")
        var state = NewRequestFeature.State()
        state.screen.document = document
        state.outbox.append(OutgoingRequest.State(id: "draft", fields: fields, attachments: AttachmentFeature.State()))
        state.pending.add("draft", text: "Keep my exact model", phase: .failed("Offline"))
        let sent = LockIsolated<[ActionRequest]>([])
        let store = TestStore(initialState: state) { NewRequestFeature() } withDependencies: {
            $0.viewsClient.prepare = { request in sent.withValue { $0.append(request) }; return request }
        }
        await store.send(.retry("draft")) { $0.pending.set("draft", .failed(String(localized: "Unavailable in the current catalog"))) }
        XCTAssertTrue(sent.value.isEmpty)
        XCTAssertEqual(store.state.outbox[id: "draft"]?.fields, fields)
        XCTAssertNil(store.state.outbox[id: "draft"]?.submission)
    }

    func testAnAttemptedIntentKeepsItsOriginalActionAndPayloadAcrossALegacyFormRefresh() async throws {
        let document = try Fixtures.document("new-request-ios")
        var state = NewRequestFeature.State()
        state.screen.document = document
        let original = ActionRequest(commandId: "original", dataEpoch: document.dataEpoch, client: .ios, lang: "en", actionId: "new_request_selection:view@7", input: ActionInput(fields: fields), resolvesCard: false)
        var outgoing = OutgoingRequest.State(id: "original", fields: fields, attachments: AttachmentFeature.State())
        outgoing.submission = original
        state.outbox.append(outgoing)
        state.pending.add("original", text: "Keep my exact model", phase: .failed("Unconfirmed"))
        let sent = LockIsolated<[ActionRequest]>([])
        let clock = TestClock<Duration>()
        let store = TestStore(initialState: state) { NewRequestFeature() } withDependencies: {
            $0.continuousClock = clock
            $0.viewsClient.prepare = { $0 }
            $0.viewsClient.perform = { request in sent.withValue { $0.append(request) }; return try Responses.applied("Request sent") }
        }
        await store.send(.retry("original")) { $0.pending.set("original", .sending) }
        await store.receive(\.answered) {
            $0.pending.set("original", .applied(shown: document.version))
            $0.outbox.remove(id: "original")
        }
        XCTAssertEqual(sent.value, [original], "reconciliation cannot silently replace a modern intent with the legacy form's action or fields")
    }
}
